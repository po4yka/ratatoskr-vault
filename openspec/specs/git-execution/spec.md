# git-execution Specification

## Purpose
Defines how Vault executes the system Git binary so that no repository, URL, ref name, or configuration can escape the confinement Vault places around backup work: structural command construction against an allowlist, validated filesystem roots, hardened child environments, wall-clock and output-size bounds, out-of-band credential delivery, and redacted results.

## Requirements

### Requirement: Structural command construction

Every Git invocation MUST be launched directly from an explicit argument vector built by typed code. No stage of command construction MAY route through a shell, and every invocation MUST name a subcommand present in the configured subcommand allowlist. A request outside the allowlist MUST be refused before any process starts.

#### Scenario: Disallowed subcommand refused without spawning

- **WHEN** an operation requests a subcommand that is not in the allowlist
- **THEN** the runner returns a refusal error immediately and no child process is created (a sentinel configured as the Git binary that would mark its own execution is never marked)

#### Scenario: Shell metacharacters remain inert data

- **WHEN** a repository under mirror operations contains ref names made of shell metacharacters such as `$(touch …)` and backticks
- **THEN** the operations complete treating those names as opaque data and no artifact named by the embedded command appears anywhere under the storage root

### Requirement: Source URL validation

A source URL MUST carry an allowlisted scheme (`https` or `file`) and MUST NOT begin with `-`. A URL failing either condition MUST be refused before any process starts, so option-shaped or exotic-protocol strings can never become Git arguments or transports.

#### Scenario: Dash-leading URL refused

- **WHEN** a source URL begins with `-` (for example `--upload-pack=touch /tmp/pwned`)
- **THEN** the runner refuses the operation with a validation error and no child process is created

#### Scenario: Exotic transport refused

- **WHEN** a source URL uses a scheme outside the allowlist (for example `ext::sh -c …`, `gitserver::…`, or plain `http://`)
- **THEN** the runner refuses the operation with a validation error before spawning

### Requirement: Filesystem confinement

Every filesystem path handed to Git MUST resolve, at use time, to a location inside a Vault-owned root passed to the runner. Intermediate symlinks, `..` components, absolute escapes, and destinations outside the root MUST be refused. Mirror paths MUST derive from internal target identifiers; provider-supplied names MUST never become path segments.

#### Scenario: Destination outside the root refused

- **WHEN** an operation names a destination that canonicalizes outside the configured root
- **THEN** the runner refuses the operation with a confinement error and creates nothing at the requested location

#### Scenario: Symlinked intermediate directory refused

- **WHEN** an otherwise-inside-root path traverses an intermediate component that is a symlink pointing outside the root
- **THEN** the runner refuses the operation with a confinement error

#### Scenario: Provider names never become paths

- **WHEN** two targets enroll with hostile display names (`../../pwned`, an absolute path, a name containing a null byte) alongside a well-named target
- **THEN** each enrolled mirror lives at an identifier-derived path inside the mirrors root, the hostile names appear nowhere on disk, and all three locations stay distinct

### Requirement: Hardened child environment

Each Git child MUST receive an environment built from policy, not inherited: only explicitly allowlisted variables plus documented hardening guards (system and global Git config excluded, terminal prompting and askpass programs disabled, hooks path pointed away from any executable script). A variable set in Vault's own process environment MUST NOT leak into the child unless allowlisted.

#### Scenario: Inherited variable does not reach the child

- **WHEN** the runner's own process environment carries a non-allowlisted variable and an operation runs
- **THEN** the spawned process observes none of that variable (observed through a probe configured as the Git binary that prints its environment)

#### Scenario: Hooks never execute

- **WHEN** a mirror's own configuration points `core.hooksPath` at scripts that write a marker file whenever they run, and the runner performs a fetch that updates refs
- **THEN** the fetch succeeds and no marker file exists anywhere under the storage root

### Requirement: Bounded execution

Every invocation MUST carry a wall-clock deadline and a per-stream output cap. A child exceeding the deadline MUST be killed together with its process group and the invocation MUST report a timeout. A stream exceeding its cap MUST terminate the child and report which stream overflowed with at most cap-plus-one bytes retained. Successful completions MUST report the exit code, both captured streams as they were retained, and elapsed time.

#### Scenario: Hung child killed at the deadline

- **WHEN** the configured Git binary ignores its arguments and waits forever, and the deadline elapses
- **THEN** the invocation reports a timeout error and the child process no longer exists when the call returns

