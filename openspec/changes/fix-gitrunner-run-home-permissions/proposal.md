## Why

`ci` / job `gate` fails on `main` (run [32992183307](https://github.com/po4yka/ratatoskr-vault/actions/runs/32992183307/job/98252201490)): `crates/gitrunner/tests/credentials.rs` panics in `leaked_secret_is_redacted_from_output` and `secret_material_absent_from_argv_and_environment` with `CredentialWriteFailed { reason: "run directory ... is group/world accessible" }`. `GitRunner::run` (`crates/gitrunner/src/lib.rs:154`) and `credentials::prepare_secret_file` (`crates/gitrunner/src/credentials.rs:49`) both create the per-run `HOME` with a bare `std::fs::create_dir_all`, leaving its permissions to whatever the ambient umask produces. `ubuntu-latest`'s default umask (`022`) yields mode `0755`, so the very next line's own group/world-accessibility check refuses the directory it just created, on every run.

This is a real defect, not a flaky test: the `git-execution` specification's "Out-of-band credentials" requirement already states the run directory holding staged credential material "admits no other users." The implementation only met that promise by accident, on hosts whose ambient umask happened to already be strict enough (this reviewer's own dev machine runs Rust test binaries under a locally sandboxed `umask 077`, which is why the bug does not reproduce there without deliberately overriding the umask). Under any host with the common `022` umask — including every `ubuntu-latest` GitHub Actions runner — every credentialed Git operation fails.

## What Changes

- Create the run-home directory with mode `0700` set at `mkdir` time (`DirBuilder::mode`), not derived from the ambient umask, at both places this crate creates it: `GitRunner::run`'s best-effort `HOME` creation and `credentials::prepare_secret_file`'s own creation of the same directory.
- Keep the existing group/world-accessibility check in `prepare_secret_file` as defense in depth: `DirBuilder` does not alter the permissions of an already-existing directory, so an operator-supplied run directory that existed beforehand with loose permissions is still refused rather than silently trusted or narrowed.
- Strengthen the `git-execution` spec's "Out-of-band credentials" requirement to say explicitly that the run directory's owner-only permissions must be established by the directory's own creation, not by depending on the ambient umask happening to already be strict enough — the gap the bug fell through.

## Capabilities

`git-execution`: the run directory backing out-of-band credential delivery is now owner-only by construction on creation, independent of the host's ambient umask, while remaining refused if the caller hands the runner an already-existing directory with wider permissions.

## Impact

- `crates/gitrunner/src/lib.rs` — `GitRunner::run`'s `HOME` creation.
- `crates/gitrunner/src/credentials.rs` — `prepare_secret_file`'s run-directory creation; adds a shared `create_private_dir_all` helper used by both sites.
- `openspec/specs/git-execution/spec.md` — "Out-of-band credentials" requirement gains an explicit umask-independence scenario.
- No wire format, schema, or public API signature changes; `Credentials`, `RunConfig`, and every public function keep their existing shapes.
