## ADDED Requirements

### Requirement: Deletion-staged artifacts are not replicated

Once an artifact has an executable deletion plan, Vault MUST exclude it from new upload and re-verification claims. An already running claim MUST NOT convert deletion-staged bytes into a new healthy placement after deletion execution begins.

#### Scenario: Replication scheduler skips deletion work

- **WHEN** due-replication discovery encounters an artifact whose deletion plan has started
- **THEN** the artifact is not returned as due work and no new replication attempt can be claimed for it

### Requirement: Replica deletion is verified and separately journaled

Vault SHALL delete a replica object only after the local deletion stage completes, SHALL verify remote absence through the storage adapter, and SHALL append one outcome per replica placement without rewriting prior replication evidence. Provider success without verified absence MUST NOT complete the stage.

#### Scenario: Provider acknowledgement without absence is incomplete

- **WHEN** a replica delete request succeeds but the object remains readable
- **THEN** the replica stage records verification failure, the plan remains incomplete, and prior placement and replication attempts remain queryable
