## Context

The cross-repository contract is XR-021 CONTRACTS.md sections S01 (subjects and envelopes), S02 (outbox rows, relays, bus lifecycle), S03 (the Vault identity), S04 (the `ratatoskr_vault_backup_policy` durable) and S09 (the policy lane). This change implements the Vault side only and does not restate those sections.

Vault already has the pieces the lane reuses: `reconcile::run_cycle` over a `DeliverySource`, an inbox that dedups `(source, message_id)`, append-only desired-state revisions where the highest revision governs, and a transactional outbox that nothing relays yet. What is missing is a bus client, a way to know which targets a previous policy document governed, a durable memory of which policy version was last applied, and a relay.

Constraints that shape the design: one deployment and one schema owner (no migrations, `schema.sql` is edited in place); `clippy.toml` size limits (functions at most 100 lines, blocks at most 5 deep, files at most 850 lines); consumers verify and never create the Edge-provisioned durable; a publish refusal by the broker is visible only in the server log, so tests observe it through the missing acknowledgement.

## Goals / Non-Goals

**Goals:**

- A pure, I/O-free adapter from the contract document to Vault's own `DesiredStateDelivery`, so the mapping is tested without a database or a broker.
- Exactly one acknowledgement per accepted or rejected command, safe under redelivery and under a crash at any point between the first ingest and the acknowledgement.
- A relay that can only ever publish the acknowledgement event type, so enabling it cannot leak `vault.target.state_changed.v1` or alert facts onto the bus before their subjects and ACL exist.
- A process that cannot sit ready while its bus is gone.

**Non-Goals:**

- Executing mirrors for the enrolled targets, publishing any other outbox event type, account erasure transport, and Edge-side provisioning (Edge owns the durable and the streams).

## Decisions

### 1. The adapter is a pure function with explicit inputs

`policy_feed::deliveries_for_policy(command_id, policy, last_applied_version, previously_governed)` returns the deliveries or `PolicyFeedError::PolicyVersionNotMonotonic`. It receives the last applied version and the previously governed repository ids (as of before the command) as arguments rather than reading them, so the whole-catalog semantics (an absent repository becomes a `none` delivery) are decided in one place and exercised with plain values. The delivery message id is `UUIDv5(command_id, repository_ref)` so the same command always produces the same ids and the inbox dedups a redelivered command per repository.

The adapter returns `IncomingDelivery` values (moved from the service crate to `ratatoskr_vault_core::delivery`, with the call sites updated) so the service feeds them to `run_cycle` through a plain `Vec` source. Alternative considered: a second delivery type in core mapped by the service, rejected as a duplicate shape.

### 2. Revisions remember their source; correlation is text

`targets_governed_before` (it takes the policy version, see decision 3) must answer "which targets did source `github-policy` govern, and are they still wanted". The inbox cannot answer it (it has no target), so `desired_state_revisions` gains a nullable `source` column set by ingest. A target is governed by a source when its highest revision came from that source and its preservation level is not already `none`, so a document that keeps omitting a repository does not append a new `none` revision per version.

The contract correlation identifier is `backup_policy:<version>`, which is not a UUID, while `desired_state_revisions.correlation_id` was a UUID column and `governed_inputs` rejected anything else. The column becomes text (verbatim, as delivered). Retention evidence tables keep UUID correlation keys; `correlation_key` in core yields the UUID itself when the text is one and a stable UUIDv5 of the text otherwise, so tombstones and audit rows stay linkable to the revision text without changing their types.

### 3. A command ledger gives idempotence and the last applied version

`git_vault.backup_policy_commands` holds one row per policy command: command id (primary key), policy version, outcome (`accepted` or `rejected`), the rejection code, and the id of the acknowledgement outbox row. The last applied version is the maximum accepted version. A partial unique index forbids two accepted rows for one version.

`record_policy_decision` inserts the command row and the acknowledgement outbox row in one transaction and reports `AlreadyRecorded` without writing when the command id exists. A redelivered command that was fully handled therefore acknowledges the JetStream message and writes nothing.

Acknowledgement atomicity. The work order asks for the acknowledgement in the same transaction as the last ingest. Ingest is one transaction per delivery by design (the inbox slot and the target write must commit together per message) and `run_cycle` converges only after all ingests, so a single transaction around the whole command would change the reconciliation cycle's contract. Instead the decision row and the acknowledgement commit together after the final ingest, and the crash window between them is closed by a resume that actually converges:

- The set of previously governed repositories is read from the state before the command (`targets_governed_before(source, version)` ignores the source's own revisions at or above the command's version). It is therefore the same set on the first attempt and on every redelivery, however many of the command's deliveries already committed, so a dropped repository gets its `none` delivery again.
- `ingest_delivery` reports a replayed `(source, message_id)` pair as `Ingested { replayed: true }` with the target it resolved, instead of failing. `run_cycle` converges every target a delivery names, replayed or not, so a redelivered command re-runs convergence for every repository it names: a withdrawn one ends `excluded`, a re-added one is reactivated. Convergence is a plan against observed state, so repeating it moves nothing that already moved.
- The ledger row and the acknowledgement are written only after the cycle succeeded. A failed or interrupted attempt leaves no ledger row, so the broker redelivers and the same steps run again; the last applied version moves only with the ledger row.

