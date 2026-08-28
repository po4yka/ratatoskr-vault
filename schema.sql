-- The Vault database, in one file.
--
-- `ratatoskr-vault` applies this at startup, to a fresh database. There is no migration ledger and
-- no incremental history: no database holds data that has to survive a schema change (DEVELOPMENT
-- status). A schema change edits this file in place; the next fresh database has it.
--
-- One schema: `git_vault`. `docs/ARCHITECTURE.md` section 4 names its tables and no others. Vault
-- writes only this schema and reads none beside it; references to Catalog identity are unenforced
-- columns, never cross-schema foreign keys.
--
-- Conventions, applied uniformly and stated once here:
--
--   * Identifiers are UUIDv7 minted by the application, never by the database. There is
--     deliberately no DEFAULT on any id column, so a missing id is an insert error rather than a
--     silently wrong version.
--
--   * Closed vocabularies are `text` with a CHECK, not a PostgreSQL enum. Adding a value to a PG
--     enum cannot run inside the same transaction that uses it, and removing one is a table
--     rewrite; a CHECK constraint is altered by one statement. The target and snapshot state
--     machines below are the ones AGENTS.md names; a state outside them is refusable by the
--     database today, before any worker exists.
--
--   * A bounded `text` with a CHECK rather than `varchar(n)`: the bound is stated where every
--     other rule about the column is stated.
--
--   * Every timestamp is `timestamptz`. `timestamp` would silently record the server's local time.
--
--   * A hash is stored as raw bytes in `bytea`, the column is named `*_hash`, and a SHA-256 column
--     is CHECK-constrained to exactly 32 bytes. There is no column anywhere in this file that can
--     hold a credential in readable form (SECURITY.md); clone URLs carry no user information and
--     credentials are injected at execution time, never stored here.
--
--   * Counts and sizes are `bigint` and constrained non-negative where a negative value could only
--     be corruption or a bug.

create schema git_vault;

comment on schema git_vault is
    'Vault-owned preservation state: backup targets, desired-state revisions, mirrors, sync runs, '
    'snapshots, artifacts, manifests, integrity checks, restore drills, retention, tombstones, '
    'storage locations, collectors, and the outbox/inbox machinery.';

-- ---------------------------------------------------------------------------------------------
-- targets: one preserved repository, its identity, and its actual state machine
-- ---------------------------------------------------------------------------------------------

create table git_vault.targets (
    target_id             uuid        primary key,
    provider              text        not null,
    external_repository_id text       not null,
    target_kind           text        not null default 'repository',
    parent_target_id      uuid        references git_vault.targets (target_id),
    status                text        not null,
    pinned                boolean     not null default false,
    created_at            timestamptz not null,
    updated_at            timestamptz not null,

    constraint targets_provider_is_known
        check (provider in ('github')),
    constraint targets_external_repository_id_is_bounded
        check (length(external_repository_id) between 1 and 255),
    constraint targets_kind_is_known
        check (target_kind in ('repository', 'wiki')),
    constraint targets_parent_shape_is_consistent
        check (
            (target_kind = 'repository' and parent_target_id is null)
            or (target_kind = 'wiki' and parent_target_id is not null and parent_target_id <> target_id)
        ),
    -- The target state machine, AGENTS.md "Target and snapshot state machines".
    constraint targets_status_is_known
        check (status in (
            'requested', 'cloning', 'ready', 'fetching', 'snapshotting', 'verifying',
            'healthy', 'degraded', 'paused', 'excluded', 'deleting', 'deleted'
        )),
    constraint targets_updated_at_is_not_before_created_at
        check (updated_at >= created_at)
);

comment on table git_vault.targets is
    'One repository Vault preserves. Catalog decides that it SHOULD be preserved; this table holds '
    'what actually is. Repository identity is stored as provider plus opaque external id, never as '
    'an owner/name pair that could define a filesystem path.';
comment on column git_vault.targets.pinned is
    'A pinned target is never deleted automatically. Unpinning is an explicit operator action '
    '(docs/ARCHITECTURE.md section 5.2).';
comment on column git_vault.targets.status is
    'requested | cloning | ready | fetching | snapshotting | verifying | healthy | degraded | '
    'paused | excluded | deleting | deleted';

-- One provider repository is at most one target, so reconciliation of duplicate events converges.
create unique index targets_repository_provider_external_id_key
    on git_vault.targets (provider, external_repository_id)
    where target_kind = 'repository';
create unique index targets_one_wiki_per_parent_key
    on git_vault.targets (parent_target_id)
    where target_kind = 'wiki';

-- ---------------------------------------------------------------------------------------------
-- The target transition guard
-- ---------------------------------------------------------------------------------------------

-- Canonical order of the eleven target states (AGENTS.md "Target and snapshot state machines").
-- Rank orders the vocabulary for deterministic reporting; it does NOT decide legality: the
-- machine is a graph, not a ladder (excluded -> requested is legal while healthy -> fetching is
-- not), so legality lives in the pair set the trigger below checks.
create function git_vault.target_status_rank(status text) returns int
    language sql immutable strict parallel safe
as $$
    select case status
        when 'requested'    then 0
        when 'cloning'      then 1
        when 'ready'        then 2
        when 'fetching'     then 3
        when 'snapshotting' then 4
        when 'verifying'    then 5
        when 'healthy'      then 6
        when 'degraded'     then 7
        when 'paused'       then 8
        when 'excluded'     then 9
        when 'deleting'     then 10
        when 'deleted'      then 11
    end
$$;

comment on function git_vault.target_status_rank(text) is
    'Position of a target status in the canonical vocabulary order. An ordering helper only; '
    'whether a move itself is legal is decided by target_guard_status_transition.';

create function git_vault.target_guard_status_transition() returns trigger
    language plpgsql
as $$
begin
    if new.status = old.status then
        -- Not a transition. Same-status writes are annotations, always permitted, never events.
        return new;
    end if;

    if old.status = 'excluded' and new.status = 'deleting' and (
        old.pinned
        or not exists (
            select 1
            from git_vault.tombstones tombstone
            where tombstone.target_id = old.target_id
              and tombstone.cancelled_at is null
              and tombstone.completed_at is null
              and clock_timestamp() >= tombstone.not_before
              and exists (
                  select 1 from git_vault.deletion_plans plan
                  where plan.tombstone_id = tombstone.tombstone_id
                    and plan.status not in ('completed', 'cancelled')
              )
              and not exists (
                  select 1
                  from git_vault.deletion_plans plan
                  join git_vault.snapshot_pins pin using (snapshot_id)
                  where plan.tombstone_id = tombstone.tombstone_id
                    and plan.status not in ('completed', 'cancelled')
                    and pin.revoked_at is null
              )
        )
    ) then
        raise exception 'target deletion requires executable unpinned tombstone evidence'
            using errcode = 'VLT05';
    end if;

    if old.status = 'deleting' and new.status = 'deleted' and not exists (
        select 1
        from git_vault.tombstones tombstone
        where tombstone.target_id = old.target_id
          and tombstone.completed_at is not null
          and exists (
              select 1 from git_vault.deletion_plans plan
              where plan.tombstone_id = tombstone.tombstone_id
          )
          and not exists (
              select 1 from git_vault.deletion_plans plan
              where plan.tombstone_id = tombstone.tombstone_id
                and plan.status <> 'completed'
          )
    ) then
        raise exception 'target deletion completion requires terminal plan evidence'
            using errcode = 'VLT06';
    end if;

    -- The single normative pair set, mirrored from ratatoskr_vault_core::Transition::TRANSITIONS.
    -- The agreement walk asserts the two agree on every ordered pair, because two enforcement
    -- points that disagree are worse than one.
    if not exists (
        select 1
        from (values
            ('requested',    'cloning'),
            ('requested',    'excluded'),
            ('cloning',      'ready'),
            ('cloning',      'degraded'),
            ('cloning',      'excluded'),
            ('ready',        'fetching'),
            ('ready',        'degraded'),
            ('ready',        'paused'),
            ('ready',        'excluded'),
            ('fetching',     'snapshotting'),
            ('fetching',     'ready'),
            ('fetching',     'degraded'),
            ('fetching',     'paused'),
            ('fetching',     'excluded'),
            ('snapshotting', 'verifying'),
            ('snapshotting', 'degraded'),
            ('verifying',    'healthy'),
            ('verifying',    'degraded'),
            ('healthy',      'fetching'),
            ('healthy',      'degraded'),
            ('healthy',      'paused'),
            ('healthy',      'excluded'),
            ('degraded',     'fetching'),
            ('degraded',     'cloning'),
            ('degraded',     'paused'),
            ('degraded',     'excluded'),
            ('paused',       'ready'),
            ('paused',       'excluded'),
            ('excluded',     'requested'),
            ('excluded',     'deleting'),
            ('deleting',     'deleted')
        ) as legal(from_status, to_status)
        where legal.from_status = old.status
          and legal.to_status = new.status
    ) then
        raise exception
            'illegal target transition % -> %',
            old.status, new.status
            using errcode = 'VLT01';
    end if;

    return new;
