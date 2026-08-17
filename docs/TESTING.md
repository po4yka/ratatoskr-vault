# Vault testing strategy

Generate local repositories covering branches/tags/notes/hidden refs, force updates, large objects, malformed refs, LFS, wiki/submodule cases, deletion, authentication failure, and hostile config/hook/filter attempts.

Required tests:

- Desired-state idempotency and state transitions.
- Mirror create/update/prune/interruption/cancellation.
- `fsck`, bundle creation/verify, hashes, manifests, and tool versions.
- Local/off-host upload, corruption, partial object, retry, and replica verification.
- Isolated restore and ref/object/LFS comparison.
- Quotas, process groups, environment/path confinement, and output redaction.
- Pin/grace/tombstone/retention/physical deletion matrix.
- SQL migrations, outbox/inbox replay, and GitHub->Vault workspace flow.

A test suite must prove that a successful fetch with failed verification/restore is not reported healthy.
