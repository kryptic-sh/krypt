# krypt

Cross-platform dotfiles manager. Rust binary. Config-driven.

[![CI](https://github.com/kryptic-sh/krypt/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/krypt/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/kryptic-sh/krypt)](https://github.com/kryptic-sh/krypt/releases)
[![crates.io](https://img.shields.io/crates/v/krypt-cli.svg)](https://crates.io/crates/krypt-cli)
[![docs.rs](https://img.shields.io/docsrs/krypt-core)](https://docs.rs/krypt-core)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A vault for your dotfiles — clone, deploy, and keep in sync across Linux, macOS,
and Windows. Part of the [kryptic.sh](https://kryptic.sh) suite.

## What it does

- Single binary manages dotfiles end-to-end on Linux / macOS / Windows.
- Replaces `stow` with a **copy**-based deploy: files are copied from the repo
  to their destinations (mtime and, on Unix, file mode preserved). krypt never
  creates symlinks. A manifest records a sha256 of every destination so drift is
  detectable.
- Replaces ad-hoc bash orchestrators (`.update` / `.setup`) via a declarative
  `.krypt.toml` schema and a step runner with predicate gating.
- Interactive first-run wizard via `[prompts.*]` blocks.
- Cross-distro package install abstraction (pacman, apt, dnf, brew, scoop,
  winget).
- Post-update lifecycle hooks with `command_exists:` / `platform:` / `env:` /
  `file_exists:` predicates.
- Generic `krypt <group> <name>` dispatcher — any `[[command]]` entry in
  `.krypt.toml` is reachable as a subcommand without binary changes.

## Status

Pre-1.0 and moving. See [CHANGELOG.md](CHANGELOG.md) for what shipped in each
release, and the [releases page](https://github.com/kryptic-sh/krypt/releases)
for the current version. Roadmap and open work live in
[issues](https://github.com/kryptic-sh/krypt/issues).

## Install

```sh
paru -S krypt-bin                  # Arch (AUR)
brew install kryptic-sh/tap/krypt  # macOS
```

Windows — the release workflow renders a Scoop manifest into
[kryptic-sh/scoop-bucket](https://github.com/kryptic-sh/scoop-bucket):

```sh
scoop bucket add kryptic-sh https://github.com/kryptic-sh/scoop-bucket
scoop install krypt
```

Every tagged release also attaches prebuilt archives (plus `.sha256` sidecars)
for six targets: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`,
`x86_64-pc-windows-msvc`.

Every channel installs a binary named `krypt` on your `$PATH`.

> **crates.io trails this repo.** `cargo install krypt-cli` works but installs
> an older build. Publishing `krypt-core` and `krypt-cli` is currently paused:
> their `gix` dependency points at the `mxaddict/gitoxide` fork (for `stash` +
> `merge` plumbing), which `cargo publish` rejects. The `publish-crates` job in
> [`.github/workflows/ci.yml`](.github/workflows/ci.yml) has those two lines
> commented out and will be restored once the upstream gitoxide change lands.
> For the current version, use AUR / Homebrew / Scoop / a release archive.

> The `krypt` crate name on crates.io is held by an unrelated, unmaintained
> project (`Stupremee/krypt`) — we publish the bin as `krypt-cli` for now.
> If/when the name transfers (see
> [#37](https://github.com/kryptic-sh/krypt/issues/37)), `cargo install krypt`
> will become the canonical install command.

Building from source needs the toolchain pinned in
[`rust-toolchain.toml`](rust-toolchain.toml); the workspace MSRV is **Rust
1.95** (`rust-version` in `Cargo.toml`).

## Usage

First run, against a freshly cloned repo:

```sh
krypt init https://github.com/you/dotfiles   # clone to ${XDG_CONFIG}/krypt/repo
cd ~/.config/krypt/repo
krypt deps                                   # install [[deps]] packages
krypt setup                                  # interactive [prompts.*] wizard
krypt link                                   # deploy: copy files to destinations
```

Then, day to day:

```sh
krypt update   # pull the repo, re-run link, run post-update hooks
krypt doctor   # diagnostic
```

Notes on that sequence:

- `krypt deps`, `krypt setup`, and `krypt link` read `.krypt.toml` from the
  **current directory** by default — hence the `cd`. Pass
  `--config <repo>/.krypt.toml` to run them from elsewhere. (`krypt setup` also
  falls back to the repo path recorded in the tool config.)
- `krypt update` finds the repo through the tool config written by `init`, so it
  needs no flag.
- **`krypt setup` only runs the wizard.** It reads `[prompts.*]` sections, asks
  the questions, and writes the answers to the `[[template]]` destinations that
  name those sections. It does not install packages and does not deploy — run
  `krypt deps` and `krypt link` yourself.
- `krypt init` clones over HTTPS only; SSH remotes are not supported
  (`krypt init --help`).

Useful subcommands:

| Command                            | Effect                                                            |
| ---------------------------------- | ----------------------------------------------------------------- |
| `krypt validate`                   | parse `.krypt.toml`, report schema errors                         |
| `krypt paths`                      | print every resolved `${VAR}` for this host                       |
| `krypt diff`                       | compare deployed files to the manifest: clean / drifted / missing |
| `krypt adopt <path>`               | import an existing file into the repo, print a `[[link]]` block   |
| `krypt adopt-edits`                | copy hand-edits on drifted destinations back into the repo        |
| `krypt unlink` / `relink`          | delete manifest-tracked destinations / unlink then link again     |
| `krypt notify <title> <body>`      | platform-correct desktop notification                             |
| `krypt menu`                       | list `[[command]] group = "menu"` entries                         |
| `krypt menu <name>`                | run a menu's steps                                                |
| `krypt <group> <name>`             | generic dispatcher for any group                                  |
| `krypt battery {report,log,clear}` | built-in battery state utility                                    |

`adopt` and `adopt-edits` are different tools: `adopt <path>` brings a file that
krypt does not manage yet into the repo, while `adopt-edits` walks the manifest
and pulls edits you made in place on already-deployed files back to their repo
sources.

## Migrating from stow + bash

If you have an existing stow-based dotfiles repo with `.update` / `.setup` bash
scripts and you want to convert it: see
[**docs/migrating-from-bash.md**](docs/migrating-from-bash.md). Step-by-step
walkthrough with the conceptual mapping (stow → `[[link]]`, `.update` →
`krypt update`, rofi launcher scripts → `[[command]]` entries, etc.).

Worked example: [mxaddict/dotfiles](https://github.com/mxaddict/dotfiles) —
Arch + Hyprland, driven end-to-end from one `.krypt.toml`.

## Architecture

Four-crate Cargo workspace:

| Crate            | Role                                                                                                                                                  |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `krypt-cli`      | Binary (`krypt`) — clap dispatch, thin                                                                                                                |
| `krypt-core`     | Engine: config, paths, include, copy, manifest, deploy, tool_config, init, update, adopt, doctor, setup, runner, predicate, notify, dispatch, battery |
| `krypt-pkg`      | Package manager abstraction (pacman, apt, dnf, brew, scoop, winget)                                                                                   |
| `krypt-platform` | Placeholder for cfg-gated OS abstractions — currently exposes only a version constant                                                                 |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) or open an issue / PR.

## License

MIT. See [LICENSE](LICENSE).
