## Context

See `proposal.md` for motivation. Plan items 5 and 6 already publish signed full-bundle manifests into `LocalBlobStore`, re-read and hash those bytes, and reconstruct exact refs in a confined scratch root. The current restore implementation is deliberately local-only, the current schema has no replica inventory, and `offsite_required` is desired-state input without actual-state convergence behind it.

The development rules require one current API and one in-place `schema.sql`, with no migration or compatibility path. Snapshot bytes and S3 responses are untrusted. Local preservation must continue when the remote endpoint fails, while no required off-host copy can be called healthy until independently re-read and verified.

## Goals / Non-Goals

**Goals:**

- Add one narrow S3-compatible storage adapter with explicit credentials, finite network budgets, atomic publication, streamed transfer, and post-upload byte verification.
- Make replication repeatable after crashes without unbounded process queues or ambiguous duplicate artifact identities.
- Persist current placement inventory separately from immutable terminal attempt evidence.
- Derive truthful local/off-host snapshot health and permit a drill to prove restoration from replica bytes.
- Keep all production and acceptance paths deterministic, bounded, and credential-safe.

**Non-Goals:**

- Provider lifecycle/IAM provisioning, remote deletion, retention, tombstones, or remote garbage collection.
- AWS-specific control-plane features, multi-cloud adapters, presigned public URLs, or a generic cross-repository BlobStore contract.
- LFS, wiki, releases, auxiliary collectors, incremental bundles, or new manifest/API major versions.
- Claiming that the local S3 harness validates a provider's IAM, TLS, regional routing, or Object Lock implementation.

## Decisions

### D1: Apache `object_store` is the single S3-compatible client

Use Apache `object_store` 0.14.x with only the S3/Tokio/HTTP features required by Vault and pin the resolved graph in `Cargo.lock`. Its S3 builder supports explicit access keys, regions, custom endpoints, path-style requests, conditional operations, bounded retry configuration, atomic object puts, multipart uploads, and streaming gets behind a small object-store API. The project is Apache-governed, Apache-2.0 licensed (already permitted by `deny.toml`), supports Rust older than the workspace's pinned 1.97 toolchain, and avoids the much larger generated surface of a provider control-plane SDK.

Vault will call `AmazonS3Builder::new()`, never `from_env()`. The latter can consult token files, web identity, container metadata, and instance metadata; that violates the explicit environment-only credential boundary. The already typed `RATATOSKR__REPLICAS__...` configuration supplies endpoint, bucket, region, access key, secret key, and optional session token directly. Debug and error types expose only stable replica identity and failure classes.

The AWS SDK is rejected because this slice needs only object PUT/GET/HEAD/multipart semantics and would add a large provider-specific dependency and ambient credential machinery. A handwritten Reqwest/SigV4 client is rejected because signing, multipart completion, retries, and compatibility are security-sensitive protocol work already maintained by `object_store`. `rust-s3` is not selected because `object_store` gives the narrower trait seam, streaming adapters, conditional operations, and in-memory testing vocabulary used by this design.

Before landing, `cargo tree --locked --duplicates`, `cargo deny --locked check`, and the existing advisory gate must confirm the exact graph, licenses, and security state. The dependency is production-critical and will follow the repository's normal dependency-update workflow.

### D2: Replica configuration is explicit and fails closed

`VaultConfig` gains a list keyed by stable replica name. Each enabled entry has an HTTPS endpoint, bucket, region, optional safe key prefix, `required` flag, secret access-key fields, and finite connect, per-request, whole-attempt, object-byte, backlog-item, backlog-byte, and concurrency ceilings. Target names and prefixes use a closed safe character grammar; bucket names and endpoints are parsed, not concatenated into URLs.

Plain HTTP is accepted only when the parsed host is loopback, solely for the local acceptance harness. Production TLS uses verified roots and hostname validation. No certificate bypass, anonymous mode, proxy credential inheritance, credential file, metadata service, or arbitrary client option enters runtime configuration. At least one enabled `required=true` target must exist before an `offsite_required` desired state can converge; absence is a visible degraded configuration outcome, not local snapshot failure.

All timing is covered by one absolute replication-attempt deadline. Client connect and request caps are shorter phase limits. Library retries are finite, use only replay-safe S3 operations, and cannot extend the absolute deadline. A later scheduler pass, rather than an unbounded loop inside one worker, performs the next logical attempt.

### D3: Object keys are derived only from immutable BlobRefs

Every remote key is `<validated-prefix>/sha256/<first-two-hex>/<64-hex-digest>`. It contains no repository owner, name, URL, target identifier, credential, or caller-controlled path. The digest and expected length come from the local immutable BlobRef after local verification.