end;
$$;

comment on function git_vault.target_guard_status_transition() is
    'The durable backstop for the target state machine. The authoritative table is '
    'ratatoskr_vault_core::Transition in Rust; this trigger enforces the same rule for any '
    'writer that bypasses it, including a manual UPDATE, and raises SQLSTATE VLT01 on a refusal.';

create trigger targets_guard_status_transition
    before update of status on git_vault.targets
    for each row
    execute function git_vault.target_guard_status_transition();

-- ---------------------------------------------------------------------------------------------
-- desired_state_revisions: every accepted revision of what Catalog wants, append-only
-- ---------------------------------------------------------------------------------------------

create table git_vault.desired_state_revisions (
    revision_id         uuid        primary key,
    target_id           uuid        not null references git_vault.targets (target_id),
    policy_revision     bigint      not null,
    preservation_level  text        not null,
    pinned              boolean     not null default false,
    include_wiki        boolean     not null default false,
    include_releases    boolean     not null default false,
    include_issues      boolean     not null default false,
    offsite_required    boolean     not null default false,
    correlation_id      uuid        not null,
    received_at         timestamptz not null,

    -- The preservation levels the desired-state contract names.
    constraint desired_state_revisions_level_is_known
        check (preservation_level in (
            'none', 'metadata_only', 'git_mirror', 'git_mirror_with_lfs', 'complete_archive'
        )),
    constraint desired_state_revisions_revision_is_positive
        check (policy_revision > 0)
);

comment on table git_vault.desired_state_revisions is
    'Append-only evidence of desired state. Reconciliation reads the LATEST revision and ignores '
    'older duplicates and out-of-order deliveries; keeping history makes that decision auditable.';
comment on column git_vault.desired_state_revisions.correlation_id is
    'The correlation id of the delivery that carried this revision, for tracing a decision back to '
    'the message that caused it.';

-- One revision number is used at most once per target, so a redelivered old event cannot win over
-- a newer one already recorded.
create unique index desired_state_revisions_target_policy_key
    on git_vault.desired_state_revisions (target_id, policy_revision);
create index desired_state_revisions_target_received_idx
    on git_vault.desired_state_revisions (target_id, received_at desc);

-- ---------------------------------------------------------------------------------------------
-- mirrors: the local bare mirror of a target, and its last integrity observation
-- ---------------------------------------------------------------------------------------------

create table git_vault.mirrors (
    mirror_id       uuid        primary key,
    target_id       uuid        not null unique references git_vault.targets (target_id),
    status          text        not null,
    storage_path    text        not null,
    fsck_result     text        not null default 'unknown',
    bytes_on_disk   bigint      not null default 0,
    last_fetch_at   timestamptz,
    created_at      timestamptz not null,
    updated_at      timestamptz not null,

    constraint mirrors_status_is_known
        check (status in ('absent', 'initializing', 'ready', 'degraded', 'quarantined')),
    constraint mirrors_fsck_result_is_known
        check (fsck_result in ('unknown', 'ok', 'failed')),
    constraint mirrors_storage_path_is_bounded_and_relative
        check (storage_path ~ '^mirrors/[0-9a-z]{2}/[0-9a-f-]{36}\.git$'),
    constraint mirrors_bytes_are_non_negative
        check (bytes_on_disk >= 0),
    constraint mirrors_updated_at_is_not_before_created_at
        check (updated_at >= created_at)
);

comment on table git_vault.mirrors is
    'The working bare mirror. Paths are derived from internal ids inside the configured root, '
    'never from owner/repository names (docs/ARCHITECTURE.md section 8.3); the pattern above is '
    'the layout contract, enforced where the row is written rather than trusted from the disk.';
comment on column git_vault.mirrors.fsck_result is
    'The result of the latest full integrity check. A mirror whose fsck has not passed since its '
    'last mutation is not healthy, whatever its fetch status says.';

-- ---------------------------------------------------------------------------------------------
-- sync_runs: one clone-or-update attempt against a mirror, with its lease
-- ---------------------------------------------------------------------------------------------

create table git_vault.sync_runs (
    run_id          uuid        primary key,
    target_id       uuid        not null references git_vault.targets (target_id),
    run_kind        text        not null,
    outcome         text        not null default 'running',
    failure_class   text,
    lease_owner     uuid        not null,
    lease_expires_at timestamptz not null,
    started_at      timestamptz not null,
    finished_at     timestamptz,

    constraint sync_runs_kind_is_known
        check (run_kind in ('clone', 'update')),
    constraint sync_runs_outcome_is_known
        check (outcome in ('running', 'succeeded', 'failed', 'cancelled')),
    constraint sync_runs_failure_class_is_bounded
        check (failure_class is null or length(failure_class) between 1 and 64),
    constraint sync_runs_finished_at_present_once_finished
        check ((outcome = 'running') = (finished_at is null)),
    constraint sync_runs_started_at_is_not_after_lease_expiry
        check (started_at <= lease_expires_at)
);

comment on table git_vault.sync_runs is
    'One attempt to create or update a mirror. The lease serializes conflicting operations on one '
    'target (docs/ARCHITECTURE.md section 20): a second worker must fail to claim it, not queue '
    'behind it, because two writers to one bare object database corrupt it.';
comment on column git_vault.sync_runs.failure_class is
    'A vault failure class code when the run failed; null otherwise. Codes are the closed set in '
    'ratatoskr-vault-core::error::FailureClass.';

create index sync_runs_target_started_idx on git_vault.sync_runs (target_id, started_at desc);
-- An expired lease is reaped by finding running rows whose expiry has passed.
create index sync_runs_outcome_lease_idx on git_vault.sync_runs (outcome, lease_expires_at);

-- ---------------------------------------------------------------------------------------------
-- mirror_lifecycle_runs and mirror_quota_reservations: bounded mirror work evidence
-- ---------------------------------------------------------------------------------------------

create table git_vault.mirror_lifecycle_runs (
    run_id          uuid        primary key,
    target_id       uuid        not null references git_vault.targets (target_id),
    operation       text        not null,
    outcome         text        not null,
    failure_class   text,
    checkpoint      text,
    object_count    bigint,
    bytes_on_disk   bigint,
    created_at      timestamptz not null,

    constraint mirror_lifecycle_runs_operation_is_known
        check (operation in ('clone', 'fetch')),
    constraint mirror_lifecycle_runs_outcome_is_known
        check (outcome in ('succeeded', 'quota_refused', 'interrupted', 'integrity_failed', 'failed')),
    constraint mirror_lifecycle_runs_failure_class_is_bounded
        check (failure_class is null or length(failure_class) between 1 and 64),
    constraint mirror_lifecycle_runs_checkpoint_is_known
        check (checkpoint is null or checkpoint in ('clone_pending', 'fetch_pending')),
    constraint mirror_lifecycle_runs_counts_are_non_negative
        check ((object_count is null or object_count >= 0)
            and (bytes_on_disk is null or bytes_on_disk >= 0))
);

