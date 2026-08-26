## Why

The confined Git runner can safely execute individual commands, but Vault still cannot turn an
enrolled target into a bounded local mirror. The deployment target is a single small host, so
clone and fetch work must reserve storage before it starts, remain cancellable, and produce
integrity evidence instead of treating a successful Git exit as a healthy backup.

## What Changes

- Add a mirror lifecycle executor for the state-machine's clone and fetch work: enrollment creates
  a target-owned bare mirror, then later reconciliation fetches that same mirror.
- Apply per-mirror and process-global byte budgets before any operation starts. A request that
  cannot reserve its budget is refused, recorded as quota-exceeded, and transitions the target to
  `degraded`; it never prunes or evicts another mirror.
- Make clone and fetch cooperatively cancellable. Initial clones use a Vault-owned staging path
  that is removed on cancellation; fetch cancellation retains only a valid existing bare mirror
  and records a resumable checkpoint for the next lifecycle run.
- Run `git fsck --full` plus object/ref-count sanity checks after every successful clone or fetch,
  persist the per-run evidence, and transition integrity failures to `degraded`.
- Bound concurrent mirror operations to the deployment target's four CPU cores and its one-host
  storage budget.

## Capabilities

### New Capabilities

- `mirror-lifecycle`: bounded, state-machine-driven creation and refresh of local bare mirrors,
  including quota reservation, cancellation cleanup/checkpointing, integrity evidence, and
  concurrency admission.

### Modified Capabilities

None.

## Impact

- `crates/core` gains lifecycle policy and result types; `crates/persistence` owns durable mirror
  runs, checkpoints, and target observations; `services/vault` wires the state-machine plan to
  the lifecycle executor.
- `crates/gitrunner` gains only the typed Git verbs or result parsing required by this lifecycle;
  it remains the sole process-execution boundary.
- `schema.sql`, configuration, integration tests using local fixture repositories, README, and the
  DEVELOPMENT/CI gate list may change together where required. No bundles, snapshots, off-host
  storage, or LFS collectors are included.
