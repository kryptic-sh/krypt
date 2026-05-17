# krypt

[![CI](https://github.com/kryptic-sh/krypt/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/krypt/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/krypt-cli.svg)](https://crates.io/crates/krypt-cli)
[![docs.rs](https://img.shields.io/docsrs/krypt-core)](https://docs.rs/krypt-core)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Cross-platform dotfiles manager. Rust binary. Config-driven.

A vault for your dotfiles — clone, deploy, and keep in sync across Linux, macOS,
and Windows. Part of the [kryptic.sh](https://kryptic.sh) suite.

## What it does

- Single binary manages dotfiles end-to-end on Linux / macOS / Windows.
- Replaces `stow` with copy-based deploy + manifest-tracked drift detection.
- Replaces ad-hoc bash orchestrators (`.update` / `.setup`) via a declarative
  `.krypt.toml` schema and a step runner with predicate gating.
- Interactive first-run wizard via `[prompts.*]` blocks.
- Cross-distro package install abstraction (pacman, paru, apt, dnf, brew, scoop,
  winget).
- Post-update lifecycle hooks with `command_exists:` / `platform:` / `env:` /
  `file_exists:` predicates.
- Generic `krypt <group> <name>` dispatcher — any `[[command]]` entry in
  `.krypt.toml` is reachable as a subcommand without binary changes.

## Install

```sh
paru -S krypt-bin                  # Arch (AUR)
brew install kryptic-sh/tap/krypt  # macOS
cargo install krypt-cli            # any platform
scoop install krypt                # Windows (planned)
nix run github:kryptic-sh/krypt    # Nix (planned)
```

Every channel installs a binary named `krypt` on your `$PATH`.

> The `krypt` crate name on crates.io is held by an unrelated 6-year-stale
> project — we publish the bin as `krypt-cli` for now. If/when the name
> transfers (see [#37](https://github.com/kryptic-sh/krypt/issues/37)),
> `cargo install krypt` will become the canonical install command.

## Quickstart

```sh
krypt init https://github.com/you/dotfiles   # clone repo to XDG path
krypt setup                                  # interactive wizard (prompts + deps)
krypt link                                   # deploy symlinks
krypt update                                 # daily: pull + redeploy + run hooks
krypt doctor                                 # diagnostic
```

Useful subcommands:

| Command                            | Effect                                     |
| ---------------------------------- | ------------------------------------------ |
| `krypt validate`                   | parse `.krypt.toml`, report schema errors  |
| `krypt diff`                       | show staged vs deployed differences        |
| `krypt adopt`                      | pull a hand-edited file back into the repo |
| `krypt unlink` / `relink`          | reverse / refresh symlinks                 |
| `krypt notify <title> <body>`      | platform-correct desktop notification      |
| `krypt menu`                       | list `[[command]] group = "menu"` entries  |
| `krypt menu <name>`                | run a menu's steps                         |
| `krypt <group> <name>`             | generic dispatcher for any group           |
| `krypt battery {report,log,clear}` | built-in battery state utility             |

## Migrating from stow + bash

If you have an existing stow-based dotfiles repo with `.update` / `.setup` bash
scripts and you want to convert it: see
[**docs/migrating-from-bash.md**](docs/migrating-from-bash.md). Step-by-step
walkthrough with the conceptual mapping (stow → `[[link]]`, `.update` →
`krypt update`, rofi launcher scripts → `[[command]]` entries, etc.).

Worked example: [mxaddict/dotfiles](https://github.com/mxaddict/dotfiles) —
Arch + Hyprland, ~70 symlinks, ~30 commands, ~10 post-update hooks.

## Architecture

Four-crate Cargo workspace:

| Crate            | Role                                                                                                 |
| ---------------- | ---------------------------------------------------------------------------------------------------- |
| `krypt-cli`      | Binary (`krypt`) — clap dispatch, thin                                                               |
| `krypt-core`     | Engine: schema, resolver, copy engine, manifest, runner, dispatch, predicate, hooks, notify, battery |
| `krypt-pkg`      | Package manager abstraction (pacman, apt, brew, scoop, winget, dnf)                                  |
| `krypt-platform` | OS-specific abstractions (cfg-gated)                                                                 |

## Status

- **v0.2.0** — Phase 2 wrap. Step runner + predicates + notify + post-update
  hooks + generic dispatcher + built-in `krypt battery`. See
  [CHANGELOG.md](CHANGELOG.md).
- Roadmap & open work: [issues](https://github.com/kryptic-sh/krypt/issues).

## License

MIT. See [LICENSE](LICENSE).