comment on table git_vault.mirror_lifecycle_runs is
    'Immutable terminal evidence for one bounded clone or fetch operation. Interrupted fetches '
    'retain a checkpoint; a later run is a new row and never rewrites this evidence.';

create index mirror_lifecycle_runs_target_created_idx
    on git_vault.mirror_lifecycle_runs (target_id, created_at desc);

create table git_vault.mirror_quota_reservations (
    run_id          uuid        primary key,
    target_id       uuid        not null references git_vault.targets (target_id),
    reserved_bytes  bigint      not null,
    created_at      timestamptz not null,

    constraint mirror_quota_reservations_bytes_are_positive
        check (reserved_bytes > 0)
);

comment on table git_vault.mirror_quota_reservations is
    'Live byte reservations held only while an admitted mirror operation is running. Terminal '
    'evidence releases them explicitly; quota refusal never silently prunes stored mirrors.';

create index mirror_quota_reservations_target_idx
    on git_vault.mirror_quota_reservations (target_id);

-- ---------------------------------------------------------------------------------------------
-- snapshots: immutable preservation points built from mirrors
-- ---------------------------------------------------------------------------------------------

create table git_vault.snapshots (
    snapshot_id     uuid        primary key,
    target_id       uuid        not null references git_vault.targets (target_id),
    mirror_id       uuid        not null references git_vault.mirrors (mirror_id),
    mirror_lifecycle_run_id uuid not null references git_vault.mirror_lifecycle_runs (run_id),
    parent_snapshot_id uuid references git_vault.snapshots (snapshot_id),
    format          text        not null,
    status          text        not null,
    refs_hash       bytea       not null check (length(refs_hash) = 32),
    includes_lfs    boolean     not null default false,
    includes_wiki   boolean     not null default false,
    created_at      timestamptz not null,

    constraint snapshots_format_is_known
        check (format in ('git_bundle')),
    -- The snapshot state machine, AGENTS.md "Snapshot-specific states". Failure states are real
    -- rows, not the absence of success: diagnosis needs them.
    constraint snapshots_status_is_known
        check (status in (
            'building', 'built', 'verifying', 'verified',
            'offsite_uploading', 'offsite_verified',
            'restore_testing', 'restorable',
            'failed', 'quarantined', 'expired', 'deleted'
        ))
);

comment on table git_vault.snapshots is
    'An immutable preservation point. Snapshots are append-only evidence: corrections create a new '
    'snapshot, they do not rewrite this table. The refs_hash pins which mirror observation a '
    'snapshot claims to preserve.';
comment on column git_vault.snapshots.status is
    'building | built | verifying | verified | offsite_uploading | offsite_verified | '
    'restore_testing | restorable | failed | quarantined | expired | deleted';

create index snapshots_target_created_idx on git_vault.snapshots (target_id, created_at desc);

-- ---------------------------------------------------------------------------------------------
-- snapshot_artifacts: the physical bytes of a snapshot, identified by content
-- ---------------------------------------------------------------------------------------------

create table git_vault.snapshot_artifacts (
    artifact_id     uuid        primary key,
    snapshot_id     uuid        not null references git_vault.snapshots (snapshot_id),
    kind            text        not null,
    sha256_hash     bytea       not null check (length(sha256_hash) = 32),
    blob_owner      text        not null check (blob_owner = 'ratatoskr-vault'),
    digest_algorithm text       not null check (digest_algorithm = 'sha256'),
    media_type      text        not null check (length(media_type) between 1 and 255),
    size_bytes      bigint      not null check (size_bytes > 0),
    created_at      timestamptz not null,

    constraint snapshot_artifacts_kind_is_known
        check (kind in ('git_bundle', 'manifest', 'lfs_object'))
);

comment on table git_vault.snapshot_artifacts is
    'Content-addressed bytes. The hash is computed while streaming and verified again after every '
    'transfer; a successful upload response alone never updates anything here.';
create index snapshot_artifacts_snapshot_idx on git_vault.snapshot_artifacts (snapshot_id);

create table git_vault.lfs_snapshot_objects (
    snapshot_id      uuid   not null references git_vault.snapshots (snapshot_id),
    artifact_id      uuid   not null unique references git_vault.snapshot_artifacts (artifact_id),
    oid              text   not null check (oid ~ '^[0-9a-f]{64}$'),
    sha256_hash      bytea  not null check (length(sha256_hash) = 32),
    size_bytes       bigint not null check (size_bytes >= 0),

    constraint lfs_snapshot_objects_key primary key (snapshot_id, oid)
);

comment on table git_vault.lfs_snapshot_objects is
    'Immutable one-to-many evidence linking a signed snapshot to every individually stored and '
    'content-verified Git LFS object it requires.';

-- ---------------------------------------------------------------------------------------------
-- manifests: the immutable evidence document describing a snapshot
-- ---------------------------------------------------------------------------------------------

create table git_vault.manifests (
    manifest_id     uuid        primary key,
    snapshot_id     uuid        not null unique references git_vault.snapshots (snapshot_id),
    schema_version  integer     not null default 1,
    manifest_hash   bytea       not null check (length(manifest_hash) = 32),
    blob_owner      text        not null check (blob_owner = 'ratatoskr-vault'),
    digest_algorithm text       not null check (digest_algorithm = 'sha256'),
    media_type      text        not null check (media_type = 'application/json'),
    size_bytes      bigint      not null check (size_bytes > 0),
    created_at      timestamptz not null,

    constraint manifests_schema_version_is_positive
        check (schema_version >= 1)
);

comment on table git_vault.manifests is
    'One manifest per snapshot. The manifest itself is an artifact stored beside the data it '
    'describes; this row records which bytes are authoritative for which snapshot. Manifests are '
    'append-only evidence: a correction creates a new revision that references its predecessor, '
    'it does not rewrite history (AGENTS.md, Snapshot manifest).';

-- ---------------------------------------------------------------------------------------------
-- integrity_checks: the record of every verification pass over a snapshot
-- ---------------------------------------------------------------------------------------------

create table git_vault.integrity_checks (
    check_id                  uuid        primary key,
    snapshot_id               uuid        not null references git_vault.snapshots (snapshot_id),
    manifest_hash             bytea       not null check (length(manifest_hash) = 32),
    outcome                   text        not null,
    failure_class             text,
    duration_millis           bigint      not null check (duration_millis >= 0),
    stages                    jsonb       not null check (jsonb_typeof(stages) = 'array'),
    checked_artifacts         jsonb       not null check (jsonb_typeof(checked_artifacts) = 'array'),
    expected_ref_count        bigint      not null check (expected_ref_count >= 0),
    expected_refs_hash        bytea       not null check (length(expected_refs_hash) = 32),
    started_at                timestamptz not null,
    finished_at               timestamptz not null,

    constraint integrity_checks_outcome_is_known
        check (outcome in ('passed', 'failed')),
    constraint integrity_checks_terminal_evidence_is_consistent
        check (
            (outcome = 'passed' and failure_class is null)
            or (outcome = 'failed' and failure_class is not null)
        ),
    constraint integrity_checks_times_are_ordered
        check (started_at <= finished_at)
);

comment on table git_vault.integrity_checks is
    'Every verification attempt, passed or failed. Failed checks are kept: an integrity failure '
    'that disappears from the record looks exactly like an integrity failure that never happened.';
create index integrity_checks_snapshot_checked_idx
    on git_vault.integrity_checks (snapshot_id, finished_at desc);

