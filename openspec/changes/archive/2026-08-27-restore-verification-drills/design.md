## Context

See `proposal.md` for motivation. Plan item 5 already publishes full bundles and canonical manifests
to a local content-addressed store and links them to healthy mirror observations. Its acceptance
spot-check invokes Git outside the confined runner and leaves verification/restoration as unpersisted
test behavior. The current schema contains minimal `integrity_checks`, `restore_drills`, and `outbox`
tables, but their shapes cannot carry the report evidence required by this change.

The development rules allow changing the current manifest and `schema.sql` in place. No historical
database or published manifest must be migrated. Git remains the only implementation allowed to
interpret bundles; repository bytes, refs, and configuration are hostile input.

## Goals / Non-Goals

**Goals:**

- Make a signed current-version manifest the trust root for repeated artifact verification.
- Prove exact ref restoration from stored bundle bytes without source or live-mirror access.
- Keep selection and execution finite, deterministic, and recoverable after budget deferral.
- Persist complete immutable reports and atomically enqueue alert-worthy failure facts.

**Non-Goals:**

- Operating a fleet message publisher or adding new cross-repository payload contracts.
- Network namespace/container orchestration, cross-region restore, or production cutover.
- Incremental bundles, LFS restoration, off-host replicas, or compatibility with unsigned manifests.

## Decisions

### D1: Ed25519 signs canonical unsigned manifest bytes

`SnapshotManifest` keeps schema version 1 and gains `signing_key_id` and a hex Ed25519 signature.
Signing serializes a private unsigned view whose field order is fixed by a Rust struct, then signs
those bytes with `ring`'s Ed25519 implementation. The key identifier is SHA-256 of the public key.
Verification reconstructs the same unsigned bytes, looks up the key identifier in the configured
trust set, and verifies before any manifest field can authorize Git work.

`ring` is selected because it is audited, already locked transitively in this workspace, has a
permissive licence accepted by `deny.toml`, and avoids custom cryptography. A hand-written signature
scheme is rejected. HMAC is rejected because any verifier could forge evidence. A new manifest major
or unsigned compatibility path is rejected by the development rules.

The signing seed remains a `SecretString` supplied by environment and decoded only at construction;
debug/configuration errors never render it. Tests use a deterministic non-production seed.

### D2: BlobRefs and signatures form a bounded parent chain

Verification resolves the requested manifest through `LocalBlobStore`, recomputes its digest and
length, verifies its signature, then follows each `parent_manifest` BlobRef. A visited-digest set
rejects cycles and a finite depth limit rejects pathological chains. A root manifest is trusted only
when its signature key exists in the configured trust set. Each bundle is likewise re-read and
re-hashed before Git sees a path.

The content-addressed manifest BlobRef protects bytes, while the signature establishes the trusted
producer and the parent BlobRef establishes ordering. Mutating a status column is therefore
insufficient to manufacture verification.

### D3: Restore uses only typed local Git operations

One drill owns `scratch_root/runs/<drill-id>/`. It creates an empty bare repository via a typed
`git init --bare`, runs typed `git bundle verify` against the stored local bundle, fetches all refs
from that bundle through a typed local-path operation, then runs existing `fsck --full` and
`show-ref`. The runner configuration carries denied live-mirror roots and refuses an overlapping or
resolving path before spawn.

Network exclusion is structural: restore operations accept `ConfinedPath`, never `SourceUrl`; the
child environment sets `GIT_ALLOW_PROTOCOL=file`, clears proxy variables, disables credentials,
config, hooks and prompting, and receives no remote URL. No repository code, checkout, hook, or
filter executes. A general-purpose shell/container wrapper is rejected because it would widen the
existing narrow runner and make CI platform-dependent.

### D4: Reports separate observation from persistence

The verifier returns a terminal `VerificationReport`; a successful artifact verification can then
feed a `RestoreDrill`, which always returns a terminal `RestoreDrillReport`, including failures.
Reports contain stable identifiers, BlobRefs, expected/observed ref summaries, stage outcomes,
timestamps, durations, isolation evidence, and one bounded typed failure. Persistence inserts them
append-only; retries mint new run identifiers.

This boundary keeps filesystem/Git work outside a database transaction. Only the short report plus
outbox insert is transactional, following the database skill's ownership rule.

### D5: Scheduling is a deterministic admission plan

`VerificationPolicy` owns positive verification/drill intervals, sample size, scratch-byte ceiling,
per-run deadline, and concurrency. A pure planner sorts due snapshots by absent/oldest successful
evidence and stable snapshot id, admits at most sample size and byte budget, and returns explicit
deferred candidates. An executor holds one semaphore permit per admitted run and carries the policy
deadline into the runner.

Deferral is not failure or success and opens no artifact. Keeping selection pure makes frequency and
budget behavior testable without sleeping or relying on wall-clock races.

### D6: Failed reports enqueue Vault facts; bus delivery remains absent

The persistence transaction for a failed verification or drill also inserts a uniquely keyed outbox
row. Subjects are `vault.snapshot.verification_failed.v1` and `vault.restore.failed.v1`, satisfying
the established `<bounded_context>.<aggregate>.<action>.v<major>` grammar. Payloads contain report,
snapshot and manifest identifiers plus a stable failure class, never repository names, paths,
credentials, or raw Git output.

This is durable producer evidence, not a claim that a bus publisher or downstream alert delivery is
running. Introducing a new cross-repository payload contract can consume these facts later without
changing verification behavior.

## Risks / Trade-offs

- [A signing key is lost or rotated incorrectly] -> Trust is keyed explicitly; unknown keys fail
  closed, multiple trusted public keys permit planned rotation, and old manifests remain immutable.
- [A hostile parent chain consumes unbounded work] -> Enforce a visited set and finite maximum depth.
- [Scratch cleanup follows an attacker-controlled path] -> Derive paths only from internal UUIDs,
  validate roots before use, and remove only the exact drill-owned directory after handles close.
- [Local-only transport policy is weaker than an OS network namespace] -> Typed operations expose no
  URL, `GIT_ALLOW_PROTOCOL=file` is mandatory, proxies/credentials are absent, and tests prove the
  prepared command/environment. OS-level sandboxing remains deployment hardening, not claimed here.
- [Large bundles monopolize disk or workers] -> Admit against declared artifact size, finite sample
  and aggregate scratch budgets, concurrency permits, and runner deadlines before opening bytes.
- [Outbox facts lack a published consumer payload contract] -> Keep payloads minimal and Vault-owned,
  do not claim publication, and require a later coordinated contracts change before consumers bind.

## Migration Plan

1. Change manifest schema version 1 and every snapshot call site together; recreate disposable test
   databases from the edited `schema.sql`.
2. Deploy signing/trust configuration before enabling snapshot or verification scheduling. Invalid
   or absent required keys fail configuration.
3. Enable artifact verification first, then restore drills with conservative sample/concurrency and
   scratch budgets. Existing unsigned development artifacts are deliberately not adopted.
4. Rollback is code plus current-schema rollback followed by disposable-database recreation. Signed
   artifacts remain immutable bytes but an older build cannot consume them; no compatibility path is
   maintained during development.
