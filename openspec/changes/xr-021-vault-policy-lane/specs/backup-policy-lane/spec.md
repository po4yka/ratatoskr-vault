## ADDED Requirements

### Requirement: A policy document becomes whole-catalog desired state

Vault SHALL map each `DesiredBackupPolicy` to desired-state deliveries from inbox source `github-policy`: one `git_mirror` delivery per repository entry (provider `github`, external repository id the `repository:<uuid>` wire string, policy revision equal to the policy version, correlation `backup_policy:<version>`, message id `UUIDv5(command_id, repository_ref)`) and one `none` delivery for every repository previously governed by that source and absent from the document. A version that does not exceed the last applied version SHALL be refused as not monotonic.

#### Scenario: A repository dropped from the document is withdrawn

- **WHEN** a policy names repositories A and C while Vault previously governed A, B and C through source `github-policy`
- **THEN** the adapter returns `git_mirror` deliveries for A and C and a `none` delivery for B, all with the policy version as revision

#### Scenario: A stale version is refused

- **WHEN** a policy version is not greater than the last applied version
- **THEN** the adapter returns `PolicyVersionNotMonotonic` and no deliveries

### Requirement: Each command is answered by exactly one acknowledgement

Vault SHALL consume `vault.backup_policy.apply_requested.v1` from the pre-provisioned durable `ratatoskr_vault_backup_policy`, apply an accepted policy through the reconciliation cycle, and record exactly one `vault.backup_policy.acknowledged.v1` per command: `accepted` when the version moved the last applied version, `rejected` with reason `policy_version_not_monotonic` otherwise. A redelivered command SHALL add no rows and no second acknowledgement. Undecodable input, a command type or producer other than `ratatoskr-github`, and a tenant on a catalog-wide command SHALL be terminated, and transient storage failures SHALL be negatively acknowledged with a 2 second delay.

#### Scenario: A valid command enrols targets and acknowledges once

- **WHEN** a valid policy command with two repositories is delivered
- **THEN** two targets are enrolled, `git_vault.inbox` holds two `github-policy` rows, the last applied policy version equals the command's version, and exactly one accepted acknowledgement is published

#### Scenario: A redelivered command changes nothing

- **WHEN** the same command id is delivered a second time after its decision was recorded
- **THEN** no inbox, revision, command or outbox row is added and the message is acknowledged to the broker

#### Scenario: A stale version is rejected with a reason

- **WHEN** a command carries a version below the last applied version
- **THEN** a rejected acknowledgement naming `policy_version_not_monotonic` is published and no target changes

#### Scenario: A malformed envelope is terminated

- **WHEN** the delivered bytes are not a command envelope
- **THEN** the message is terminated, nothing is written, and it is not redelivered

### Requirement: Only the acknowledgement event is relayed, and only after the broker acknowledges

Vault SHALL publish outbox rows of type `vault.backup_policy.acknowledged.v1` to `evt.vault.backup_policy.acknowledged.v1` with `Nats-Msg-Id` equal to the event id, SHALL set `published_at` only after the JetStream publish acknowledgement resolves, and SHALL leave every other outbox event type unpublished and unchanged. A failed publish SHALL leave the row unpublished, increment its attempt count, record a safe error class and delay the next attempt.

#### Scenario: Neighbouring outbox facts stay unpublished

- **WHEN** a command is applied and enrolment writes `vault.target.state_changed.v1` rows beside the acknowledgement
- **THEN** only the acknowledgement row has `published_at` set

### Requirement: A lost bus makes the process not ready and ends it

Vault SHALL report a `bus` readiness check when a bus is configured, SHALL report not ready whenever any reported check fails, and SHALL exit non-zero when any supervised background task returns before an orderly shutdown. A configured bus without a configured database SHALL be a configuration error.

#### Scenario: The broker disappears

- **WHEN** the broker the running process is connected to stops
- **THEN** `/health/ready` reports `not_ready` with a failed `bus` check and the process exits non-zero

### Requirement: Vault holds a least-privilege bus identity

The repository SHALL carry the VAULT stanza of the reviewed ACL as `deploy/nats/identity.conf`: publish only `evt.vault.backup_policy.acknowledged.v1` and the INFO, MSG.NEXT and ACK subjects of `ratatoskr_vault_backup_policy`, subscribe only to `_INBOX.>`.

#### Scenario: The identity can do its job and nothing else

- **WHEN** the Vault identity connects to an authorization-enabled broker built from the fragment
- **THEN** it can fetch and acknowledge a policy command and publish the acknowledgement, and a publish to another `evt.*` subject or a consumer-info request on a foreign durable is refused
