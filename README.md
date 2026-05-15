# files

Cross-platform dotfiles manager. Rust binary. Config-driven.

> Status: **early development**. v0.0.1 is a scaffolding release — only
> `files --version` works today. The roadmap lives in
> [GitHub Issues](https://github.com/mxaddict/files/issues) organized into
> phase milestones.

## What it will do

- One binary to manage your dotfiles end-to-end on Linux, macOS, and Windows.
- Replaces stow (copy-based deploy with manifest-tracked drift detection).
- Replaces ad-hoc bash scripts via a declarative `.files.toml` and a step runner.
- Interactive first-run wizard that fills in user-specific values.
- Cross-distro / cross-platform package install abstraction.

## Install

Not yet published. Once v0.1+ ships:

```sh
cargo install files
# or
paru -S files-bin            # Arch
brew install mxaddict/files  # macOS
scoop install files          # Windows
nix run github:mxaddict/files
```

## Quickstart (planned API)

```sh
files init https://github.com/mxaddict/dotfiles   # clone repo to XDG path
files setup                                       # interactive wizard
files link                                        # deploy
files update                                      # daily-driver: pull + redeploy
files doctor                                      # diagnostic
```

## Architecture

Four-crate Cargo workspace:

| Crate            | Role                                                                |
| ---------------- | ------------------------------------------------------------------- |
| `files-cli`      | Binary (`files`) — clap dispatch, thin                              |
| `files-core`     | Engine: config parser, path resolver, copy engine, manifest, runner |
| `files-pkg`      | Package manager abstraction (pacman, apt, brew, scoop, winget, dnf) |
| `files-platform` | OS-specific abstractions (cfg-gated)                                |

## License

MIT. See [LICENSE](LICENSE).
