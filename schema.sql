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
    status                text        not null,
    pinned                boolean     not null default false,
    created_at            timestamptz not null,
    updated_at            timestamptz not null,

    constraint targets_provider_is_known
        check (provider in ('github')),
    constraint targets_external_repository_id_is_bounded
        check (length(external_repository_id) between 1 and 255),
    -- The target state machine, AGENTS.md "Target and snapshot state machines".
    constraint targets_status_is_known
        check (status in (
            'requested', 'cloning', 'ready', 'fetching', 'snapshotting', 'verifying',
            'healthy', 'degraded', 'paused', 'excluded', 'deleting'
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
    'paused | excluded | deleting';

-- One provider repository is at most one target, so reconciliation of duplicate events converges.
create unique index targets_provider_external_id_key
    on git_vault.targets (provider, external_repository_id);

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

    -- The single normative pair set, mirrored from ratatoskr_vault_core::Transition::TRANSITIONS.
    -- The agreement walk asserts the two agree on every ordered pair, because two enforcement
    -- points that disagree are worse than one.
    if not exists (
        select 1
        from (values
            ('requested',    'cloning'),
            ('requested',    'excluded'),
            ('requested',    'deleting'),
            ('cloning',      'ready'),
            ('cloning',      'degraded'),
            ('cloning',      'excluded'),
            ('cloning',      'deleting'),
            ('ready',        'fetching'),
            ('ready',        'degraded'),
            ('ready',        'paused'),
            ('ready',        'excluded'),
            ('ready',        'deleting'),
            ('fetching',     'snapshotting'),
            ('fetching',     'ready'),
            ('fetching',     'degraded'),
            ('fetching',     'paused'),
            ('fetching',     'excluded'),
            ('fetching',     'deleting'),
            ('snapshotting', 'verifying'),
            ('snapshotting', 'degraded'),
            ('snapshotting', 'deleting'),
            ('verifying',    'healthy'),
            ('verifying',    'degraded'),
            ('verifying',    'deleting'),
            ('healthy',      'fetching'),
            ('healthy',      'degraded'),
            ('healthy',      'paused'),
            ('healthy',      'excluded'),
            ('healthy',      'deleting'),
            ('degraded',     'fetching'),
            ('degraded',     'cloning'),
            ('degraded',     'paused'),
            ('degraded',     'excluded'),
            ('degraded',     'deleting'),
            ('paused',       'ready'),
            ('paused',       'excluded'),
            ('paused',       'deleting'),
            ('excluded',     'requested'),
            ('excluded',     'deleting')
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
    size_bytes      bigint      not null check (size_bytes > 0),
    created_at      timestamptz not null,

    constraint snapshot_artifacts_kind_is_known
        check (kind in ('git_bundle', 'manifest', 'lfs_archive'))
);

comment on table git_vault.snapshot_artifacts is
    'Content-addressed bytes. The hash is computed while streaming and verified again after every '
    'transfer; a successful upload response alone never updates anything here.';
create index snapshot_artifacts_snapshot_idx on git_vault.snapshot_artifacts (snapshot_id);

-- ---------------------------------------------------------------------------------------------
-- manifests: the immutable evidence document describing a snapshot
-- ---------------------------------------------------------------------------------------------

create table git_vault.manifests (
    manifest_id     uuid        primary key,
    snapshot_id     uuid        not null unique references git_vault.snapshots (snapshot_id),
    schema_version  integer     not null default 1,
    manifest_hash   bytea       not null check (length(manifest_hash) = 32),
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
    check_id        uuid        primary key,
    snapshot_id     uuid        not null references git_vault.snapshots (snapshot_id),
    subject         text        not null,
    result          text        not null,
    checked_at      timestamptz not null,

    constraint integrity_checks_subject_is_known
        check (subject in ('bundle_verify', 'artifact_hash', 'remote_hash', 'restore_refs')),
    constraint integrity_checks_result_is_known
        check (result in ('passed', 'failed'))
);

comment on table git_vault.integrity_checks is
    'Every verification attempt, passed or failed. Failed checks are kept: an integrity failure '
    'that disappears from the record looks exactly like an integrity failure that never happened.';
create index integrity_checks_snapshot_checked_idx
    on git_vault.integrity_checks (snapshot_id, checked_at desc);

-- ---------------------------------------------------------------------------------------------
-- restore_drills: proof that an artifact can become a repository again
-- ---------------------------------------------------------------------------------------------

create table git_vault.restore_drills (
    drill_id        uuid        primary key,
    snapshot_id     uuid        not null references git_vault.snapshots (snapshot_id),
    outcome         text        not null,
    refs_matched    boolean     not null,
    lfs_restored    boolean,
    started_at      timestamptz not null,
    finished_at     timestamptz not null,

    constraint restore_drills_outcome_is_known
        check (outcome in ('passed', 'failed')),
    constraint restore_drills_times_are_ordered
        check (started_at <= finished_at)
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

-- ---------------------------------------------------------------------------------------------
-- retention_policies: the windows deletion is evaluated against
-- ---------------------------------------------------------------------------------------------

create table git_vault.retention_policies (
    policy_id           uuid        primary key,
    name                text        not null,
    grace_days          integer     not null check (grace_days between 0 and 3650),
    keep_last_verified  integer     not null check (keep_last_verified >= 1),
    created_at          timestamptz not null
);

comment on table git_vault.retention_policies is
    'Named retention windows. Deletion is evaluated against these plus pin state and tombstones; '
    'an inactive policy alone deletes nothing.';
create unique index retention_policies_name_key on git_vault.retention_policies (name);

-- ---------------------------------------------------------------------------------------------
-- tombstones: the audit record that a target was retired, and what still must be kept
-- ---------------------------------------------------------------------------------------------

create table git_vault.tombstones (
    tombstone_id    uuid        primary key,
    target_id       uuid        not null references git_vault.targets (target_id),
    reason          text        not null,
    was_pinned      boolean     not null,
    purge_after     timestamptz,
    recorded_at     timestamptz not null,

    constraint tombstones_reason_is_known
        check (reason in ('policy_inactive', 'retention_expired', 'operator_request'))
);

comment on table git_vault.tombstones is
    'An unstar is not a deletion. A tombstone records that a target left active service, starts '
    'the grace window, and outlives the data it describes so the deletion stays auditable.';
comment on column git_vault.tombstones.was_pinned is
    'Evidence at recording time. A target that was pinned reached a tombstone only through an '
    'explicit unpin, and the audit trail shows it.';
create unique index tombstones_target_active_key
    on git_vault.tombstones (target_id) where purge_after is null;

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
-- collector_runs: completeness of each auxiliary collection behind a complete archive
-- ---------------------------------------------------------------------------------------------

create table git_vault.collector_runs (
    collector_run_id uuid       primary key,
    snapshot_id     uuid        not null references git_vault.snapshots (snapshot_id),
    collector       text        not null,
    completeness    text        not null,
    object_count    bigint      not null default 0 check (object_count >= 0),
    total_bytes     bigint      not null default 0 check (total_bytes >= 0),
    ran_at          timestamptz not null,

    constraint collector_runs_collector_is_known
        check (collector in (
            'git_lfs', 'wiki', 'releases', 'issues', 'pull_requests', 'discussions', 'settings'
        )),
    -- Partial success is preserved honestly: a missing release asset does not erase the run.
    constraint collector_runs_completeness_is_known
        check (completeness in ('complete', 'partial', 'failed'))
);

comment on table git_vault.collector_runs is
    'A complete archive is a manifest of independent collectors, not one opaque command. Each run '
    'records its own completeness so partial success is visible instead of silently upgrading the '
    'snapshot to a completeness it does not have.';
create index collector_runs_snapshot_idx on git_vault.collector_runs (snapshot_id);

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
