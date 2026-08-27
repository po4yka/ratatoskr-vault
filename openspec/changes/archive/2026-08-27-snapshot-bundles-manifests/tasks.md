## 1. Immutable local BlobStore

- [x] 1.1 RED: add `crates/blobstore/tests/local_store.rs::existing_blob_is_immutable_and_identical_retry_is_deterministic`, with a compiling test seam, that asserts an identical retry returns the same BlobRef and a different stream claimed under that reference fails while the original bytes remain unchanged; run it and confirm the assertion fails.
- [x] 1.2 GREEN: add the `ratatoskr-vault-blobstore` workspace crate, the owner-scoped BlobRef model, owner-only confined local layout, bounded streaming SHA-256 publication, fsync/create-only atomic publish, duplicate verification, and cleanup of only run-owned temps; add the owner-approved `sha2` dependency and verify task 1.1 passes.

## 2. Typed evidence and Git operations

- [x] 2.1 RED: add `crates/core/tests/snapshot_manifest.rs::manifest_canonically_records_all_refs_digests_generator_parent_and_bundle_ref`, with a compiling test seam, that asserts sorted complete refs, ref-set digest, generator version, timestamp, optional parent, and full bundle BlobRef fields; run it and confirm the expected manifest assertion fails.
- [x] 2.2 GREEN: implement the pure ordered snapshot manifest/ref evidence types and canonical JSON serialization/digest in `crates/core`; verify task 2.1 passes.
- [x] 2.3 RED: add `crates/gitrunner/tests/bundle.rs::full_bundle_operation_is_typed_all_ref_and_confined`, with a compiling test seam, that asserts the runner's exact argument vector uses `bundle create <confined-output> --all` and keeps hostile ref data out of command/path construction; run it and confirm the assertion fails.
- [x] 2.4 GREEN: extend the closed Git subcommand enum and typed builders with full bundle creation, preserving direct argument construction, confinement, bounds, and allowlist refusal; verify task 2.3 passes.

## 3. Snapshot persistence

- [x] 3.1 RED: add `crates/persistence/tests/snapshots.rs::snapshot_records_link_one_manifest_and_bundle_refs_to_the_successful_mirror_run`, with a compiling test seam, that asserts a fresh disposable schema accepts one append-only manifest BlobRef and bundle BlobRef for an exact successful mirror observation and rejects a second manifest; run it and confirm the database assertion fails.
- [x] 3.2 GREEN: edit the current `schema.sql` in place and add persistence APIs for source mirror-run linkage, parent snapshots, immutable BlobRef fields, and transactional append-only snapshot/artifact/manifest insertion; verify task 3.1 passes.

## 4. Snapshot lifecycle acceptance

- [x] 4.1 RED: add `services/vault/tests/snapshot_lifecycle.rs::healthy_fixture_mirror_produces_complete_immutable_restorable_bundle_evidence`, with a compiling test seam, that snapshots a healthy fixture mirror and asserts exact manifest refs/digests, deterministic BlobStore retry semantics, persisted links, and successful clone of the stored bundle into an isolated temporary directory; run it and confirm the stated assertion fails.
- [x] 4.2 GREEN: add `SnapshotLifecycle`, strict optional snapshot configuration, staging ownership checks, healthy-observation selection, bundle/ref capture, BlobStore streaming, manifest publication, parent lookup, and persistence composition; verify task 4.1 passes without creating a production verification or restore-success claim.
- [x] 4.3 RED: add `services/vault/tests/snapshot_lifecycle.rs::publication_failure_preserves_the_last_healthy_mirror_observation`, with a compiling fault-injection seam, that makes BlobStore publication fail and asserts no snapshot success is written while mirror evidence and target health do not advance; run it and confirm the assertion fails.
- [x] 4.4 GREEN: implement typed failure handling and transactional boundaries so snapshot publication failure preserves prior mirror evidence and does not report verified/healthy snapshot state; verify task 4.3 passes.

## 5. Documentation and full gate

- [x] 5.1 Update `README.md`, `DEVELOPMENT.md`, and relevant architecture documentation for implemented local immutable bundle/manifests, explicitly retaining item 6 verification/restore and item 7 off-host placement as future work; no failing test applies because this is documentation, then verify links and status wording against the code.
- [x] 5.2 Run `npx --yes @fission-ai/openspec@1.10.0 validate snapshot-bundles-manifests --strict`, then every `DEVELOPMENT.md` gate command with each compiler-backed command wrapped in `build-gate --`; verify every command exits 0 and review the final diff for scope, credentials, generated artifacts, and unstated success claims.
