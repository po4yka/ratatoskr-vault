## Why

Immutable snapshot bytes are not sufficient backup evidence: Vault must continuously prove that a
stored bundle still matches its manifest and can recreate the exact recorded refs without consulting
or modifying the live mirror. This change delivers plan item 6, making restorability a durable,
scheduled, alertable fact rather than a one-time creation spot-check.

## What Changes

- Add scheduled verification jobs that re-hash stored bundle and manifest bytes, validate canonical
  manifest parent-digest chains and signatures, and run typed `git bundle verify` checks.
- Add isolated, network-disabled restore drills that reconstruct a repository from stored artifacts,
  verify connectivity, and compare every restored ref name and object ID with the manifest.
- Persist append-only verification and drill reports, including outcome, bounded diagnostics,
  timings, checked artifacts/refs, isolation evidence, and correlation metadata.
- Add finite configuration for verification cadence, drill cadence, sample size, scratch byte/time
  budgets, and concurrency; budget refusal records a truthful deferred outcome without touching live
  mirrors or existing snapshot evidence.
- Surface alert-worthy corruption or restore failures through versioned Vault event subjects in the
  transactional outbox, ready for the fleet publisher rather than claiming an absent bus delivery.
- Keep production cutover automation and cross-region restore execution out of this change.

## Capabilities

### New Capabilities

- `restore-verification`: Scheduled artifact verification, manifest trust-chain validation, isolated
  restore drills, durable reports, bounded selection, and alert-worthy outcome emission.

### Modified Capabilities

- `configuration`: Add strict, finite verification and restore-drill policy and resource budgets.
- `git-execution`: Add typed, allowlisted bundle verification and artifact-only restore operations
  whose runner cannot use the network or reach live mirror roots.
- `git-vault-schema`: Persist append-only verification/drill evidence and transactional fleet events
  in the current editable schema.
- `snapshot-bundles`: Make each current-version manifest carry verifiable signing and parent-chain
  evidence and define the artifact checks required before a snapshot can be called verified.

## Impact

- Affected packages: `ratatoskr-vault-core`, `ratatoskr-vault-gitrunner`,
  `ratatoskr-vault-blobstore`, `ratatoskr-vault-persistence`, and the `ratatoskr-vault` service.
- The current `schema.sql` and current manifest shape change in place; no migration, later API major,
  compatibility path, off-host adapter, or production cutover is introduced.
- Signature verification requires a reviewed cryptographic implementation and secret/public-key
  configuration; the design must select an existing audited library rather than custom crypto.
- The change records Vault-owned outbox facts with the fleet subject grammar. Publishing typed
  cross-repository payload contracts and operating a bus publisher remain separate delivery work;
  no local implementation claims either is live.
