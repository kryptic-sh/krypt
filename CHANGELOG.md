# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

### Added

- `krypt update [--dry-run] [--skip-hooks] [--force]` subcommand — pulls the
  dotfiles repo via fast-forward using gix (no system `git` required), re-runs
  `link` to deploy any new files, and warns to stderr if the binary is older
  than `[meta] krypt_min` in the config (#17). Errors immediately on a dirty
  working tree with an actionable message. Post-update hooks are deferred — a
  warning is printed when any are configured (#43).
- `krypt_core::update` module: `UpdateOpts`, `UpdateReport`, `UpdateError`,
  `update` — pure orchestration for the pull → version-check → link pipeline.

- `krypt init [URL] [--from <url>] [--bare] [--force] [--repo-path <path>]`
  subcommand — clones a dotfiles repo into `${XDG_CONFIG}/krypt/repo` (or a
  custom path) and writes a tool config at `${XDG_CONFIG}/krypt/config.toml`
  with `[repo] path` and optional `url`. Cloning uses gix with rustls — no
  system `git` required, no OpenSSL. Only HTTPS URLs are supported (gix 0.83 has
  no SSH transport). `--bare` creates an empty `.krypt.toml` stub instead of
  cloning. `--force` wipes an existing repo path before proceeding (#14).
- `krypt_core::tool_config` module: `ToolConfig`, `RepoConfig`,
  `ToolConfigError` — TOML-backed tool config with atomic save +
  `deny_unknown_fields`.
- `krypt_core::init` module: `InitOpts`, `InitReport`, `InitError`, `init` —
  pure orchestration using gix for cloning (no system `git` dependency).
- Deployment manifest at `${XDG_STATE}/krypt/manifest.json`: versioned JSON
  schema, atomic write, SHA-256 hashes per entry, and drift detection comparing
  recorded hashes to current destination contents (#13).
- `krypt diff` CLI subcommand — reports each manifest entry as `clean`,
  `drifted`, or `missing`; exits non-zero when any entry is dirty.
- `krypt_core::manifest` module: `Manifest`, `ManifestEntry`, `hash_file`,
  `detect_drift`, `DriftStatus`, `DriftRecord`.
- `krypt link` / `krypt unlink` / `krypt relink` CLI subcommands — idempotent
  deploy of every entry in `.krypt.toml`, manifest-aware conflict narrowing
  (re-writes against your own deploys are silent; foreign conflicts are skipped
  unless `--force`), drift-safe unlink (`--force` to delete edited files), and a
  `relink` convenience that chains both (#15). All three accept `--dry-run` and
  `--platform <linux|macos|windows>` for cross-platform testing.
- `krypt_core::deploy` module: `DeployOpts`, `LinkReport`, `UnlinkReport`,
  `link`, `unlink`, `relink`, `DeployError`.

### Changed

- `krypt update` and `krypt init` now use
  [gix (gitoxide)](https://github.com/Byron/gitoxide) as the sole git backend —
  no system `git` binary, no `git2`/`libgit2`. Only HTTPS URLs are supported for
  `krypt init --from <url>`; SSH URLs require a manual `git clone` first.
  Auto-stash and `--no-stash` removed pending gix stash support (#44).
- `copy::Report.written` is now a `Vec<Written>` carrying per-file
  `(src, dst, kind, hash_src, hash_dst)`. Old `usize` counts are available via
  `Report::written_count()`.
- `copy::EntryKind` now serializes as lowercase JSON for manifest storage.

## [0.0.2] - 2026-05-15

### Added

- Path variable resolver with XDG defaults and platform-specific escape hatches
  (#11).
- Include directive expansion in `.krypt.toml` (#10).
- Copy engine: plan generation and atomic deploy (#12).
- Published workspace bin as `krypt-cli` on crates.io (the `krypt` crate name is
  held by an unrelated stale project — see
  [#37](https://github.com/kryptic-sh/krypt/issues/37)). The installed binary is
  still named `krypt`.
- Lib crates `krypt-core`, `krypt-pkg`, `krypt-platform` published at 0.0.2.
- AUR package `krypt-bin` and Homebrew formula available as install channels.

### Changed

- Repo and org renamed to `kryptic-sh/krypt` (was `files-*` in a brief
  pre-rename window; those artifacts were deleted before public publish).

## [0.0.1] - 2026-05-15

### Added

- Cargo workspace scaffold with four-crate split: `krypt-cli`, `krypt-core`,
  `krypt-pkg`, `krypt-platform`.
- CI pipeline: `cargo fmt`, `cargo clippy`, `cargo test`.
- Release pipeline: cross-compile matrix producing binaries for Linux, macOS,
  and Windows, uploaded to GitHub Releases.
- `.krypt.toml` schema parser (#9).

> **Note:** release artifacts from this version were named `files-*` during a
> brief pre-rename window and were deleted before public publish. The first
> publicly visible release is 0.0.2.