-- ---------------------------------------------------------------------------------------------
-- restore_drills: proof that an artifact can become a repository again
-- ---------------------------------------------------------------------------------------------

create table git_vault.restore_drills (
    drill_id                 uuid        primary key,
    snapshot_id              uuid        not null references git_vault.snapshots (snapshot_id),
    manifest_hash            bytea       not null check (length(manifest_hash) = 32),
    source_kind              text        not null,
    replica_target_id        uuid,
    outcome                  text        not null,
    failure_class            text,
    refs_matched             boolean     not null,
    lfs_restored             boolean,
    expected_lfs_object_count bigint,
    observed_lfs_object_count bigint,
    expected_lfs_bytes       bigint,
    observed_lfs_bytes       bigint,
    expected_lfs_aggregate_hash bytea,
    observed_lfs_aggregate_hash bytea,
    duration_millis          bigint      not null check (duration_millis >= 0),
    stages                   jsonb       not null check (jsonb_typeof(stages) = 'array'),
    expected_ref_count       bigint      not null check (expected_ref_count >= 0),
    observed_ref_count       bigint      not null check (observed_ref_count >= 0),
    expected_refs_hash       bytea       not null check (length(expected_refs_hash) = 32),
    observed_refs_hash       bytea       not null check (length(observed_refs_hash) = 32),
    network_disabled         boolean     not null,
    live_mirror_accessed     boolean     not null,
    started_at               timestamptz not null,
    finished_at              timestamptz not null,

    constraint restore_drills_outcome_is_known
        check (outcome in ('passed', 'failed')),
    constraint restore_drills_source_is_consistent
        check (
            (source_kind = 'local' and replica_target_id is null)
            or (source_kind = 'replica' and replica_target_id is not null)
        ),
    constraint restore_drills_terminal_evidence_is_consistent
        check (
            (outcome = 'passed' and failure_class is null and refs_matched)
            or (outcome = 'failed' and failure_class is not null)
        ),
    constraint restore_drills_isolation_is_proven
        check (network_disabled and not live_mirror_accessed),
    constraint restore_drills_times_are_ordered
        check (started_at <= finished_at),
    constraint restore_drills_lfs_evidence_is_consistent
        check (
            (lfs_restored is null
                and expected_lfs_object_count is null and observed_lfs_object_count is null
                and expected_lfs_bytes is null and observed_lfs_bytes is null
                and expected_lfs_aggregate_hash is null and observed_lfs_aggregate_hash is null)
            or (lfs_restored is not null
                and expected_lfs_object_count >= 0 and observed_lfs_object_count >= 0
                and expected_lfs_bytes >= 0 and observed_lfs_bytes >= 0
                and length(expected_lfs_aggregate_hash) = 32
                and length(observed_lfs_aggregate_hash) = 32)
        )
);

comment on table git_vault.restore_drills is
    'Restore is a product requirement, not an emergency script. A drill reconstructs a repository '
    'from the STORED artifact in an isolated empty destination and records whether refs and, where '
    'policy requires it, LFS objects came back. A snapshot is not called verified without one '
    '(docs/ARCHITECTURE.md section 14).';
comment on column git_vault.restore_drills.lfs_restored is
    'Null when the snapshot carries no LFS objects; true or false otherwise. Whether it must be '
    'non-null is a property of the snapshot the drill ran against, which a CHECK constraint cannot '
    'reach across tables (PostgreSQL forbids subqueries in CHECK); the drill writer sets it from '
    'snapshots.includes_lfs, and restore tests assert that rule.';

create function git_vault.reject_terminal_evidence_mutation()
returns trigger
language plpgsql
as $$
begin
    raise exception 'terminal evidence in %.% is append-only', tg_table_schema, tg_table_name;
end;
$$;

create trigger integrity_checks_are_append_only
    before update or delete on git_vault.integrity_checks
    for each row execute function git_vault.reject_terminal_evidence_mutation();

create trigger restore_drills_are_append_only
    before update or delete on git_vault.restore_drills
    for each row execute function git_vault.reject_terminal_evidence_mutation();

create trigger lfs_snapshot_objects_are_append_only
    before update or delete on git_vault.lfs_snapshot_objects
    for each row execute function git_vault.reject_terminal_evidence_mutation();

-- ---------------------------------------------------------------------------------------------
-- retention_policies: the windows deletion is evaluated against
-- ---------------------------------------------------------------------------------------------

create table git_vault.retention_policies (
    policy_id             uuid        primary key,
    name                  text        not null,
    minimum_age_seconds   bigint      not null,
    grace_seconds         bigint      not null,
    keep_last_restorable  integer     not null,
    created_at            timestamptz not null,

    constraint retention_policies_name_is_bounded
        check (length(name) between 1 and 128),
    constraint retention_policies_minimum_age_is_bounded
        check (minimum_age_seconds between 0 and 315360000),
    constraint retention_policies_grace_is_positive_and_bounded
        check (grace_seconds between 1 and 315360000),
    constraint retention_policies_keep_last_is_positive
        check (keep_last_restorable between 1 and 1000000)
);

comment on table git_vault.retention_policies is
    'Named retention windows. Deletion is evaluated against these plus pin state and tombstones; '
    'an inactive policy alone deletes nothing.';
create unique index retention_policies_name_key on git_vault.retention_policies (name);

-- ---------------------------------------------------------------------------------------------
-- snapshot_pins: source-scoped protection with one-way revocation
-- ---------------------------------------------------------------------------------------------

create table git_vault.snapshot_pins (
    pin_id                     uuid        primary key,
    snapshot_id                uuid        not null references git_vault.snapshots (snapshot_id),
    source                     text        not null,
    source_reference           text        not null,
    correlation_id             uuid        not null,
    pinned_at                  timestamptz not null,
    revoked_at                 timestamptz,
    revocation_correlation_id  uuid,

    constraint snapshot_pins_source_is_known
        check (source in ('operator', 'user')),
    constraint snapshot_pins_reference_is_bounded
        check (length(source_reference) between 1 and 255),
    constraint snapshot_pins_revocation_is_consistent
        check ((revoked_at is null) = (revocation_correlation_id is null)
            and (revoked_at is null or revoked_at >= pinned_at))
);

comment on table git_vault.snapshot_pins is
    'Durable operator/user protection. Revocation closes this row once; neither action erases '
    'history or authorizes deletion before a later retention evaluation and grace window.';
create unique index snapshot_pins_one_active_source_key
    on git_vault.snapshot_pins (snapshot_id, source, source_reference)
    where revoked_at is null;
create index snapshot_pins_snapshot_active_idx
    on git_vault.snapshot_pins (snapshot_id) where revoked_at is null;

create function git_vault.guard_snapshot_pin_mutation()
returns trigger
language plpgsql
as $$
begin
    if tg_op = 'DELETE' then
        raise exception 'snapshot pin history is append-only';
    end if;
    if old.revoked_at is not null
        or new.pin_id <> old.pin_id
        or new.snapshot_id <> old.snapshot_id
        or new.source <> old.source
        or new.source_reference <> old.source_reference
        or new.correlation_id <> old.correlation_id
        or new.pinned_at <> old.pinned_at
        or new.revoked_at is null
        or new.revocation_correlation_id is null then
        raise exception 'snapshot pin identity is immutable and revocation is one-way';
    end if;
    return new;
end;
$$;

create trigger snapshot_pins_guard_update
    before update on git_vault.snapshot_pins
    for each row execute function git_vault.guard_snapshot_pin_mutation();
create trigger snapshot_pins_guard_delete
    before delete on git_vault.snapshot_pins
    for each row execute function git_vault.guard_snapshot_pin_mutation();

-- ---------------------------------------------------------------------------------------------
-- retention_evaluations and candidates: immutable explanation of every policy decision
-- ---------------------------------------------------------------------------------------------

