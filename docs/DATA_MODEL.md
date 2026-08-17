# Vault data model

## Owned schema: `git_vault.*`

- `targets`, desired-state revisions, source references, pin/retention/status.
- `mirror_runs`, refs/object summaries, tool versions, safe command results.
- `snapshots`, artifacts, manifests, hashes, sizes, storage replicas.
- `verification_runs`, `restore_drills`, comparison results and SLA status.
- auxiliary collector runs/artifacts.
- tombstones, deletion approvals/attempts, outbox/inbox.

## Constraints

Source/target identity is stable and owner-scoped. Artifact hash/size/type/storage key is unique and immutable. Manifests reference exact artifact hashes and tool/policy versions. Successful mirror fetch does not set snapshot/restore success. Retention and deletion transitions are append-audited. Cross-schema writes/foreign keys are forbidden.

Filesystem paths are implementation details, validated and never public IDs. Credentials are encrypted secret references outside manifests/events.
