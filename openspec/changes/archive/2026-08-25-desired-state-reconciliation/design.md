# Design: Desired-State Target Reconciliation

## Context

See proposal.md for motivation. The foundation stores every table this change needs: `targets` (status constrained to the 11-value CHECK, no guard yet), append-only `desired_state_revisions` with `UNIQUE (target_id, policy_revision)`, `outbox`, `inbox` (dedup primary key `(source, message_id)`), and `sync_runs` with lease columns. `repos/platform/schema.sql` (~lines 480-660) already demonstrates the pattern this change copies for Vault: an immutable `status_rank` helper, a `guard_status_transition` trigger function that rejects illegal moves while allowing same-status annotation writes, and a trigger wired to the table. Workspace lints deny `unwrap`/`expect`/`panic`; sqlx runs without the migrate feature, embedding `SCHEMA: &str = include_str!("../../../schema.sql")`. The binding development status forbids migration files and versioning layers.

Exploration of `ratatoskr-github` confirmed no live publisher of backup policies exists yet (item-1 foundation only), and its placeholder `github_catalog.backup_policies` carries `(backup_policy_id, repository_id, policy_level, pinned, updated_at)` with no revision column. The workspace contracts store holds no `vault.*` schemas.

## Goals / Non-Goals

**Goals:**

- One authoritative definition of transition legality, enforced at both boundaries (database guard and application map) with an automated agreement proof.
- Purely computable convergence: `f(desired, observed) -> plan`, tested on fixture policies, executing today only the steps that need no Git runner (enroll, exclude, reactivate, record-only).
- At-least-once-safe ingestion: deduplication by message identity, append-only revision evidence, newest-revision-wins discipline.
- Transactionally evidenced transitions via `vault.target.state_changed.v1` in the existing outbox.

**Non-Goals:**

- No Git execution, bundles, snapshots, offsite upload, restore drills (items 3-8).
- No retention or tombstone workflow; `pinned` here only gates automatic exclusion.
- No periodic scheduler or daemon; the reconciliation entry point is a library function that tests call and future workers will call.
- No cross-repo contract publication to the `ratatoskr-workspace` store (recommended follow-up once a second repository consumes the input contract).
- No new status values (`auth_required`, `quota_exceeded`, `deleted` stay out; extension point documented).

## Decisions

### D1. Vault-owned input contract behind a source-trait stub

Vault defines its own desired-state struct: preservation level (`none` | `metadata_only` | `git_mirror` | `git_mirror_with_lfs` | `complete_archive`), `pinned`, wiki/releases/issues inclusion flags, offsite-required flag, correlation ID, monotonic per-target `policy_revision`. Deliveries enter through a narrow trait modelling "fetch undelivered messages"; a stub implementation serves fixtures in tests.

*Why*: the live publisher does not exist (confirmed by exploration), and depending on the catalog crate would couple Vault to star/list models, which the bounded-context rules forbid. The trait keeps the future publisher swap-in trivial because the contract is already Vault-shaped.

*Alternatives*: wait for the publisher (blocks item 2 indefinitely); import `ratatoskr-github` types (forbidden coupling; their rows lack a revision column, which Vault requires for staleness discipline).

### D2. Dual enforcement with an agreement test

Legality lives in one Rust constant table (`Transition`) and one database guard (`target_guard_status_transition` trigger + `target_status_rank` helper, mirroring the platform precedent). An integration test on a disposable database walks all 11x11 ordered pairs through both enforcement points and fails on any disagreement.

*Why both*: the database guard makes illegal state "refusable by the database today" even under raw SQL or future workers; the Rust map gives typed errors and lets the pure planner reason about reachability; the agreement test turns drift into a build failure instead of a production incident.

*Alternatives*: DB-only enforcement (opaque constraint errors, no pure planning); Rust-only (bypassable, violates the refusable-by-database posture).

Final transition map (the single normative listing; code and SQL comments reference it):

```
requested   -> cloning | excluded | deleting
cloning     -> ready | degraded | excluded | deleting
ready       -> fetching | degraded | paused | excluded | deleting
fetching    -> snapshotting | degraded | paused | excluded | deleting
snapshotting-> verifying | degraded | deleting
verifying   -> healthy | degraded | deleting
healthy     -> fetching | degraded | paused | excluded | deleting
degraded    -> fetching | paused | excluded | deleting
paused      -> ready | excluded | deleting
excluded    -> requested | deleting
deleting    -> (terminal)
Same-status writes are annotations, always permitted, never events.
```