create table git_vault.retention_evaluations (
    evaluation_id   uuid        primary key,
    target_id       uuid        not null references git_vault.targets (target_id),
    policy_id       uuid        not null references git_vault.retention_policies (policy_id),
    mode            text        not null,
    policy_snapshot jsonb       not null check (jsonb_typeof(policy_snapshot) = 'object'),
    required_bytes  bigint,
    outcome         text        not null,
    correlation_id  uuid        not null,
    evaluated_at    timestamptz not null,

    constraint retention_evaluations_mode_is_known
        check (mode in ('scheduled', 'quota_pressure')),
    constraint retention_evaluations_required_bytes_match_mode
        check ((mode = 'scheduled' and required_bytes is null)
            or (mode = 'quota_pressure' and required_bytes > 0)),
    constraint retention_evaluations_outcome_is_known
        check (outcome in ('selected', 'no_candidates', 'allocation_refused'))
);

create index retention_evaluations_target_time_idx
    on git_vault.retention_evaluations (target_id, evaluated_at, evaluation_id);

create table git_vault.retention_candidates (
    evaluation_id       uuid        not null references git_vault.retention_evaluations (evaluation_id),
    snapshot_id         uuid        not null references git_vault.snapshots (snapshot_id),
    ordinal             integer     not null check (ordinal >= 0),
    classification      text        not null,
    pin_sources         jsonb       not null check (jsonb_typeof(pin_sources) = 'array'),
    target_inactive     boolean     not null,
    estimated_bytes     bigint      not null check (estimated_bytes >= 0),
    deletion_not_before timestamptz,

    constraint retention_candidates_classification_is_known
        check (classification in (
            'eligible_ordinary', 'eligible_inactive_target', 'protected_pinned',
            'protected_age_floor', 'protected_keep_last_restorable', 'grace_active'
        )),
    constraint retention_candidates_key primary key (evaluation_id, snapshot_id),
    constraint retention_candidates_ordinal_key unique (evaluation_id, ordinal)
);

create index retention_candidates_snapshot_idx
    on git_vault.retention_candidates (snapshot_id, evaluation_id);

create trigger retention_evaluations_are_append_only
    before update or delete on git_vault.retention_evaluations
    for each row execute function git_vault.reject_terminal_evidence_mutation();
create trigger retention_candidates_are_append_only
    before update or delete on git_vault.retention_candidates
    for each row execute function git_vault.reject_terminal_evidence_mutation();

-- ---------------------------------------------------------------------------------------------
-- tombstones: the audit record that a target was retired, and what still must be kept
-- ---------------------------------------------------------------------------------------------

create table git_vault.tombstones (
    tombstone_id              uuid        primary key,
    target_id                 uuid        not null references git_vault.targets (target_id),
    governing_policy_revision bigint      not null check (governing_policy_revision > 0),
    reason                    text        not null,
    was_pinned                boolean     not null,
    correlation_id            uuid        not null,
    recorded_at               timestamptz not null,
    not_before                timestamptz not null,
    cancelled_at              timestamptz,
    completed_at              timestamptz,

    constraint tombstones_reason_is_known
        check (reason in ('policy_inactive', 'retention_expired', 'operator_request')),
    constraint tombstones_window_is_positive
        check (not_before > recorded_at),
    constraint tombstones_terminal_state_is_consistent
        check (not (cancelled_at is not null and completed_at is not null)
            and (cancelled_at is null or cancelled_at >= recorded_at)
            and (completed_at is null or completed_at >= not_before))
);

comment on table git_vault.tombstones is
    'An unstar is not a deletion. A tombstone records that a target left active service, starts '
    'the grace window, and outlives the data it describes so the deletion stays auditable.';
comment on column git_vault.tombstones.was_pinned is
    'Evidence at recording time. A target that was pinned reached a tombstone only through an '
    'explicit unpin, and the audit trail shows it.';
create unique index tombstones_target_active_key
    on git_vault.tombstones (target_id)
    where cancelled_at is null and completed_at is null;

create function git_vault.guard_tombstone_mutation()
returns trigger
language plpgsql
as $$
begin
    if tg_op = 'DELETE' then
        raise exception 'target tombstone evidence is append-only';
    end if;
    if old.cancelled_at is not null or old.completed_at is not null
        or new.tombstone_id <> old.tombstone_id
        or new.target_id <> old.target_id
        or new.governing_policy_revision <> old.governing_policy_revision
        or new.reason <> old.reason
        or new.was_pinned <> old.was_pinned
        or new.correlation_id <> old.correlation_id
        or new.recorded_at <> old.recorded_at
        or new.not_before <> old.not_before
        or ((new.cancelled_at is null) = (new.completed_at is null)) then
        raise exception 'tombstone identity and deadline are immutable and closure is one-way';
    end if;
    return new;
end;
$$;

create trigger tombstones_guard_update
    before update on git_vault.tombstones
    for each row execute function git_vault.guard_tombstone_mutation();
create trigger tombstones_guard_delete
    before delete on git_vault.tombstones
    for each row execute function git_vault.guard_tombstone_mutation();

-- ---------------------------------------------------------------------------------------------
-- deletion plans and physical-object claims: authorized, grace-bounded external effects
-- ---------------------------------------------------------------------------------------------

create table git_vault.deletion_plans (
    plan_id              uuid        primary key,
    evaluation_id        uuid        not null references git_vault.retention_evaluations (evaluation_id),
    target_id            uuid        not null references git_vault.targets (target_id),
    snapshot_id          uuid        not null references git_vault.snapshots (snapshot_id),
    tombstone_id         uuid        references git_vault.tombstones (tombstone_id),
    reason               text        not null,
    automatic            boolean     not null,
    tombstoned_at        timestamptz not null,
    not_before           timestamptz not null,
    estimated_bytes      bigint      not null check (estimated_bytes >= 0),
    status               text        not null default 'planned',
    correlation_id       uuid        not null,
    cancelled_at         timestamptz,
    completed_at         timestamptz,

    constraint deletion_plans_reason_is_known
        check (reason in ('ordinary_retention', 'target_inactive', 'operator_request')),
    constraint deletion_plans_window_is_positive
        check (not_before > tombstoned_at),
    constraint deletion_plans_status_is_known
        check (status in ('planned', 'local_deleting', 'replica_deleting', 'completed', 'cancelled')),
    constraint deletion_plans_terminal_state_is_consistent
        check ((status = 'completed' and completed_at is not null and cancelled_at is null)
            or (status = 'cancelled' and cancelled_at is not null and completed_at is null)
            or (status not in ('completed', 'cancelled')
                and completed_at is null and cancelled_at is null))
);

create unique index deletion_plans_one_active_snapshot_key
    on git_vault.deletion_plans (snapshot_id)
    where status not in ('completed', 'cancelled');
create index deletion_plans_target_status_idx
    on git_vault.deletion_plans (target_id, status, not_before);

