# Design: confined-git-runner

## Context

The workspace has no process-execution code today. `docs/ARCHITECTURE.md` section 7 fixes the
constraints (no shell, allowlist, sanitized env, hooks off, deadlines, output caps, safe
credentials); `docs/INTERFACES.md` names `GitRunner` as the boundary. The workspace forbids
`unsafe`, `unwrap`, `expect`, and `panic!`; every public item needs documentation; functions stay
under 100 lines. The runner must therefore be built from small pure validators plus one spawn
module, with the dangerous decisions concentrated in one place.

## Crate placement

New crate `crates/gitrunner` (`ratatoskr-vault-gitrunner`). Rationale: Git execution is
security-critical, has its own threat model, and must not leak into domain crates; a separate
crate makes the attack surface auditable by listing its dependencies alone. It depends on tokio,
libc (process-group signalling), thiserror, and nothing else from outside the workspace.

## Module layout

```text
crates/gitrunner/src/
  lib.rs          facade: GitRunner, RunConfig, re-exports
  command.rs      GitCommand spec: subcommand enum + typed argument builders
  url.rs          SourceUrl validation (scheme allowlist, dash rejection)
  confine.rs      path confinement validator (canonicalize-at-use, root prefix check)
  env.rs          environment builder (allowlist + hardening guards)
  outcome.rs      bounded capture types + redaction
  error.rs        typed failures incl. refusal classes
  credentials.rs  per-run secret file materialization + helper wiring
process.rs        spawn/wait/kill loop under src/spawn.rs (single place touching Command)
bin/git-credential-helper.rs  the credential-protocol endpoint
```

Each module stays far below the 850-line file limit; the spawn loop is the only function expected
to need a documented `#[expect(clippy::too_many_lines)]`.

## Key decisions

### D1 — Structural commands, fixed allowlist

`Subcommand` is a closed enum (`CloneMirror`, `Fetch`, `RemoteUpdatePrune`, `Fsck`, `BundleCreate`,
`BundleVerify`, `RevList`, `ShowRef`, `ForEachRef`, `Version`). Argument builders return
`Vec<OsString>` assembled from typed values only; callers cannot append raw strings for clone/fetch
operations that carry URLs or paths - those flow through validated wrappers (`SourceUrl`,
`ConfinedPath`). Unknown operations are unrepresentable; an operation requesting a subcommand not
enabled in `RunConfig.allow` is refused pre-spawn.

### D2 — Confinement at use time

`ConfinedPath::new(root, relative)` canonicalizes the parent chain and rejects if the resolved path
does not start with the canonicalized root, or if any intermediate component is a symlink leaving
the root. TOCTOU between check and use remains inherent to filesystem confinement; the design
narrows it by resolving immediately before spawn and by keeping all work inside root-owned
directories created with mode `0700`. Mirror layout is `<root>/mirrors/<first-two-hex-of-id>/<id>.git`
— identifier-derived, so provider names never reach the filesystem.

### D3 — Environment

The child environment is exactly:

- `PATH=<configured helper dir>:<system git dir>` (from config, not inherited wholesale);
- `HOME=<run dir>/home` (created empty, mode 0700);
- `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`;
- `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=/dev/null`, `SSH_ASKPASS=/dev/null`;
- `GIT_PAGER=cat`, `PAGER=cat` (non-tty pipes already disable pagers; this is belt-and-braces);
- `LANG=C.UTF-8` for deterministic diagnostics;
- when credentials are attached, `GIT_CONFIG_COUNT`-style `-c credential.helper=…` goes into argv,
  never into env.

Everything else is dropped; unit tests assert the exact map. Hook disabling uses
`-c core.hooksPath=/dev/null` (argv override beats repo config) plus `core.hooksPath` pointing at a
non-directory, which fails closed even if a future git changes semantics around empty values.

### D4 — Spawn, bounds, kill

One async function spawns `tokio::process::Command` with `process_group(0)` (child leads its own
group), piped stdout/stderr read through two bounded reader tasks (cap+1 detection), and a
`tokio::time::timeout` deadline. On deadline or cap overflow the child group receives SIGKILL via
`libc::kill(-pid, SIGKILL)`; the direct child is reaped before returning so no zombie survives.
Cancellation safety: dropping the future kills the group (kill-on-drop guard struct). Output caps
default to 256 KiB stdout / 128 KiB stderr; default deadline 300 s; both configurable in
`RunConfig`.

Timeout/cap behavior is tested against a probe binary (the test's own "git") rather than real Git:
`RunConfig.git_binary` points at trusted configuration, so tests may point it at `/usr/bin/env` or
a sleeping helper without weakening production allowlisting (the subcommand allowlist still gates
what may run).

### D5 — Credentials out of band

Per operation with credentials: create `<run-dir>/cred-<uuid>` mode `0600` inside the run directory
mode `0700`; write `username=<u>\npassword=<s>\n`; pass
`-c credential.helper=<helper-binary-path> <secret-file-path>` as two argv words after the helper
path (Git word-splits the helper value and execvp's it directly — no shell unless the value starts
with `!`, which we never produce). The shipped helper reads the file, prints the credential
protocol, exits. The secret file is deleted in a drop guard whether the run succeeds, times out, or
is cancelled. Trade-off documented honestly: the secret exists briefly as a `0600` file on disk
inside a `0700` directory instead of being fd-passed, because fd inheritance beyond stdio is not
expressible in this workspace under the `unsafe` ban; the file lives entirely inside Vault's
confined storage root.

Redaction keeps a set of active secret byte strings for the run; captured streams are scanned
before being returned and occurrences become `[redacted]`.

### D6 — Deterministic hostile fixtures

A committed generator module (`tests/support/fixtures.rs`) builds fixture repositories with fixed
author/committer timestamps and identifiers, so object ids are reproducible. Fixtures: hostile-ref
repo, large-blob repo (~8 MiB repeating pattern), promisor/partial clone (built with
`--filter=blob:none` against a local server-enabled source), hooked mirror (config +
reference-transaction hook writing a marker), plus URL/path validation cases needing no repository.
Fixture setup uses plain `std::process::Command` calling system Git directly - setup code is test
code, not subject of the suite; every *operation under test* goes through the runner.

## Testing strategy

Unit tests next to validators (URL, confinement, env, redaction). Integration tests under
`crates/gitrunner/tests/`: `bounds.rs` (probe-binary timeout/caps/env), `credentials.rs` (helper
protocol, structural secret absence), `hostile_repositories.rs` (the generated suite). Every RED
task runs its named test first and confirms the stated assertion failure, not a compile error.
Postgres is untouched; these tests need none.

## Risks and mitigations

- **TOCTOU in path checks** — narrowed by canonicalize-at-use and `0700` root ownership; full
  elimination would need openat2-style APIs unavailable safely here.
- **Probe-binary testing could mask allowlist drift** — production allowlist is a closed enum;
  compile-time construction means drift requires editing the enum, which the review gate sees.
- **Helper word-splitting** — helper paths with spaces would split; the helper ships at a
  workspace-controlled path and `RunConfig` documents the constraint, validated at config build.