Deviation from the first draft: `excluded -> requested` was added. Without it, a repository withdrawn (`none`) and later re-authorized strands in `excluded` forever, contradicting "reconcile actual state toward the latest authorized desired state".

### D3. Store everything, obey only the maximum

Every valid delivery inserts its revision row (append-only evidence, `UNIQUE (target_id, policy_revision)` makes replays inert). The governing revision is `max(policy_revision)` per target; only it drives planning and transitions.

*Why*: preserves delivery evidence, keeps retry semantics trivial under at-least-once transport, and makes staleness a read-time property rather than write-time rejection logic.

*Alternative*: reject stale revisions at the door - loses evidence and duplicates dedup logic already owned by the inbox.

### D4. Serialize per target with a row lock, not a lease

The ingest-plus-converge step runs in one transaction holding `SELECT ... FOR UPDATE` on the target row. Leases (`sync_runs`) remain reserved for long-running external work arriving with the runner items.

*Why*: these transactions are short and database-local; row locks scope exactly to the contended entity. Advisory locking stays reserved for schema initialization, its existing use.

### D5. Planner output separates executable from planned-but-unexecutable work

The pure planner yields work items: `Enroll`, `Exclude`, `Reactivate`, `RecordRevisionOnly`, plus forward-looking items (`CloneAndFetch`, `Snapshot`, ...). Today's executor performs the former four and reports the latter as planned-not-executable with structured counters - never as success.

*Why*: the acceptance criteria demand diff computation over fixture policies now, and honesty rules forbid pretending runner actions happened. Reporting keeps the missing capability visible instead of silent.

*Alternative*: defer planning until runners land - would ship untested diff logic together with execution risk later.

### D6. Events

Single payload type `vault.target.state_changed.v1`: `{target_id, from_status (null on enrollment insert), to_status, policy_revision, correlation_id}`. Written in the same transaction as the status write; enrollment emits `null -> requested`. Inbox dedup consumes `(source, message_id)` before any state work. No other event types are added.

### D7. Code placement

- `crates/core`: domain types (preservation level, desired state, target status enum), the `Transition` table, the pure planner, unit tests including exhaustive pair checks against the map.
- `crates/persistence`: store operations (revision insert, guarded transition, outbox writer, inbox claim), the schema additions, integration tests on disposable databases including the DB/Rust agreement walk and the failed-write atomicity case.
- `services/vault`: a thin module binding source -> ingest -> converge -> result; integration test drives the full path end to end.
- `schema.sql`: `target_status_rank(text)`, `target_guard_status_transition()` trigger function, trigger `targets_guard_status_transition` on `UPDATE OF status`.

### D8. Error taxonomy

Typed error set (`InvalidDelivery`, `DuplicateDelivery`, `IllegalTransition { from, to }`, storage errors) with `From<sqlx::Error>` at the persistence edge. No `unwrap`/`expect` anywhere, per workspace lints; refused transitions return typed values so callers distinguish expected conflicts from bugs.

## Risks / Trade-offs

- [Map drift between design doc, Rust table, SQL guard] -> the Rust table is the single runtime source; the agreement test proves database parity; this document is descriptive commentary, and the test, not the prose, is the gate.
- [Concurrent deliveries racing on one target] -> row-lock serialization plus the unique `(target_id, policy_revision)` index; the losing waiter re-reads committed state and converges to the same result.
- [`pinned` vs `none` semantics misread] -> the chosen reading (pin blocks automatic exclusion, divergence stays observable) is documented here; revisit with the retention owner before item 9 builds deletion workflows.
- [Planned-but-unexecutable work becomes invisible] -> structured counters and logs now; health-plane surfacing rides with later items rather than expanding this change.
- [Stub source diverges from the eventual real publisher] -> contract fields mirror the placeholder `backup_policies` shape plus the optional policy fields the architecture mandates; publish the JSON-schema contract to the workspace store as a follow-up when a second consumer appears.

## Migration Plan

Edit `schema.sql` in place; no migration files are created and disposable test databases rebuild from the embedded definition on every run. No database holds data that must survive (development status), so rollout is deploy-and-recreate. Rollback is reverting the commit; no compatibility shims are added.