create function git_vault.guard_deletion_plan_mutation()
returns trigger
language plpgsql
as $$
begin
    if tg_op = 'DELETE' then
        raise exception 'deletion plan evidence is append-only';
    end if;
    if tg_op = 'INSERT' then
        if new.automatic and exists (
            select 1 from git_vault.snapshot_pins
            where snapshot_id = new.snapshot_id and revoked_at is null
        ) then
            raise exception 'active snapshot pin blocks automatic deletion plan'
                using errcode = 'VLT02';
        end if;
        return new;
    end if;
    if old.status in ('completed', 'cancelled')
        or new.plan_id <> old.plan_id
        or new.evaluation_id <> old.evaluation_id
        or new.target_id <> old.target_id
        or new.snapshot_id <> old.snapshot_id
        or new.tombstone_id is distinct from old.tombstone_id
        or new.reason <> old.reason
        or new.automatic <> old.automatic
        or new.tombstoned_at <> old.tombstoned_at
        or new.not_before <> old.not_before
        or new.estimated_bytes <> old.estimated_bytes
        or new.correlation_id <> old.correlation_id then
        raise exception 'deletion plan identity and deadline are immutable';
    end if;
    if not ((old.status = 'planned' and new.status in ('local_deleting', 'cancelled'))
        or (old.status = 'local_deleting' and new.status = 'replica_deleting')
        or (old.status = 'replica_deleting' and new.status = 'completed')) then
        raise exception 'illegal deletion plan transition % -> %', old.status, new.status;
    end if;
    if new.status = 'completed' and (
        exists (
            select 1 from git_vault.snapshot_artifacts artifact
            where artifact.snapshot_id = new.snapshot_id
              and not exists (
                  select 1 from git_vault.deletion_stage_attempts stage
                  where stage.plan_id = new.plan_id
                    and stage.stage_kind = 'local'
                    and stage.artifact_id = artifact.artifact_id
                    and stage.outcome in ('succeeded', 'shared_reference_retained')
              )
        )
        or exists (
            select 1
            from git_vault.replica_placements placement
            join git_vault.snapshot_artifacts artifact using (artifact_id)
            where artifact.snapshot_id = new.snapshot_id
              and not exists (
                  select 1 from git_vault.deletion_stage_attempts stage
                  where stage.plan_id = new.plan_id
                    and stage.stage_kind = 'replica'
                    and stage.placement_id = placement.placement_id
                  and stage.outcome in ('succeeded', 'shared_reference_retained')
              )
        )
        or (new.tombstone_id is not null and not exists (
            select 1 from git_vault.deletion_stage_attempts stage
            where stage.plan_id = new.plan_id
              and stage.stage_kind = 'mirror_local'
              and stage.outcome = 'succeeded'
        ))
    ) then
        raise exception 'deletion plan cannot complete with missing terminal stages';
    end if;
    return new;
end;
$$;

create trigger deletion_plans_guard_insert_or_update
    before insert or update on git_vault.deletion_plans
    for each row execute function git_vault.guard_deletion_plan_mutation();
create trigger deletion_plans_guard_delete
    before delete on git_vault.deletion_plans
    for each row execute function git_vault.guard_deletion_plan_mutation();

create table git_vault.physical_object_claims (
    claim_id         uuid        primary key,
    plan_id          uuid        not null references git_vault.deletion_plans (plan_id),
    identity_kind    text        not null,
    identity_key     text        not null,
    lease_owner      uuid        not null,
    lease_expires_at timestamptz not null,
    outcome          text        not null default 'running',
    failure_class    text,
    started_at       timestamptz not null,
    finished_at      timestamptz,

    constraint physical_object_claims_kind_is_known
        check (identity_kind in ('local_digest', 'mirror_path', 'replica_key')),
    constraint physical_object_claims_key_is_bounded
        check (length(identity_key) between 1 and 768),
    constraint physical_object_claims_outcome_is_known
        check (outcome in ('running', 'completed', 'abandoned')),
    constraint physical_object_claims_terminal_state_is_consistent
        check ((outcome = 'running' and finished_at is null and failure_class is null)
            or (outcome = 'completed' and finished_at is not null and failure_class is null)
            or (outcome = 'abandoned' and finished_at is not null and failure_class is not null)),
    constraint physical_object_claims_times_are_ordered
        check (started_at <= lease_expires_at and (finished_at is null or finished_at >= started_at))
);

create unique index physical_object_claims_one_live_identity_key
    on git_vault.physical_object_claims (identity_kind, identity_key)
    where outcome = 'running';
create index physical_object_claims_lease_idx
    on git_vault.physical_object_claims (lease_expires_at) where outcome = 'running';

create function git_vault.guard_physical_object_claim_mutation()
returns trigger
language plpgsql
as $$
begin
    if tg_op = 'DELETE' or old.outcome <> 'running'
        or new.claim_id <> old.claim_id
        or new.plan_id <> old.plan_id
        or new.identity_kind <> old.identity_kind
        or new.identity_key <> old.identity_key
        or new.lease_owner <> old.lease_owner
        or new.lease_expires_at <> old.lease_expires_at
        or new.started_at <> old.started_at then
        raise exception 'physical-object claim identity and terminal evidence are immutable';
    end if;
    return new;
end;
$$;

create trigger physical_object_claims_guard_update
    before update on git_vault.physical_object_claims
    for each row execute function git_vault.guard_physical_object_claim_mutation();
create trigger physical_object_claims_guard_delete
    before delete on git_vault.physical_object_claims
    for each row execute function git_vault.guard_physical_object_claim_mutation();

create table git_vault.retention_audit (
    audit_id       uuid        primary key,
    target_id      uuid        not null references git_vault.targets (target_id),
    snapshot_id    uuid        references git_vault.snapshots (snapshot_id),
    evaluation_id  uuid        references git_vault.retention_evaluations (evaluation_id),
    plan_id        uuid        references git_vault.deletion_plans (plan_id),
    event_kind     text        not null,
    reason         text        not null,
    outcome        text        not null,
    correlation_id uuid        not null,
    details        jsonb       not null check (jsonb_typeof(details) = 'object'),
    occurred_at    timestamptz not null,

    constraint retention_audit_event_is_known
        check (event_kind in ('pin', 'unpin', 'evaluation', 'tombstone', 'plan', 'stage', 'refusal')),
    constraint retention_audit_reason_is_bounded
        check (length(reason) between 1 and 64),
    constraint retention_audit_outcome_is_bounded
        check (length(outcome) between 1 and 64)
);

create index retention_audit_target_time_idx
    on git_vault.retention_audit (target_id, occurred_at, audit_id);
create index retention_audit_snapshot_time_idx
    on git_vault.retention_audit (snapshot_id, occurred_at, audit_id)
    where snapshot_id is not null;
create trigger retention_audit_is_append_only
    before update or delete on git_vault.retention_audit
    for each row execute function git_vault.reject_terminal_evidence_mutation();

-- ---------------------------------------------------------------------------------------------
-- storage_locations: backends Vault may place bytes in
-- ---------------------------------------------------------------------------------------------

create table git_vault.storage_locations (
    location_id     uuid        primary key,
    backend         text        not null,
    root            text        not null,
    created_at      timestamptz not null,

    constraint storage_locations_backend_is_known
        check (backend in ('local_filesystem', 's3_compatible')),
    constraint storage_locations_root_is_bounded
        check (length(root) between 1 and 512)
);

comment on table git_vault.storage_locations is
    'Configured placement roots. Off-host requirement is satisfied only against a location of a '
    'different backend than the primary (docs/ARCHITECTURE.md section 13).';
create unique index storage_locations_backend_root_key
    on git_vault.storage_locations (backend, root);

-- ---------------------------------------------------------------------------------------------
-- replica_targets, replication_attempts, replica_placements: off-host inventory and evidence
-- ---------------------------------------------------------------------------------------------

create table git_vault.replica_targets (
    replica_target_id uuid        primary key,
    name              text        not null unique,
    endpoint_origin   text        not null,
    bucket            text        not null,
    key_prefix        text        not null,
    required          boolean     not null,
    enabled           boolean     not null,
    first_seen_at     timestamptz not null,
    last_seen_at      timestamptz not null,

    constraint replica_targets_name_is_bounded
        check (name ~ '^[a-z0-9][a-z0-9_-]{0,62}$'),
    constraint replica_targets_endpoint_is_credential_free_origin
        check (length(endpoint_origin) between 8 and 512
            and endpoint_origin !~ '@'
            and endpoint_origin !~ '[?#]'),
    constraint replica_targets_bucket_is_bounded
        check (length(bucket) between 1 and 255),
    constraint replica_targets_key_prefix_is_safe
        check (length(key_prefix) <= 255
            and key_prefix !~ '(^|/)\.\.(/|$)'
            and key_prefix !~ '^/'
            and key_prefix !~ '/$'),
    constraint replica_targets_observation_times_are_ordered
        check (first_seen_at <= last_seen_at)
);

