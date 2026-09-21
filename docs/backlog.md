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

### CI lints on latest stable, not the pinned toolchain

The `clippy` and `test` jobs pass `toolchain: stable` to `setup-rust-toolchain`,
which runs `rustup override` and so replaces `rust-toolchain.toml` (rustc 1.98.1
on 2026-09-15, against a 1.95.0 pin and `rust-version = "1.95"`). The `distro`
job now runs the test suite on the pinned toolchain in four Linux containers,
and `build` compiles it for every release target, but clippy and the macOS and
Windows tests never run on it. Options: a 1.95.0 clippy leg, or dropping the
override (loses the early warning from new stable lints).

### Windows notifications use Windows PowerShell 5.1

`notify::command_for(NotifyBackend::PowerShell, ..)` spawns `powershell`, the
5.1 that ships with Windows, not PowerShell 7 (`pwsh`). The mxaddict dotfiles
standardise on PowerShell 7. `System.Windows.Forms` is available in both on
Windows, so `pwsh` first with a `powershell` fallback would work; it ties into
the MessageBox-versus-toast choice above.

## Bugs

### `krypt update --dry-run` auto-stashes

Seen on Windows: `krypt update --dry-run` in a dotfiles checkout with local
changes failed with `auto-stash push failed: ...`, so the dry run was about to
stash. `--dry-run` is documented as "show plan, change nothing". Not traced in
`update` yet.

### Auto-stash fails on git symlinks checked out as files

The same run failed reading `.claude-work/CLAUDE.md`, a symlink in git that Git
for Windows (`core.symlinks = false`) checks out as a small text file holding
the target path. gix's stash reads the worktree entry as a symlink and errors,
so `krypt update` cannot stash in such a repo on Windows. The mxaddict dotfiles
are dropping their symlinks, which hides this there; any other repo with
symlinks still hits it.

### Programs installed mid-run are not on krypt's `PATH`

Scoop and winget packages that add a directory to the user `PATH` (`mingw`,
`rustup`'s `~/.cargo/bin`) change the registry, not the running process, so a
later step in the same `krypt deps` — a `cargo:` entry after `rustup` — fails
with `program not found`. Scoop's shims avoid this for most apps. Options:
re-read the user and machine `PATH` from the registry after each Windows
install, or document running `krypt deps` twice.

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

- **Scoop's behaviour is pinned by hand, not by CI.** Everything `Scoop` relies
  on — exit 0 on every failure, `scoop export` JSON, `scoop cat` printing JSON
  only for a known app, one unknown app aborting a multi-app `scoop install` —
  was observed against Scoop 0.5.3 on one Windows 11 machine and is encoded in
  mocks. No CI job installs scoop, so a scoop release that changes any of it
  goes unnoticed. The `Install failed` filter in `scoop::export` comes from
  reading `scoop-list.ps1`, not from a failed install.
- **Untrusted Homebrew taps in `--check`.** Homebrew ignores formulae from a tap
  until `brew trust --tap <tap>`. On Linuxbrew 4.6.20 `brew tap` of an untrusted
  tap exits 1, so `Brew::exists` reports its packages missing; on the 2026-09-15
  macOS runner the same check found them, while `brew install` would have
  skipped them. Not reproduced on a Mac; a trust check (e.g.
  `brew trust --json`) would make the answer independent of the brew version.
- **Brew casks as installed.** `Brew::is_installed` runs `brew list --versions`
  for casks too; verified by the dotfiles deps workflow's idempotent `core`
  install on macOS (Alacritty is a cask), not by a krypt test.
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