Bundles and manifests are replicated as one snapshot/replica unit, with bundle artifacts first and the manifest last. Completing the manifest does not by itself mark the unit verified; every remote object must be downloaded and hashed. Publishing the manifest last nevertheless avoids presenting an apparently complete replica while its named bundle is still uploading.

Uploads use atomic multipart completion and abort the owned multipart session after an error or cancellation when the client can still do so within the deadline. A retry uses the same deterministic key. If a key already exists, Vault verifies the existing bytes; matching bytes are idempotent success, while mismatching bytes are a typed corruption failure and are never overwritten merely to turn the attempt green. Provider ETags are retained only as diagnostics and never as SHA-256 evidence.

### D4: The adapter streams in both directions and owns byte accounting

A narrow async `ReplicaStore` seam exposes upload, verify/download, and head operations in terms of `BlobRef` and a validated object key. The S3 implementation reads local artifacts in fixed bounded chunks into `WriteMultipart`, counts uploaded bytes, and completes atomically. It consumes `get` as a stream, computes SHA-256 and length incrementally, and optionally writes the same verified stream to a create-new scratch file for restore.

The byte ceiling is checked before admission against BlobRef length and again while streaming. Short, long, stalled, truncated, or digest-mismatched responses fail with distinct bounded classes. No production code collects a bundle into one allocation. Manifests may be small today but use the same bounded streaming path so the contract does not depend on their current size.

### D5: Due work is derived; the admitted backlog is bounded

Vault does not insert an ever-growing in-memory or database job queue. A database query derives due snapshot/replica units from locally verified snapshots that lack a complete current placement set or whose replica verification is stale. It orders absent/oldest verification first with stable identifiers and retrieves at most `max_backlog_items + 1`, allowing the scheduler to report that more work remains without materializing it.

A pure admission planner packs at most the configured item and aggregate-byte ceilings. One semaphore caps active units. Excess units are returned as `deferred`; all later work remains discoverable from immutable snapshot plus placement evidence. This bounds process memory and active transfer state without discarding preservation intent.

One short transaction claims a unit by appending a running attempt with a finite lease. A partial unique constraint prevents two live attempts for the same snapshot/replica unit. On crash recovery, an expired attempt is terminally classified as abandoned and a new identifier claims the still-due unit. Filesystem/network transfer happens outside the transaction. Success or failure is committed in a second short transaction.

### D6: Three tables separate target identity, inventory, and attempts

Edit the current `schema.sql` in place to add:

- `replica_targets`: stable UUID/name, sanitized endpoint identity, bucket, prefix, required/enabled flags, configuration fingerprint, and observation timestamps; never credentials;
- `replica_placements`: one row per replica target and existing `snapshot_artifact`, deterministic key, current verification state, uploaded-at and last-verified-at, observed digest/length, and provider version/ETag as non-authoritative diagnostics;
- `replication_attempts`: append-only attempt identity, snapshot/replica correlation, lease, byte budget, start/finish, terminal outcome, stable failure class, and bounded stage evidence.

A placement is mutable actual-state inventory; terminal attempts are immutable evidence protected by the same trigger pattern as verification/drill reports. The transaction that records a passed re-download verification upserts the placement observation and finalizes its attempt together. A failed attempt cannot create or advance `last_verified_at`.

### D7: Snapshot health is a projection, not a rewrite of local proof

Add a core health projection with separate local and replica components. Local verification remains its existing immutable passed/failed report. For `offsite_required=false`, an optional replica failure is visible operationally but does not block snapshot health. For `offsite_required=true`, every enabled `required=true` target must have verified placements for the manifest and all named bundle artifacts; missing, stale, deferred, or failed placement yields `offsite_pending` or `offsite_degraded`. Only the complete set yields `offsite_verified` and overall healthy.

This projection can drive the existing snapshot progress vocabulary (`verified`, `offsite_uploading`, `offsite_verified`) without converting a locally valid snapshot to `failed` or erasing the last healthy snapshot. Status reads join the governing desired-state revision with placement inventory; metadata alone cannot manufacture a pass.

### D8: Restore source selection is explicit and reported

The drill planner gains `local`, `replica_preferred`, and `replica_required` source policies. A replica is eligible only when its manifest and every bundle placement are verified within policy and all inventory rows match the signed BlobRefs. Selection is deterministic: eligible replicas with the oldest successful drill evidence come first, then stable replica identity.

`replica_preferred` may fall back to verified local bytes and must report that actual choice. `replica_required` never falls back silently; it returns a typed source-unavailable/deferred result. The selected source identity and placement observations are persisted with the drill report.

For a replica drill, the network-enabled acquisition phase downloads manifest and bundle into a UUID-derived create-new scratch subtree while recomputing both digests. Only after those checks pass does the existing typed Git runner begin. Its child environment remains network-disabled and live-mirror-denied. Thus `network_disabled` continues to prove the Git reconstruction phase; new report fields separately prove the remote acquisition source and checksum result.

