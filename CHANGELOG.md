# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/) once it reaches
0.1.0; the 0.0.x series is a churn phase where breaking changes may land on
patch bumps.

## [Unreleased]

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
