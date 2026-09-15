# Backlog

Work raised and not finished, decisions still open, and gaps in what has been
verified. Delete an entry when it ships; `git log` keeps the history.

## Decisions needed

### `krypt <group> <name>` ignores `[meta] notify_backend`

`dispatch::run_in_group` builds its notifier with
`AutoNotifier::with_backend(NotifyBackend::Stderr)`, so `notify` steps in a
`[[command]]` always print to stderr. `krypt update` hooks build theirs with
`AutoNotifier::new(meta.notify_backend)` and do raise desktop notifications.
Nothing in the README or the schema docs says commands differ.

- **Honour the setting in dispatch too.** Consistent, but see the next entry: on
  Windows `auto` picks a modal dialog, which would stop a command mid-run until
  someone clicks it.
- **Keep stderr and document it.** No surprise dialogs, but `notify` steps in
  commands never reach the desktop, whatever the config says.

Verified by reading both call sites; the original dispatcher commit (`f9a207a`)
introduced the stderr pin without a stated reason.

### Windows notify backend is a blocking `MessageBox`

`notify::command_for(NotifyBackend::PowerShell, ..)` runs
`[System.Windows.Forms.MessageBox]::Show`, a modal dialog that does not return
until dismissed, so a hook or step that notifies waits on the user. A toast
(WinRT `ToastNotificationManager` from Windows PowerShell 5.1, or the BurntToast
module the module docs already rejected) would not block. None of these can be
observed on a headless CI runner, so any choice is verified by hand on a
desktop.

### Steps cannot interact with the terminal

`runner::RealProcessExec` gives every step `stdin` from `/dev/null` (unless it
is a `pipe` step) and captures `stdout`/`stderr`. Output of a `run` step without
`capture` is discarded, and a program that prompts reads EOF. The mxaddict
dotfiles already declare such a command: `system clearkeys` wraps a script whose
comment says it "requires interactive stdin confirm". Options: inherit the
terminal for `run` steps that have no `capture` (changes what users see for
every command), or add a per-step opt-in such as `interactive = true` (schema
change).

### CI tests on latest stable, not the pinned toolchain

The `clippy` and `test` jobs pass `toolchain: stable` to `setup-rust-toolchain`,
which runs `rustup override` and so replaces `rust-toolchain.toml`: the
2026-09-15 run tested with rustc 1.98.1 while the workspace pins 1.95.0 and
declares `rust-version = "1.95"`. Since the `build` job now runs on every event,
the pinned toolchain is at least compiled for every release target, but clippy
and the test suite never run on it, so the MSRV claim is untested. Options: add
a 1.95.0 leg to the test matrix (cost: three more jobs), or drop the override
and test on the pinned toolchain only (loses the early warning from new stable
lints).

## Deferred

### Battery reading on macOS and Windows

`battery::default_reader` returns `UnsupportedReader` off Linux, so
`krypt battery report` / `log` only work there. Needs an IOKit reader (macOS)
and `GetSystemPowerStatus` (Windows); neither is observable on CI runners, which
have no battery.

### `krypt battery log` timestamps shell out to `date(1)`

`format_timestamp_local` runs `date "+%Y-%m-%d %H:%M:%S"` to match the old bash
script, and writes `epoch:<secs>` when that fails. Windows has no `date`
executable unless Git's `usr\bin` is on `PATH` (the GitHub runner has it, so CI
never sees the fallback). Formatting local time natively needs a time-zone
crate; `jiff` is already in `Cargo.lock` through `gix`, but depending on it
directly is a new direct dependency and needs a yes first. Low impact today,
since logging a battery reading only works on Linux.

## Unverified

- **Re-deploying over a read-only file on Windows.** `copy::copy_atomic`
  finishes with `fs::rename(tmp, dst)`. `fs::copy` carries the read-only
  attribute across, and renaming over a read-only destination fails on Windows,
  so a repo file marked read-only may deploy once and then fail on every later
  `link`. Inferred from the code; not reproduced.

## Coverage gaps

- **Not reviewed** beyond a scan for platform-specific code (`cfg`, direct env
  reads, process spawns, path separators): `update` (gix pull and auto-stash),
  `setup` template writers, `adopt`, `doctor`, `deploy` manifest handling,
  `include`.
- The `x86_64-apple-darwin`, `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-gnu` release targets are compiled by the `build` job
  but no tests run on them.
- `unix_mode_is_preserved` in `tests/copy_engine.rs` is Unix-only by nature
  (Windows has no mode bits); no Windows counterpart checks file attributes.
