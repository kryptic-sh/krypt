# Backlog

Work raised and not finished, decisions still open, and gaps in what has been
verified. Delete an entry when it ships; `git log` keeps the history.

## Decisions needed

### Windows release flagged by Defender; CI dependency cache suspect

Microsoft Defender quarantines the `x86_64-pc-windows-msvc` `krypt.exe` of
v0.6.0 and v0.6.1 as `Trojan:Win32/Bearfoos.B!ml` on write, before it runs
(signatures 1.463.266.0, 2026-11-13). Not yet known whether this is a false
positive or tampering. Established on 2026-11-13:

- The installed zip is what CI built for the tag: its SHA-256 matches
  `SHA256SUMS` on the release, the hash the release job logged, and
  scoop-bucket's `bucket/krypt.json`. The assets were never replaced after
  upload, and scoop-bucket's history for that manifest is `krypt-ci` release
  commits only.
- The source is clean: v0.4.4..v0.6.1 is the user's own commits (template edits
  kept on `link`, `paru` run unprivileged), the only lockfile change is tokio,
  and the `gix` fork pin (`094dce38`) is unchanged and signed.
- A local build of the v0.6.1 tag with a fresh `CARGO_HOME` scans clean. So do
  v0.4.4, v0.4.2 and the other kryptic-sh Windows releases on the same machine.
  Its imports match the release's apart from two CRT functions; strings differ
  only in embedded build paths.
- The release job compiled only the workspace crates. Every dependency was
  linked from the `Swatinem/rust-cache` entry
  `v0-rust-release-x86_64-pc-windows-msvc-build-Windows_NT-x64-71c74219-bbdc48aa`,
  saved on `refs/heads/main` by the run for `af89ef2` (2026-10-28). v0.4.4
  predates that entry; every flagged release was built from it.

The cache is the one input to the flagged binaries that was never built from
verified source in the open, and the `build` job also holds `contents: write`.
Options, in order of how much they settle:

- **Delete both release cache entries and rebuild the Windows target with no
  cache**, then scan the result. Clean means the cached artifacts carried the
  trigger; flagged means it is the CI toolchain or a classifier quirk.
- **Drop `rust-cache` from the `build` job permanently** so release binaries are
  always compiled from the lockfile; costs a few minutes per tag.
- **Submit the binary to Microsoft** as a false positive. Only once the rebuild
  says it is one.

Also open: the workflow pins third-party actions to moving tags
(`Swatinem/rust-cache@v2`, `softprops/action-gh-release@v3`,
`bnjbvr/cargo-machete@main`), and `build` has `contents: write` where `read`
would do.

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

### Auto-stash fails on git symlinks checked out as files

The same run failed reading `.claude-work/CLAUDE.md`, a symlink in git that Git
for Windows (`core.symlinks = false`) checks out as a small text file holding
the target path. gix's stash reads the worktree entry as a symlink and errors,
so `krypt update` cannot stash in such a repo on Windows. The mxaddict dotfiles
are dropping their symlinks, which hides this there; any other repo with
symlinks still hits it.

### `file://` clones need `git-upload-pack` on `PATH`

gix's local transport spawns `git-upload-pack`, so `krypt init <file://...>`
(and `init::tests::clone_from_local_file_url`) fails with "program not found"
where only `git` is on `PATH`. Scoop's git is such a case: it shims `git` but
not `git-upload-pack`, which sits in its `cmd` folder; Git for Windows' own
installer puts that folder on `PATH`. Seen on a Windows machine after moving
from winget's git to scoop's. Options: find `git-upload-pack` next to the `git`
on `PATH` (following a scoop shim), or document it. HTTPS clones are unaffected.

## Deferred

### `link` does not name the conflicts it skips

`krypt link` and `krypt update` print only a count ("conflicts skipped: N"), so
finding which destinations those are means re-deriving every entry's destination
by hand. `krypt diff` does not help for a destination krypt never recorded,
since it walks only the manifest. Listing each skipped `dst` (at least under
`--dry-run`) would make the count actionable.

### Pruning leaves empty destination directories behind

When a `[[link]]` entry is dropped from the repo, `link`/`update` prune its
deployed files but not the directories krypt created for them: retiring the
mxaddict dotfiles' `.config/swaync/**/*` link on 2026-11-02 left an empty
`~/.config/swaync/`. Removing a directory after a prune only when it is empty
and krypt created it would need the manifest to record created directories,
which it does not today.

### `krypt-core` and `krypt-cli` are not published to crates.io

The `publish-crates` job in `.github/workflows/ci.yml` publishes only
`krypt-platform` and `krypt-pkg`; the `krypt-core` / `krypt-cli` lines are
commented out because their `gix` dependency points at the `mxaddict/gitoxide`
fork for the `stash` and `merge` features, which crates.io's `gix` lacks, so
`cargo publish` rejects the manifest. The job still reports success. As of the
v0.5.0 release (2026-10-30), crates.io had `krypt-pkg` / `krypt-platform` at
0.5.0 and `krypt-core` / `krypt-cli` at 0.2.0, so `cargo install krypt-cli`
installs an old binary. AUR, brew, scoop and GitHub Releases are unaffected.
Unblocked by upstream GitoxideLabs/gitoxide#2603: repoint `gix` at crates.io and
restore the two `publish_or_skip` lines.

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
- **The Windows `PATH` refresh in `krypt deps` has not been seen end to end.**
  `RealRunner::refresh_path` is tested against the real registry (a runner
  started with only `System32` finds `powershell` after it), and `install_deps`
  is tested to refresh between an install and a later `cargo:` entry, but no
  real run has installed rustup through scoop and then built a `cargo:` entry.
  The refresh only appends: a directory an install removed from the registry
  `PATH` stays until krypt exits.
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

## Not bugs

- **`paru` reinstalls the old `krypt-bin` right after a release.** The `aur-bin`
  job pushes the new PKGBUILD to the AUR git repo, but the AUR RPC (and cgit)
  kept reporting the previous version for a while after v0.5.0, and
  `paru -S krypt-bin` trusted it and reinstalled 0.4.4.
  `paru -S --redownload --rebuild krypt-bin` refetches the PKGBUILD and gets the
  new one. To check what was actually pushed, read
  `git ls-remote https://aur.archlinux.org/krypt-bin.git`, not the RPC.

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