### 4. The acknowledgement is a complete envelope in the outbox

The payload column holds the canonical `EventEnvelope` (producer `ratatoskr-vault`, `event_id` equal to the row id, `aggregate_id` `backup_policy:<acknowledged version>`, `correlation_id` copied from the command, `causation_id` `command:<command_id>`, no tenant). `outbox.aggregate_id` becomes text because an entity reference is not a UUID; the existing inserts bind the target or report id as text. `event_type` is constrained to a closed list (the four Vault event types) instead of a regular expression, and `attempt_count`, `last_error` and `next_attempt_at` carry relay bookkeeping so a failing head row never starves later rows.

A rejected acknowledgement for a stale version names `policy_version_not_monotonic`; `last_applied_policy_version` in the payload is the value before the decision, which is what the contract's own validation requires for an accepted acknowledgement (`acknowledged > last_applied`).

### 5. The consumer decides, the lifecycle owner acts

`policy_bus::handle_command` is a function from bytes and the database to a `Disposition` (`Ack`, `Term`, `Nak`). The consumer loop owns the broker interaction and applies the disposition: ack after the durable outcome committed, Term for undecodable input, wrong command type, wrong producer or a tenant on a catalog-wide command, Nak with a 2 s delay for transient storage failures. The durable is fetched with `get_consumer_from_stream` and its filter, ack policy and ack wait are checked against S04; a mismatch or absence refuses to start.

### 6. The relay is closed over one event type

The relay selects only `vault.backup_policy.acknowledged.v1` rows whose `next_attempt_at` is due, maps the type to `evt.` plus the type with a closed match (any other type is an error that stops the worker), parses the stored envelope (an invalid stored envelope is a programming error that stops the worker), publishes with `Nats-Msg-Id` equal to the row id and sets `published_at` only after the PubAck future resolves. A failure records a safe error class and a capped exponential backoff. Error text for an unacknowledged publish says to check the NATS server log for a Publish Violation.

### 7. Lifecycle: readiness is the conjunction of the checks, and a task exit is fatal

`RuntimeState` gains a `bus` check that is absent when no bus is configured, and `is_ready` now requires every reported check to pass. The runner supervises `Serving.tasks`: the first task that returns before a shutdown signal marks readiness failed, starts the normal drain and exits non-zero. A broker disconnect event flips the bus check immediately and ends the consumer, so the process exits and the supervisor restarts it (S02 rule 5) rather than reconnecting silently while advertising readiness. Configuring a bus without a database is a configuration error (exit 78), because the consumer could not finish its work.

### 8. The identity is a reviewed fragment proved by a real broker

`deploy/nats/identity.conf` is the VAULT stanza of S03. `authorized_bus.rs` starts an authorization-enabled `nats-server` from that file (public key substituted at runtime from a freshly generated nkey), checks the fragment against the S03 allow list after stripping comments, has an admin identity provision the durable, and then drives the Vault identity two ways. A plain client with only the fragment's permissions fetches and acknowledges a policy command, publishes the acknowledgement, and observes two refusals (another `evt.*` subject and `CONSUMER.INFO` on a foreign durable) as missing acknowledgements and as `Publish Violation` lines in the server log. A second test starts the shipped lane (`policy_bus::start`: the real connection code, the pull consumer stream and the relay's publish with headers) against the same broker and asserts a command is acknowledged, its acknowledgement reaches the events stream, the outbox row is marked published and the server log holds no violation.

## Risks / Trade-offs

- [The workspace equality check compares this fragment with Platform's VAULT stanza byte for byte after normalisation] -> The fragment follows the layout of the existing TELEGRAM stanza; the allow list order is the S03 order.
- [`is_ready` now fails on a down database] -> Intended; the previous behaviour reported ready while a check failed. Covered by a test.
- [No broker disconnect grace] -> A broker reload restarts Vault. Accepted: S02 rule 5 prefers a visible restart to a silent stale-ready state, and the outbox and ledger make the restart lossless.
- [The ack for a version commits after the final ingest, not inside it] -> See decision 3; the resume converges and is covered by `policy_bus.rs::a_redelivery_after_a_crash_still_excludes_the_dropped_repository` and `a_redelivery_after_a_crash_still_reactivates_a_readded_repository`, which commit a version's deliveries without converging and then redeliver the command.
- [Any error item from the pull message stream ends the lane] -> Deliberate. async-nats can recover from some of them (a missed idle heartbeat) by itself, but S02 rule 5 prefers a visible restart to a lane whose health the process cannot tell; the outbox and ledger make the restart lossless, and a single pull error should be rare on a healthy broker.
- [A very large catalog] -> The dropped-set computation is linear (a hash set). Ingest and convergence are sequential inside the 30 second ack wait and the consumer sends no in-progress acknowledgement, so a catalog of several thousand repositories could be redelivered while the first attempt still runs. The command ledger, the inbox and the idempotent convergence make the second attempt harmless but wasted work; in-progress acknowledgements are not added because nothing could exercise them below the 30 second contract value, and an untested timer is worse than a bounded waste.

## Migration Plan

None: development status applies, `schema.sql` is edited in place and databases are recreated. Deploy order is S14: reload NATS with the Vault stanza, start Edge, then Vault. Rollback: stop Vault; never delete the provisioned durable.
