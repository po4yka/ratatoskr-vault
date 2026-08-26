## Context

The desired-state change provides the durable target statuses and a pure plan that reports
`CloneAndFetch` as planned-but-unexecutable. The confined runner provides typed clone, fetch,
`fsck`, `rev-list`, and `show-ref` commands with process-group cancellation, but no lifecycle,
quota, or persistence boundary. The workspace deployment target permits one Raspberry Pi 5 with
four cores and finite NVMe storage; it is a code constraint, not a host to modify.

## Goals / Non-Goals

**Goals:**

- Execute the state-machine's clone/fetch work against an identifier-derived local mirror layout.
- Reserve capacity before Git starts; represent quota refusal, interruption, and integrity failure
  durably and truthfully.
- Preserve a known-good prior mirror when an update is interrupted or fails integrity.
- Test the real system Git binary against local fixture repositories only.

**Non-Goals:**

- Bundles, snapshots, manifests, restore drills, off-host replicas, LFS, auxiliary collectors,
  retention, or deletion.
- Scheduler daemons, network fixture remotes, changing the workspace deployment target, or
  reporting `healthy` from mirror work alone.

## Decisions

### D1. One lifecycle boundary, runner remains the only process boundary

`services/vault` turns executable reconciliation work into a `MirrorLifecycle` request. The
lifecycle owns admission, staging/publish, transition sequencing, and run evidence; it invokes
only typed `GitOperation`s through `GitRunner`. The core crate holds policy/value types and the
persistence crate owns database rows and reservations.

This avoids teaching the planner about I/O or allowing a service to spawn Git directly. A larger
worker/scheduler is deferred: the lifecycle takes one request at a time and has an async semaphore
for shared admission.

### D2. Reservation is pessimistic and non-destructive

Configuration supplies finite `per_mirror_bytes`, `global_mirror_bytes`, and a four-operation
maximum fixed to the deployment-target CPU budget. Before clone/fetch, persistence takes a
transactional, target-keyed reservation for the requested maximum. The admission query sums only
live reservations plus committed mirror bytes, serializing the decision under a stable lock. A
failed admission returns `QuotaExceeded`; it never runs Git and never calls prune/deletion.

Actual on-disk usage is measured after Git finishes. If it exceeds either budget, the run is
degraded and its new staging result is discarded; an existing mirror remains intact. Reservations
release on every completion/cancellation path. Pessimistic reservations trade some utilization for
the stronger invariant that concurrent clones cannot overcommit the single host.

### D3. Publish initial clones atomically; updates retain the last valid mirror

Initial clone targets a unique `<work-root>/<run-id>/mirror.git` staging directory. Only after
`fsck` and count checks succeed does the lifecycle rename that directory into the confined,
identifier-derived mirror path. Cancellation and every pre-publish failure remove only that
run-owned work directory.

Fetch runs against an existing bare mirror. Git's cancellation is process-group based in the
runner; afterwards the lifecycle rechecks the existing mirror before retaining a checkpoint. The
checkpoint contains the target, source/policy revision, operation kind, and prior successful
observation, not a claim that Git can resume a partially transferred pack. The next fetch is
Git's normal idempotent re-fetch against the existing mirror.

### D4. Integrity means fsck plus independently counted repository shape

After clone or fetch the lifecycle runs `git fsck --full`, `git show-ref`, and `git rev-list
--all`. It parses only bounded, strictly validated output to derive non-negative ref and unique
object/commit counts. No command succeeds by an exit status alone. The run persists the runner
outcomes and the resulting counts; an fsck error maps to `MirrorCorruption` and prevents a new
successful observation from replacing the last one.

### D5. Schema is edited in place with a test-created database

`schema.sql` adds vault-owned mirror observations, lifecycle runs, and quota reservations with
target/run uniqueness and no cross-schema foreign keys. The persistence APIs keep a transaction
open only for short row/reservation changes; no database connection is held while Git runs. There
are no migrations or version negotiation layers because the development-status rule overrides the
generic database playbook.

### D6. Tests use a controllable real-Git fixture and explicit cancellation seam

Integration fixtures create local bare/source repositories with `std::process::Command`; all
operations under test use the real configured Git binary through `GitRunner`. A lifecycle test
seam waits at named post-admission/pre-publish points using oneshot channels, allowing cancellation
to be asserted without sleeps or a fake Git binary. The tests cover quota refusal, clone cleanup,
fetch checkpoint/retry, and induced fsck failure.

## Risks / Trade-offs

- [A cancelled fetch could leave a corrupt mirror] → re-run `fsck` before retaining it; if that
  fails, record corruption and quarantine/leave it degraded rather than treating it as resumable.
- [Pessimistic reservations reduce throughput] → configuration makes the reservation explicit;
  never trading safety for unbounded host pressure is the primary requirement.
- [Filesystem use can grow between checks] → reserve before work and measure after it; cgroup or
  filesystem project quotas remain a deployment hardening follow-up.
- [A process crashes after a reservation] → reservations carry an expiry and are reclaimed only
  after their owner run is no longer active; tests cover release on normal cancellation, while
  crash recovery belongs to the future worker/lease integration.

## Migration Plan

Edit `schema.sql` in place and rebuild disposable databases. Rollout configures finite budgets and
the four-operation cap before enabling lifecycle work. Rollback stops new admissions and reverts the
application/schema together; no existing mirror is pruned or deleted by this change.
