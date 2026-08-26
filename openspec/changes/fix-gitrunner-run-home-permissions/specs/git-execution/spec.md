# Git Execution Specification (delta)

## MODIFIED Requirements

### Requirement: Out-of-band credentials

Credential material MUST reach Git only through the Git credential-helper contract backed by a per-operation secret file with owner-only permissions inside a run directory that admits no other users. The run directory's owner-only permissions MUST be established at the moment the directory is created, by the creating call itself, and MUST NOT depend on the host's ambient umask happening to already be restrictive enough. Secrets MUST NOT appear in any process argument vector or environment block. Captured output MUST be scanned and have every occurrence of active secret material replaced before the result leaves the runner.

#### Scenario: Run directory is owner-only regardless of the host umask

- **WHEN** the runner creates the credential run directory on a host whose ambient umask would otherwise leave a freshly created directory group- or world-accessible (for example the common `022`)
- **THEN** the created directory is `0700` and the operation proceeds; the outcome does not vary with the umask in effect on the host

#### Scenario: Helper answers the credential protocol

- **WHEN** the shipped credential-helper binary is invoked by Git's helper protocol with the operation URL on standard input and a prepared secret file
- **THEN** it emits exactly one `username=` line and one `password=` line carrying the stored values and nothing else

#### Scenario: Secret absent from argv and environment

- **WHEN** an operation runs with credentials attached
- **THEN** neither the Git argument vector nor the child environment contains any byte of the secret material (verified structurally on the prepared invocation and through an environment-printing probe)

#### Scenario: Leaked secret redacted from output

- **WHEN** a child process echoes active credential material into its captured output
- **THEN** the delivered result contains a redaction placeholder wherever the secret appeared and the raw secret nowhere

#### Scenario: Pre-existing run directory with wider permissions is refused

- **WHEN** the run directory already exists before the operation starts, with permissions wider than owner-only
- **THEN** the runner refuses the operation with a credential-write error and does not narrow the directory's permissions on the caller's behalf