comment on table git_vault.replica_targets is
    'Credential-free observations of named S3-compatible targets. Secrets exist only in process '
    'environment and never enter this schema; first/last seen preserve operational history.';

alter table git_vault.restore_drills
    add constraint restore_drills_replica_target_fk
    foreign key (replica_target_id)
    references git_vault.replica_targets (replica_target_id);

create table git_vault.replication_attempts (
    attempt_id        uuid        primary key,
    artifact_id       uuid        not null references git_vault.snapshot_artifacts (artifact_id),
    replica_target_id uuid        not null references git_vault.replica_targets (replica_target_id),
    outcome           text        not null,
    failure_class     text,
    lease_owner       uuid        not null,
    lease_expires_at  timestamptz not null,
    remote_hash       bytea       check (remote_hash is null or length(remote_hash) = 32),
    remote_size_bytes bigint      check (remote_size_bytes is null or remote_size_bytes >= 0),
    started_at        timestamptz not null,
    finished_at       timestamptz,

    constraint replication_attempts_outcome_is_known
        check (outcome in ('running', 'succeeded', 'failed', 'abandoned')),
    constraint replication_attempts_failure_class_is_bounded
        check (failure_class is null or length(failure_class) between 1 and 64),
    constraint replication_attempts_terminal_fields_are_consistent
        check (
            (outcome = 'running' and failure_class is null and remote_hash is null
                and remote_size_bytes is null and finished_at is null)
            or (outcome = 'succeeded' and failure_class is null and remote_hash is not null
                and remote_size_bytes is not null and finished_at is not null)
            or (outcome in ('failed', 'abandoned') and failure_class is not null
                and remote_hash is null and remote_size_bytes is null and finished_at is not null)
        ),
    constraint replication_attempts_times_are_ordered
        check (started_at <= lease_expires_at
            and (finished_at is null or started_at <= finished_at))
);

comment on table git_vault.replication_attempts is
    'One leased transfer or re-verification attempt. Terminal rows are immutable evidence; an '
    'expired running row becomes abandoned and a retry receives a new attempt id.';

create unique index replication_attempts_one_live_unit_key
    on git_vault.replication_attempts (artifact_id, replica_target_id)
    where outcome = 'running';
create index replication_attempts_lease_recovery_idx
    on git_vault.replication_attempts (lease_expires_at) where outcome = 'running';
create index replication_attempts_unit_started_idx
    on git_vault.replication_attempts (artifact_id, replica_target_id, started_at desc);

create table git_vault.replica_placements (
    placement_id      uuid        primary key,
    artifact_id       uuid        not null references git_vault.snapshot_artifacts (artifact_id),
    replica_target_id uuid        not null references git_vault.replica_targets (replica_target_id),
    object_key        text        not null,
    sha256_hash       bytea       not null check (length(sha256_hash) = 32),
    size_bytes        bigint      not null check (size_bytes >= 0),
    first_placed_at   timestamptz not null,
    last_verified_at  timestamptz not null,
    last_attempt_id   uuid        not null references git_vault.replication_attempts (attempt_id),

    constraint replica_placements_object_key_is_content_derived
        check (length(object_key) between 75 and 512
            and object_key ~ '(^|/)sha256/[0-9a-f]{2}/[0-9a-f]{64}$'
            and object_key !~ '(^|/)\.\.(/|$)'),
    constraint replica_placements_times_are_ordered
        check (first_placed_at <= last_verified_at),
    constraint replica_placements_unit_key unique (artifact_id, replica_target_id)
);

comment on table git_vault.replica_placements is
    'Current verified location of one immutable snapshot artifact at one target. The attempt '
    'history remains append-only while this projection advances last_verified_at.';

create function git_vault.guard_replication_attempt_mutation()
returns trigger
language plpgsql
as $$
begin
    if tg_op = 'DELETE' or old.outcome <> 'running' then
        raise exception 'terminal replication attempt evidence is append-only';
    end if;
    if new.attempt_id <> old.attempt_id
        or new.artifact_id <> old.artifact_id
        or new.replica_target_id <> old.replica_target_id
        or new.lease_owner <> old.lease_owner
        or new.lease_expires_at <> old.lease_expires_at
        or new.started_at <> old.started_at then
        raise exception 'replication attempt identity and lease are immutable';
    end if;
    return new;
end;
$$;

create trigger replication_attempts_guard_update
    before update on git_vault.replication_attempts
    for each row execute function git_vault.guard_replication_attempt_mutation();
create trigger replication_attempts_guard_delete
    before delete on git_vault.replication_attempts
    for each row execute function git_vault.guard_replication_attempt_mutation();

-- ---------------------------------------------------------------------------------------------
-- deletion_stage_attempts: ordered local-first, replica-second journal
-- ---------------------------------------------------------------------------------------------

create table git_vault.deletion_stage_attempts (
    attempt_id        uuid        primary key,
    plan_id           uuid        not null references git_vault.deletion_plans (plan_id),
    stage_kind        text        not null,
    stage_key         text        not null,
    artifact_id       uuid        references git_vault.snapshot_artifacts (artifact_id),
    replica_target_id uuid        references git_vault.replica_targets (replica_target_id),
    placement_id      uuid        references git_vault.replica_placements (placement_id),
    claim_id          uuid        references git_vault.physical_object_claims (claim_id),
    ordinal           integer     not null check (ordinal >= 0),
    outcome           text        not null,
    failure_class     text,
    lease_owner       uuid        not null,
    lease_expires_at  timestamptz not null,
    absence_verified  boolean     not null default false,
    started_at        timestamptz not null,
    finished_at       timestamptz,

    constraint deletion_stage_attempts_kind_is_known
        check (stage_kind in ('local', 'mirror_local', 'replica')),
    constraint deletion_stage_attempts_key_is_bounded
        check (length(stage_key) between 1 and 255),
    constraint deletion_stage_attempts_shape_is_consistent
        check ((stage_kind = 'local' and artifact_id is not null
                    and replica_target_id is null and placement_id is null)
            or (stage_kind = 'mirror_local' and artifact_id is null
                    and replica_target_id is null and placement_id is null)
            or (stage_kind = 'replica' and artifact_id is not null
                    and replica_target_id is not null and placement_id is not null)),
    constraint deletion_stage_attempts_outcome_is_known
        check (outcome in (
            'running', 'succeeded', 'failed', 'abandoned',
            'shared_reference_retained', 'refused'
        )),
    constraint deletion_stage_attempts_terminal_state_is_consistent
        check ((outcome = 'running' and finished_at is null and failure_class is null
                    and not absence_verified)
            or (outcome = 'succeeded' and finished_at is not null and failure_class is null
                    and absence_verified)
            or (outcome = 'shared_reference_retained' and finished_at is not null
                    and failure_class is null and not absence_verified)
            or (outcome in ('failed', 'abandoned', 'refused') and finished_at is not null
                    and failure_class is not null and not absence_verified)),
    constraint deletion_stage_attempts_times_are_ordered
        check (started_at <= lease_expires_at and (finished_at is null or finished_at >= started_at)),
    constraint deletion_stage_attempts_plan_ordinal_key unique (plan_id, ordinal)
);

create unique index deletion_stage_attempts_one_live_unit_key
    on git_vault.deletion_stage_attempts (plan_id, stage_kind, stage_key)
    where outcome = 'running';
create index deletion_stage_attempts_plan_time_idx
    on git_vault.deletion_stage_attempts (plan_id, started_at, attempt_id);
