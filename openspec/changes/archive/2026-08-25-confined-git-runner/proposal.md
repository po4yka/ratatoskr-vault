# Proposal: confined-git-runner

## Why

Vault shells out to the system Git for every mirror operation, and Git pointed at a hostile
repository is an attack surface: option injection through clone URLs, hook and filter execution,
credential prompts, unbounded output, symlink escapes out of Vault-owned storage. Nothing in the
workspace can execute a Git process today. Plan item 3 is the security-critical foundation every
later pipeline stage (mirror lifecycle, snapshots, restore drills) must go through, so it lands
before any of them.

## What Changes

- Add a new workspace crate `ratatoskr-vault-gitrunner` owning all Git process execution behind a
  narrow runner interface.
- Commands are built structurally as typed argument vectors against an explicit subcommand
  allowlist; there is no shell anywhere on the path from intent to `exec`.
- Every invocation is confined to Vault-owned roots (validated canonical paths), runs with a
  sanitized minimal environment, carries a wall-clock timeout, caps captured stdout/stderr, kills
  its own process group on timeout or cancellation, and reports a structured bounded outcome.
- Credential material is supplied only through the Git credential-helper contract backed by a
  per-run `0600` file inside a `0700` run directory; secrets never appear in argv or the child
  environment, and captured output is redacted before it leaves the runner.
- A generated hostile-repository test suite (deterministic fixture builder, committed) proves each
  containment property: hostile names, symlink escapes, huge objects, promisor/partial-clone
  surprises, hook-execution attempts, config injection via clone URLs, oversized output.
- No schema change; no service wiring yet (mirror lifecycle is plan item 4).

## Capabilities

### New Capabilities

- `git-execution`: how Vault executes the system Git binary safely - structural command
  construction, allowlisting, filesystem confinement, environment hardening, resource bounds,
  credential handling, output redaction, and the hostile-repository containment guarantees.

### Modified Capabilities

None. Existing capabilities (configuration, desired-state-ingestion, git-vault-schema,
operator-health, service-runtime, target-state-machine) are untouched.

## Impact

- New crate `crates/gitrunner` added to the workspace members and dependency graph; no existing
  crate changes except the workspace manifest listing.
- New dependency `libc` (Unix process-group signalling) reviewed by `cargo deny check`.
- Documentation updates: README status line and DEVELOPMENT.md "Git commands" section gain the
  runner's exact command surface; no CI workflow list change (the gate steps are unchanged).
- The binary `ratatoskr-vault-git-credential-helper` ships inside the same crate as the
  credential-helper endpoint of the documented contract.
