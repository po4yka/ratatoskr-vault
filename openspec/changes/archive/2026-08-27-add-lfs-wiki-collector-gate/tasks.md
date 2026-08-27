## 1. Closed configuration and collector policy

- [x] 1.1 RED: add strict LFS configuration and unapproved-collector planner tests; run the exact core selectors and observe missing validation/closed planning behavior.
- [x] 1.2 GREEN: add optional absolute-binary/finite LFS limits, the `GitLfs|Wiki` executable vocabulary, and explicit inert releases/issues divergences; run the core suite green.

## 2. Typed process boundaries

- [x] 2.1 RED: add hostile-pointer, descendant-cancellation, and present/absent/auth/timeout wiki probe tests; observe unsupported or incomplete typed behavior.
- [x] 2.2 GREEN: add the direct absolute-binary `LfsRunner`, hardened fixed environment, process-group cancellation, typed `ls-remote`, validated `.wiki.git` derivation, and fail-closed probe classification; run all gitrunner tests green.

## 3. Current schema and append-only evidence

- [x] 3.1 RED: add a disposable-database schema test for repository/wiki target shape, `git_lfs|wiki` collector closure, LFS object links, and restore proof columns; observe the missing current-schema shape.
- [x] 3.2 GREEN: edit only root `schema.sql`; add constrained sibling targets, append-only collector/LFS object evidence and LFS restore fields without a migration or version bump; run schema tests green.
- [x] 3.3 TEST: add persistence tests for canonical complete LFS evidence, immutable terminal collector/restore rows, repeated wiki enrollment, confirmed absence versus failure, and exact replication inventory.
- [x] 3.4 IMPLEMENT: add transactional LFS/wiki persistence methods, direct repository-only ingest conflict targeting, and atomic snapshot-to-LFS artifact links; run persistence tests green.

## 4. Explicit LFS mirror collection and quota

- [x] 4.1 RED: add a real local Git LFS fixture with two objects on distinct refs; observe the enabled collection fail before the run-owned standalone-file storage path is supported.
- [x] 4.2 GREEN: collect through run-owned bare staging, enumerate bounded unique OIDs, reject symlinks/missing/corrupt bytes, hash and create-only publish each object, clean the run child, and rerun the fixture green.
- [x] 4.3 TEST: add Git-only bypass, combined Git+LFS admission, staging-overrun/no-publication, and durable degraded quota evidence tests.
- [x] 4.4 IMPLEMENT: thread explicit `with_lfs` enablement through mirror lifecycle, conservatively add the LFS ceiling to pre-Git admission, persist typed terminal results, and preserve prior/shared storage on failure; run all LFS/mirror tests green.

## 5. Manifest, snapshot, and off-host completeness

- [x] 5.1 RED: add canonical reordered LFS manifest coverage in schema version 1 and observe differing or absent signed evidence.
- [x] 5.2 GREEN: add sorted object BlobRefs, total bytes, aggregate digest, derived inclusion, validation and signature coverage without a new manifest version; run core manifest tests green.
- [x] 5.3 TEST: add snapshot persistence and required-replication inventory checks covering bundle, manifest, and every LFS object.
- [x] 5.4 IMPLEMENT: publish/persist each LFS object as an immutable snapshot artifact so the existing replication planner and verifier include the complete set; run snapshot and replication tests green.

## 6. LFS verification and restore

- [x] 6.1 TEST: add end-to-end collect -> signed manifest -> source/live-mirror removal -> isolated restore, corrupt stored-object failure, and replica-only LFS restore coverage.
- [x] 6.2 IMPLEMENT: verify every LFS BlobRef, materialize create-new standard `lfs/objects/<2>/<2>/<oid>` paths, recompute count/bytes/aggregate proof, persist immutable reports, and ensure replica drills use only downloaded verified storage; run restore suites green.

## 7. Wiki sibling mirroring

- [x] 7.1 RED: add the typed wiki discovery classification test and observe the absent probe implementation.
- [x] 7.2 GREEN: implement bounded present/confirmed-absent/auth/timeout/other classification with no checkout; run gitrunner wiki tests green.
- [x] 7.3 TEST: add disposable-database convergence/absence/failure tests and a real local present-wiki mirror acceptance test.
- [x] 7.4 IMPLEMENT: derive a policy projection from the authorized parent, idempotently enroll one sibling target, append discovery evidence, and send the child through the shared independent mirror/quota lifecycle; run wiki tests green.

## 8. Operator contract and auxiliary approval gate

- [x] 8.1 TEST: add bounded collector telemetry-field coverage that excludes repository names, URLs, credentials, and provider output.
- [x] 8.2 IMPLEMENT: add closed LFS/wiki attempts, bytes, objects and duration telemetry vocabulary.
- [x] 8.3 DOCS: update configuration, README, architecture, implementation plan, testing, threat model, and ADR index with executable LFS/wiki behavior and the exact separate owner-approval checklist; state that releases/issues collectors are absent and inert.

## 9. Validation and delivery

- [x] 9.1 Run formatting and workspace clippy with warnings denied; fix causes without lint allowances.
- [x] 9.2 Run strict OpenSpec validation and the exact full `DEVELOPMENT.md` gate against the verified disposable PostgreSQL endpoint.
- [x] 9.3 Review the complete diff for migrations/version bumps, unapproved collectors, arbitrary commands, unsafe paths, quota bypass, false completeness, secrets, stale docs, missing call sites, and unrelated edits.
