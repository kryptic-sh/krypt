# krypt

Cross-platform dotfiles manager. Rust binary. Config-driven.

A vault for your dotfiles — clone, deploy, and keep in sync across Linux,
macOS, and Windows. Part of the [kryptic.sh](https://kryptic.sh) suite.

> Status: **early development**. v0.0.1 is a scaffolding release — only
> `krypt --version` works today. The roadmap lives in
> [GitHub Issues](https://github.com/kryptic-sh/krypt/issues) organized into
> phase milestones.

## What it will do

- One binary to manage your dotfiles end-to-end on Linux, macOS, and Windows.
- Replaces stow (copy-based deploy with manifest-tracked drift detection).
- Replaces ad-hoc bash scripts via a declarative `.krypt.toml` and a step runner.
- Interactive first-run wizard that fills in user-specific values.
- Cross-distro / cross-platform package install abstraction.

## Install

```sh
cargo install krypt-cli            # any platform
paru -S krypt-bin                  # Arch (AUR)
brew install kryptic-sh/tap/krypt  # macOS
scoop install krypt                # Windows (planned)
nix run github:kryptic-sh/krypt    # Nix (planned)
```

Every channel installs a binary named `krypt` on your `$PATH`.

> The `krypt` crate name on crates.io is held by an unrelated 6-year-stale
> project — we publish the bin as `krypt-cli` for now. If/when the name
> transfers (see [#37](https://github.com/kryptic-sh/krypt/issues/37)),
> `cargo install krypt` will become the canonical install command.

## Quickstart (planned API)

```sh
krypt init https://github.com/you/dotfiles   # clone repo to XDG path
krypt setup                                  # interactive wizard
krypt link                                   # deploy
krypt update                                 # daily-driver: pull + redeploy
krypt doctor                                 # diagnostic
```

## Architecture

Four-crate Cargo workspace:

| Crate            | Role                                                                |
| ---------------- | ------------------------------------------------------------------- |
| `krypt-cli`      | Binary (`krypt`) — clap dispatch, thin                              |
| `krypt-core`     | Engine: config parser, path resolver, copy engine, manifest, runner |
| `krypt-pkg`      | Package manager abstraction (pacman, apt, brew, scoop, winget, dnf) |
| `krypt-platform` | OS-specific abstractions (cfg-gated)                                |

## License

MIT. See [LICENSE](LICENSE).