#### Scenario: Oversized stdout terminated and reported

- **WHEN** an allowed command writes more than the stdout cap
- **THEN** the child is terminated, the invocation reports a stdout-overflow error, and retained bytes never exceed the cap

#### Scenario: Oversized stderr terminated and reported

- **WHEN** an allowed command writes more than the stderr cap
- **THEN** the child is terminated, the invocation reports a stderr-overflow error, and retained bytes never exceed the cap

#### Scenario: Clean completion reports structured evidence

- **WHEN** an allowed command finishes within deadline and caps
- **THEN** the result carries the exit code, the captured stdout and stderr, whether either was truncated, and the elapsed duration

### Requirement: Out-of-band credentials

Credential material MUST reach Git only through the Git credential-helper contract backed by a per-operation secret file with owner-only permissions inside a run directory that admits no other users. Secrets MUST NOT appear in any process argument vector or environment block. Captured output MUST be scanned and have every occurrence of active secret material replaced before the result leaves the runner.

#### Scenario: Helper answers the credential protocol

- **WHEN** the shipped credential-helper binary is invoked by Git's helper protocol with the operation URL on standard input and a prepared secret file
- **THEN** it emits exactly one `username=` line and one `password=` line carrying the stored values and nothing else

#### Scenario: Secret absent from argv and environment

- **WHEN** an operation runs with credentials attached
- **THEN** neither the Git argument vector nor the child environment contains any byte of the secret material (verified structurally on the prepared invocation and through an environment-printing probe)

#### Scenario: Leaked secret redacted from output

- **WHEN** a child process echoes active credential material into its captured output
- **THEN** the delivered result contains a redaction placeholder wherever the secret appeared and the raw secret nowhere

### Requirement: Hostile repository containment

The runner MUST remain correct and bounded against repositories crafted to attack it: hostile ref names survive mirror operations as data; repositories carrying very large objects complete verification within the configured bounds; partial-clone (promisor) repositories whose objects are missing produce an integrity failure classification rather than success or a hang; and no repository-controlled configuration can enable hook execution, alter the hardened environment, or widen the transport allowlist.

#### Scenario: Hostile ref names preserved as data

- **WHEN** a fixture repository carries branch and tag names combining shell metacharacters, Unicode, and maximum-length segments (Git's own ref syntax refuses dash-leading and `$(`-style names; those vectors live at the URL layer)
- **THEN** mirroring the repository preserves exactly that ref set and the subsequent listing reports the same names byte-for-byte

#### Scenario: Large-object repository verifies within bounds

- **WHEN** a fixture repository holds multi-megabyte blobs and the runner performs integrity verification on the mirrored copy
- **THEN** verification succeeds inside the default deadlines and the retained output stays within its caps

#### Scenario: Promisor repository integrity failure classified

- **WHEN** integrity verification runs on a mirror whose history references missing promisor objects
- **THEN** the invocation fails with an integrity-classified failure carrying diagnostic excerpts, within the deadline, and never reports success

#### Scenario: Config-injected clone URL refused

- **WHEN** a source URL smuggles configuration or transport options (`--config`, `-c`, `--upload-pack`) via leading dashes or scheme tricks
- **THEN** the runner refuses the operation before spawning, regardless of what follows the injected tokens

### Requirement: Deterministic hostile fixture generation

The hostile-repository suite MUST build its fixtures deterministically: running the committed generator twice MUST produce repositories with identical commit object identifiers, so suite failures reproduce anywhere.

#### Scenario: Generator reproduces identical state

- **WHEN** the fixture generator builds the same named fixture twice into different directories
- **THEN** every commit object identifier matches between the two builds

### Requirement: Typed full-bundle and ref operations

The Git runner SHALL expose full-bundle creation and complete-ref enumeration only as typed,
allowlisted direct-argument operations. A bundle destination SHALL be a confined Vault-owned path;
ref names and object IDs emitted by Git SHALL remain opaque data and SHALL NOT be interpolated into
commands, paths, or storage keys.

#### Scenario: Full bundle receives no shell-shaped input

- **WHEN** a healthy mirror has ref names containing shell metacharacters and Vault creates its
  full bundle and enumerates its refs
- **THEN** Git receives only the typed all-ref operation and a confined destination, the bundle
  preserves the refs, and no embedded command is executed