create index deletion_stage_attempts_lease_idx
    on git_vault.deletion_stage_attempts (lease_expires_at) where outcome = 'running';

create function git_vault.guard_deletion_stage_attempt()
returns trigger
language plpgsql
as $$
declare
    deadline timestamptz;
    target_tombstone uuid;
    plan_snapshot uuid;
begin
    if tg_op = 'DELETE' then
        raise exception 'deletion stage evidence is append-only';
    end if;
    if tg_op = 'INSERT' then
        select not_before, tombstone_id, snapshot_id
        into deadline, target_tombstone, plan_snapshot
        from git_vault.deletion_plans
        where plan_id = new.plan_id
        for update;
        if deadline is null or clock_timestamp() < deadline then
            raise exception 'deletion grace window is active' using errcode = 'VLT03';
        end if;
        if new.stage_kind = 'replica' and not exists (
            select 1 from git_vault.deletion_stage_attempts local_stage
            where local_stage.plan_id = new.plan_id
              and local_stage.stage_kind = 'local'
              and local_stage.artifact_id = new.artifact_id
              and local_stage.outcome in ('succeeded', 'shared_reference_retained')
        ) then
            raise exception 'replica deletion requires terminal local evidence'
                using errcode = 'VLT04';
        end if;
        if new.stage_kind = 'mirror_local' and (
            target_tombstone is null or exists (
                select 1 from git_vault.snapshot_artifacts artifact
                where artifact.snapshot_id = plan_snapshot
                  and not exists (
                      select 1 from git_vault.deletion_stage_attempts local_stage
                      where local_stage.plan_id = new.plan_id
                        and local_stage.stage_kind = 'local'
                        and local_stage.artifact_id = artifact.artifact_id
                        and local_stage.outcome in ('succeeded', 'shared_reference_retained')
                  )
            )
        ) then
            raise exception 'mirror deletion requires tombstone and terminal local artifacts'
                using errcode = 'VLT04';
        end if;
        if new.stage_kind = 'replica' and target_tombstone is not null and not exists (
            select 1 from git_vault.deletion_stage_attempts mirror_stage
            where mirror_stage.plan_id = new.plan_id
              and mirror_stage.stage_kind = 'mirror_local'
              and mirror_stage.outcome = 'succeeded'
        ) then
            raise exception 'replica deletion requires terminal local mirror evidence'
                using errcode = 'VLT04';
        end if;
        return new;
    end if;
    if old.outcome <> 'running'
        or new.attempt_id <> old.attempt_id
        or new.plan_id <> old.plan_id
        or new.stage_kind <> old.stage_kind
        or new.stage_key <> old.stage_key
        or new.artifact_id is distinct from old.artifact_id
        or new.replica_target_id is distinct from old.replica_target_id
        or new.placement_id is distinct from old.placement_id
        or new.claim_id is distinct from old.claim_id
        or new.ordinal <> old.ordinal
        or new.lease_owner <> old.lease_owner
        or new.lease_expires_at <> old.lease_expires_at
        or new.started_at <> old.started_at then
        raise exception 'deletion stage identity and terminal evidence are immutable';
    end if;
    return new;
end;
$$;

create trigger deletion_stage_attempts_guard_insert_or_update
    before insert or update on git_vault.deletion_stage_attempts
    for each row execute function git_vault.guard_deletion_stage_attempt();
create trigger deletion_stage_attempts_guard_delete
    before delete on git_vault.deletion_stage_attempts
    for each row execute function git_vault.guard_deletion_stage_attempt();

-- ---------------------------------------------------------------------------------------------
-- collector_runs: completeness of each auxiliary collection behind a complete archive
-- ---------------------------------------------------------------------------------------------

create table git_vault.collector_runs (
    collector_run_id       uuid        primary key,
    target_id             uuid        not null references git_vault.targets (target_id),
    collector             text        not null,
    outcome               text        not null,
    mirror_lifecycle_run_id uuid       references git_vault.mirror_lifecycle_runs (run_id),
    snapshot_id           uuid        references git_vault.snapshots (snapshot_id),
    child_target_id       uuid        references git_vault.targets (target_id),
    tool_version          text,
    object_count          bigint      not null default 0 check (object_count >= 0),
    total_bytes           bigint      not null default 0 check (total_bytes >= 0),
    aggregate_hash        bytea       check (aggregate_hash is null or length(aggregate_hash) = 32),
    failure_class         text,
    ran_at                timestamptz not null,

    constraint collector_runs_collector_is_known
        check (collector in ('git_lfs', 'wiki')),
    constraint collector_runs_outcome_is_known
        check (outcome in ('complete', 'absent', 'incomplete', 'failed')),
    constraint collector_runs_terminal_evidence_is_consistent
        check (
            (outcome = 'complete' and failure_class is null)
            or (outcome = 'absent' and collector = 'wiki' and failure_class is null
                and child_target_id is null)
            or (outcome in ('incomplete', 'failed') and failure_class is not null)
        ),
    constraint collector_runs_lfs_shape_is_consistent
        check (
            collector <> 'git_lfs'
            or (mirror_lifecycle_run_id is not null and child_target_id is null
                and tool_version is not null
                and (outcome <> 'complete' or aggregate_hash is not null))
        ),
    constraint collector_runs_wiki_shape_is_consistent
        check (
            collector <> 'wiki'
            or (mirror_lifecycle_run_id is null and snapshot_id is null
                and tool_version is null and object_count = 0 and total_bytes = 0
                and aggregate_hash is null)
        )
);

comment on table git_vault.collector_runs is
    'A complete archive is a manifest of independent collectors, not one opaque command. Each run '
    'records its own completeness so partial success is visible instead of silently upgrading the '
    'snapshot to a completeness it does not have.';
create index collector_runs_target_ran_idx on git_vault.collector_runs (target_id, ran_at desc);
create index collector_runs_snapshot_idx
    on git_vault.collector_runs (snapshot_id) where snapshot_id is not null;

create trigger collector_runs_are_append_only
    before update or delete on git_vault.collector_runs
    for each row execute function git_vault.reject_terminal_evidence_mutation();

-- ---------------------------------------------------------------------------------------------
-- outbox: events Vault owes the bus, written in the same transaction as their cause
-- ---------------------------------------------------------------------------------------------

create table git_vault.outbox (
    event_id        uuid        primary key,
    event_type      text        not null,
    aggregate_type  text        not null,
    aggregate_id    uuid        not null,
    payload         jsonb       not null,
    created_at      timestamptz not null,
    published_at    timestamptz,

    -- Event names may carry several dotted segments (design D6: vault.target.state_changed.v1);
    -- the version suffix stays terminal and numeric.
    constraint outbox_event_type_is_versioned
        check (event_type ~ '^vault(\.[a-z_]+)+\.v[0-9]+$'),
    constraint outbox_aggregate_type_is_bounded
        check (length(aggregate_type) between 1 and 64)
);

comment on table git_vault.outbox is
    'Transactional outbox. Events are `verified` or `restorable` only after the corresponding '
    'evidence exists; the payload carries references and hashes, never credentials or repository '
    'contents.';
create index outbox_unpublished_idx on git_vault.outbox (created_at) where published_at is null;

-- ---------------------------------------------------------------------------------------------
-- inbox: deduplication for at-least-once delivery into Vault
-- ---------------------------------------------------------------------------------------------

create table git_vault.inbox (
    message_id      uuid        not null,
    source          text        not null,
    consumed_at     timestamptz not null,
    processed_at    timestamptz,

    constraint inbox_source_is_bounded
        check (length(source) between 1 and 64),

    constraint inbox_message_key primary key (source, message_id)
);

comment on table git_vault.inbox is
    'At-least-once delivery means duplicates happen. A duplicate desired-state event reconciles '
    'the same target rather than enrolling a second mirror, and this table is how the handler '
    'knows it has seen the message before.';
