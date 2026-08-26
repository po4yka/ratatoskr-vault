## Context

See [proposal.md](proposal.md). `crates/gitrunner` stages Git credentials as a `0600` file inside what the module doc for `credentials.rs` already calls "the owner-only run directory" — the design intent was never in doubt. The bug is purely in how that directory comes into existence: `std::fs::create_dir_all` requests mode `0o777` and lets the kernel apply the caller's umask, so the resulting mode is whatever `0o777 & !umask` happens to be. `prepare_secret_file` then reads that mode back and refuses to proceed if it is group- or world-accessible. On a `022` umask (the Linux default, and `ubuntu-latest`'s) that produces `0755`, which the very check the function just ran trips on — so credentialed operations fail unconditionally on the runners CI actually uses.

## Goals / Non-Goals

**Goals:**

- Make the run directory owner-only (`0700`) at the moment of creation, on every umask a real deployment or CI runner might have, without a race window where it briefly exists at a wider mode.
- Preserve the existing behavior for a run directory an operator (or a previous crashed run) left behind with looser permissions: still refuse rather than silently narrow it, since this crate cannot know why that directory looks the way it does.
- Touch only the two sites in `crates/gitrunner` that create a directory able to hold or sit above credential material. `ConfinedPath` (`confine.rs`) never creates directories in production code — it only resolves and validates paths already expected to exist — so it is out of scope.

**Non-Goals:**

- Change the secret-file-level protection (`0600`, set via `set_permissions` after `std::fs::write`); that path is unaffected by this bug and untouched.
- Add a general-purpose "create any private directory" utility beyond this crate's own two call sites; `create_private_dir_all` is `pub(crate)` and lives next to the one property (owner-only creation for credential-adjacent directories) it exists to guarantee.

## Decisions

**`DirBuilder::mode(0o700)` at creation time, not `create_dir_all` followed by `set_permissions`.** Both close the umask-dependent window if applied unconditionally, but `set_permissions` run unconditionally after every `create_dir_all` call — including on a directory that already existed — would silently overwrite the permissions of a pre-existing operator-supplied directory before the group/world check ever saw its real mode, defeating the "defense in depth" property the task explicitly asks to keep. `DirBuilder`'s recursive `create` only applies the requested mode to directories it actually creates; an already-existing directory is left exactly as it was, which is precisely the behavior the check downstream depends on to still catch a bad pre-existing directory.

**Mode `0o700` cannot be widened by any sane ambient umask.** `mkdir`'s effective mode is `requested & !umask`; umask can only *clear* bits, never set them. Since `0o700` already has zero bits in the group/other octals, no umask — `022`, `002`, `077`, even `000` — can add group or world access back. This was verified directly: a `DirBuilder::new().mode(0o700)` creation was exercised under umask `000`, `002`, `022`, and `077` and produced mode `700` in every case (see tasks.md §3).

**One shared helper, not two independent fixes.** `GitRunner::run` and `credentials::prepare_secret_file` both create the same path (`RunConfig.run_home`) for the same reason. A single `pub(crate) fn create_private_dir_all` in `credentials.rs`, called from both sites, means the umask-independence property has one place to be correct rather than two copies that could drift.

## Risks / Trade-offs

- [A future call site adds a third directory creation for run-adjacent state without routing through `create_private_dir_all`] → mitigated by keeping the helper colocated with the one module (`credentials.rs`) that owns the property, and by the module doc already stating the owner-only-directory invariant; a reviewer touching this file sees both.
- [Non-Unix targets get no mode guarantee, only a plain recursive create] → this crate already gates every other permission decision (secret file mode, the group/world check itself) behind `#[cfg(unix)]`; Vault's deployment target is Linux (Pi hardware, per `DEVELOPMENT.md`), and this change does not newly weaken anything unix does today.

## Migration Plan

No data or rollout migration: this is a same-process, same-request behavior change inside a runner that materializes credentials fresh on every call and deletes them on drop. Merging the fix is sufficient; the next `gate` run on `main` is the verification. Reverting the single commit restores the previous (unconditionally failing on `022`-umask hosts) behavior with no state to unwind.
