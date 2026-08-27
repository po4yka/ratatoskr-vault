## 1. Signed manifest trust chain

- [x] 1.1 Add `crates/core/tests/snapshot_manifest.rs::signed_manifest_rejects_tampered_payload` and run it RED, proving altered manifest evidence is currently accepted or cannot be verified rather than failing to compile.
- [x] 1.2 Add the direct audited `ring` dependency and current-version Ed25519 signing/verification model until 1.1 passes with deterministic canonical unsigned bytes and no secret rendering.
- [x] 1.3 Add `crates/core/tests/snapshot_manifest.rs::manifest_chain_rejects_unknown_key_cycle_and_broken_parent` and run it RED on the missing bounded chain verifier.
- [x] 1.4 Implement trusted-key lookup plus bounded/cycle-safe parent BlobRef traversal until 1.3 passes.

## 2. Stored-byte verification

- [x] 2.1 Add `crates/blobstore/tests/local_store.rs::stored_blob_hash_mismatch_is_detected` and run it RED, asserting a corrupted stored bundle is classified as a digest mismatch.
- [x] 2.2 Implement streamed BlobRef size/digest verification without publishing or rewriting bytes until 2.1 passes.

## 3. Confined Git restore operations

- [x] 3.1 Add `crates/gitrunner/tests/bundle.rs::restore_operations_accept_only_confined_local_bundle_paths` and run it RED, asserting typed init/verify/fetch operations have no URL or shell-shaped argument.
- [x] 3.2 Implement typed bare-init, bundle-verify, and local-bundle-fetch operations plus the file-only child environment until 3.1 passes.
- [x] 3.3 Add `crates/gitrunner/tests/bundle.rs::restore_runner_refuses_live_mirror_paths_before_spawn` and run it RED, asserting a denied-root sentinel is never executed and the mirror tree is unchanged.
- [x] 3.4 Add denied-root validation to runner preparation and apply it to restore paths until 3.3 passes.

## 4. Budget-aware scheduling

- [x] 4.1 Add `services/vault/tests/restore_verification.rs::schedule_selects_oldest_due_snapshots_within_sample_and_byte_budgets` and run it RED on the missing planner.
- [x] 4.2 Implement strict finite `VerificationPolicy`, deterministic due selection, concurrency admission, and explicit deferrals until 4.1 passes.

## 5. Artifact verification and isolated restore drills

- [x] 5.1 Add `services/vault/tests/restore_verification.rs::stored_bundle_hash_mismatch_produces_complete_failed_verification_report` and run it RED on the missing verifier/report API.
- [x] 5.2 Implement manifest-chain and stored-artifact verification with terminal stage timings and typed failures until 5.1 passes.
- [x] 5.3 Add `services/vault/tests/restore_verification.rs::valid_bundle_restores_exact_manifest_refs_without_live_mirror_access` and run it RED, asserting bundle-only reconstruction, connectivity, exact refs, complete timings, and scratch-only cleanup.
- [x] 5.4 Implement the isolated restore drill orchestration and complete pass report until 5.3 passes.
- [x] 5.5 Add `services/vault/tests/restore_verification.rs::restore_ref_mismatch_is_failed_and_live_mirror_remains_unchanged` and run it RED, asserting mismatch classification and live-mirror byte preservation.
- [x] 5.6 Implement failure capture/cleanup so every started drill returns terminal immutable evidence until 5.5 passes.

## 6. Durable reports and alert-worthy outbox facts

- [x] 6.1 Add `crates/persistence/tests/restore_verification.rs::failed_drill_report_and_alert_fact_commit_atomically_and_remain_immutable` and run it RED against a disposable PostgreSQL database.
- [x] 6.2 Edit current `schema.sql` and add persistence transactions for complete verification/drill reports and uniquely keyed versioned failure facts until 6.1 passes; no migration is permitted.
- [x] 6.3 Add `crates/persistence/tests/restore_verification.rs::pass_and_failure_retries_append_distinct_terminal_reports` and run it RED, asserting prior terminal evidence cannot be rewritten.
- [x] 6.4 Implement append-only retry semantics and due-snapshot report reads until 6.3 passes.

## 7. Strict runtime configuration

- [x] 7.1 Add `crates/core/tests/config_strictness.rs::verification_policy_rejects_zero_budgets_overlapping_roots_and_bad_keys` and run it RED with assertions for every named violation and secret redaction.
- [x] 7.2 Add the one current verification configuration section, environment examples, and semantic validation until 7.1 passes.

## 8. Integration, documentation, and delivery

- [x] 8.1 Update service composition and `DEVELOPMENT.md`/architecture documentation to describe signed manifests, local-only drills, finite schedules, and the outbox-without-publisher boundary; no failing test applies because these are wiring/documentation updates, then verify affected integration tests.
- [x] 8.2 Run targeted crate suites, `openspec validate restore-verification-drills --strict`, and the complete `DEVELOPMENT.md` gate through `build-gate`; fix root causes without weakening checks.
- [x] 8.3 Review the final diff, mark only observed tasks complete, sync every delta spec into the current main specs, and validate the completed change before archival.
