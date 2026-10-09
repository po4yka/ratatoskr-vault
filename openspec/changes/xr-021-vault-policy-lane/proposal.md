## Why

Vault has no bus client and nothing ever hands it GitHub's desired backup policy: `reconcile::run_cycle` is called only from tests, so the policy lane between `ratatoskr-github` and Vault is a design on paper. XR-021 CONTRACTS.md section S09 fixes the lane: GitHub publishes `vault.backup_policy.apply_requested.v1` on `cmd.vault.backup_policy.apply_requested.v1`, Vault applies the whole-catalog `DesiredBackupPolicy` through its existing reconciliation cycle and answers with exactly one `vault.backup_policy.acknowledged.v1` on `evt.vault.backup_policy.acknowledged.v1`. Sections S01 to S04 fix the subjects, the outbox and consumer rules, the Vault NATS identity and the `ratatoskr_vault_backup_policy` durable that Edge provisions.

## What Changes

- Add a pure adapter `policy_feed` in `ratatoskr-vault-core` that maps one `DesiredBackupPolicy` plus the set of repositories previously governed by source `github-policy` to desired-state deliveries: one `git_mirror` delivery per entry, one `none` delivery for every previously governed repository the document no longer names, and `PolicyVersionNotMonotonic` for a version that does not exceed the last applied one.
- Make desired-state evidence carry what the lane needs: a revision remembers the inbox source that delivered it, a correlation identifier is the verbatim text of the delivery (`backup_policy:<version>`) instead of a UUID, and a command row per policy command records its outcome so the last applied version and redelivery are answerable from durable state.
- Add the acknowledgement outbox: the acknowledgement is a complete canonical `EventEnvelope` stored in `git_vault.outbox` (now with a text `aggregate_id`, a closed `event_type` list and relay bookkeeping columns), committed in the same transaction as the command row.
- Add the policy consumer: connect with the Vault nkey, verify (never create) the Edge-provisioned durable, decode the `CommandEnvelope`, run `policy_feed` and `reconcile::run_cycle`, write the acknowledgement, and ack, Term or Nak the message per S02 rule 7.
- Add the ack-gated relay: publish only `vault.backup_policy.acknowledged.v1` rows to `evt.vault.backup_policy.acknowledged.v1` with `Nats-Msg-Id`, set `published_at` only after the PubAck, back off on failure; every other outbox event type stays unpublished and unchanged.
- Add bus readiness and fail-fast lifecycle: a `bus` readiness check, readiness that fails when any check fails, and a process that exits non-zero when any background task returns before an orderly shutdown (S02 rule 5). A configured bus requires a configured database (exit 78 otherwise).
- Add the Vault NATS identity fragment `deploy/nats/identity.conf` (the VAULT stanza of S03) with an authorization-enabled real-broker test, and document the `/etc/ratatoskr/vault.nkey` seed.

Deliberate breaks, named per CONTRACTS.md S00: `git_vault.outbox.aggregate_id` changes from `uuid` to `text` and its `event_type` check becomes a closed list; `git_vault.desired_state_revisions.correlation_id` changes from `uuid` to `text`; `/health/ready` now fails when any reported check fails (a down database previously left the process ready). The schema is edited in place; no migration exists.

## Capabilities

### New Capabilities

- `backup-policy-lane`: Applying GitHub's whole-catalog desired backup policy from the bus, the acknowledgement outbox and its ack-gated relay, bus readiness and fail-fast lifecycle, and the Vault bus identity.

### Modified Capabilities

- `git-vault-schema`: The outbox gains a text aggregate reference, a closed event-type list and relay bookkeeping; desired-state revisions gain a source and a text correlation identifier; a policy command table records outcomes.

## Impact

`crates/core` (policy adapter, bus configuration), `crates/persistence` (source-aware ingest, policy command and relay queries), `crates/http` (bus readiness, task supervision), `services/vault` (policy consumer, relay, wiring), root `schema.sql`, `deploy/nats/identity.conf`, `deny.toml` (the contracts git source), CI (a NATS server for the real-broker tests), and operator documentation. Cross-repository behaviour is defined in XR-021 CONTRACTS.md sections S01 to S04 and S09 and is not restated here. Out of scope: Git runner execution of mirrors (policy is accepted and targets are enrolled; execution stays planned), publishing `vault.target.state_changed.v1`, and account erasure transport.
