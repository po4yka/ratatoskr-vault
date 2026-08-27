## ADDED Requirements

### Requirement: Git LFS operations are typed, confined, and non-checkout

Vault SHALL invoke the explicitly configured Git LFS executable directly with typed allowlisted operations for version reporting, all-ref fetch, referenced-object enumeration, and object verification. Arguments SHALL remain distinct values; the child SHALL use the hardened non-interactive credential and environment boundary; repository hooks, filters, and working-tree checkout MUST remain disabled; mirror, staging, and object paths SHALL be confined to Vault-owned roots; and existing wall-clock, process-group, output, and cancellation bounds SHALL apply.

#### Scenario: LFS pointer metadata cannot become a command

- **WHEN** an enabled fixture contains hostile path text and pointer metadata while Vault fetches and enumerates all LFS objects
- **THEN** the configured binary receives only typed arguments, no shell or filter executes, and all written bytes remain under the confined staging root

#### Scenario: Cancelled LFS fetch kills descendants

- **WHEN** cancellation arrives while a probe Git LFS process and its descendant are running
- **THEN** the whole process group terminates within the deadline and the operation returns a typed interruption with bounded redacted output

### Requirement: Wiki discovery is a bounded typed Git probe

Vault SHALL discover a wiki only through a direct typed remote-ref probe against a validated sibling Git URL. The probe SHALL distinguish present, confirmed absent, authentication failure, timeout, and other remote failure; it MUST NOT route through a shell, execute repository content, follow provider-supplied filesystem names, or interpret an unclassified failure as absence.

#### Scenario: Wiki probe classifies fixture sources

- **WHEN** the probe runs against deterministic present, absent, and failing sibling fixture sources
- **THEN** it returns the corresponding typed result within its deadline and creates no working tree or file outside its run root