### D9: Failures are bounded, retryable evidence rather than global outages

Network, authentication, timeout, remote-not-found, multipart, size, and checksum failures map to a closed `ReplicationFailure` vocabulary. Raw endpoint URLs, request bodies, authorization headers, provider error text, object keys, and credentials never enter logs or metric labels. Telemetry uses replica name, operation, stage, outcome class, byte bucket, duration, attempt, backlog admitted/deferred counts, and last successful verification age.

An unavailable replica affects only its unit and off-host health. Snapshot construction, local verification, and local restore admission do not acquire replica permits and therefore continue. Shutdown stops new claims, allows active attempts to finish within one grace deadline, aborts owned multipart uploads where possible, awaits tasks, and leaves expired leases recoverable.

### D10: Acceptance uses a local S3-compatible HTTP harness plus hand-written fakes

The S3 adapter integration suite starts an Axum server on `127.0.0.1:0` with an in-memory object and multipart map. It implements only the signed path-style S3 operations Vault uses: multipart create/part/complete/abort, GET, and HEAD. It accepts but never logs the test SigV4 authorization header and supports deterministic truncation, corruption, delay, and unreachability controls. This exercises the real production client without public network, fixed ports, provider credentials, Docker, or a second S3 SDK.

Pure scheduler, health, and source-selection tests use hand-written fakes and controlled timestamps. PostgreSQL inventory tests run against the existing disposable same-engine harness. The end-to-end round trip streams a synthetic repository bundle and signed manifest through the real local store, S3 client, database inventory, replica download, and confined restore drill.

The harness proves Vault's request subset and state transitions, not general S3 conformance or production TLS/IAM behavior. A real-provider smoke test remains a deployment acceptance check and cannot be claimed by CI.

### D11: Bucket policy is documented but never provisioned by Vault

Operator documentation recommends a dedicated bucket or prefix and identity limited to ListBucket for that prefix plus GetObject, PutObject, AbortMultipartUpload, and multipart listing/part actions. DeleteObject is denied to the Vault writer. Bucket versioning, server-side encryption, access logging, and provider Object Lock/WORM are recommended where available. Provider lifecycle rules must never expire bytes earlier than Vault retention permits.

Vault neither creates nor edits remote bucket policy or lifecycle rules in this item. Remote deletion is deliberately absent until plan item 9 supplies retention authorization and audit semantics.

## Risks / Trade-offs

- [An S3-compatible provider implements only part of S3 multipart or conditional semantics] -> Exercise the exact request subset in the local harness, classify unsupported operations distinctly, document required compatibility, and do not weaken post-download verification.
- [A completed upload has an unknown client outcome] -> Reconcile the deterministic key by full GET and digest; matching bytes are success, mismatching or missing bytes remain failed/due.
- [A remote outage creates more due work than one pass can inspect] -> Limit query/admission windows and expose deferred counts/oldest age while deriving later work from durable snapshot evidence.
- [A corrupt object already occupies its content key] -> Fail closed, preserve mismatch evidence, and do not overwrite it automatically; operator remediation and remote deletion remain outside this item.
- [Multipart cancellation leaves orphan parts] -> Abort owned uploads within the attempt deadline and recommend a provider incomplete-multipart cleanup rule that does not affect completed snapshot objects.
- [Replica download temporarily doubles scratch usage] -> Admit against aggregate artifact bytes, create files only under drill-owned roots, and clean only the exact owned subtree after handles close.
- [The local harness accepts signatures without validating IAM] -> Keep its claim narrow and require a deployment smoke test for actual TLS, credentials, bucket policy, and provider behavior.
- [The new client adds transport/crypto dependencies] -> Minimize features, inspect the locked duplicate tree, run deny/advisory gates, and prefer existing workspace TLS/crypto providers when the crate feature set permits.

## Migration Plan

1. Add configuration and edit the current schema in place; recreate disposable development/test databases from `schema.sql`. No migration file or version negotiation is permitted.
2. Deploy with replicas configured but worker admission disabled, validate endpoint/TLS/bucket access through a bounded startup or operator probe that neither uploads production bytes nor exposes credentials, then enable the worker.
3. Backfill existing locally verified snapshots through the same bounded due-work planner. Snapshots with `offsite_required=true` remain visibly degraded until their complete required placement set verifies.
4. Enable replica-preferred scheduled drills after at least one full placement verifies, then observe a real off-host drill before treating the deployment as closing the single-host-loss gap.
5. Rollback disables replica admission first so local work continues, then rolls code and the current schema back and recreates development databases. Already uploaded remote objects are left intact; this item has no authority to delete them.
