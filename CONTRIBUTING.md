# Contributing

Thanks for taking a look. The project is in early development — read the
phase milestones in [Issues](https://github.com/kryptic-sh/krypt/issues)
to see where help fits.

## Commit style

This repo follows **[Conventional Commits](https://www.conventionalcommits.org/)**.

```
type(scope): short summary

Longer body explaining the WHY, not the WHAT.

Closes #123
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`, `perf`,
`ci`, `build`. Scope is optional.

Examples:

- `feat(core): implement path variable resolver`
- `fix(cli): exit non-zero when subcommand fails`
- `docs: clarify XDG escape-hatch usage`
- `ci: add windows runner to test matrix`

Breaking changes: append `!` after type (`feat(core)!: ...`) and explain
in the body.

## Development

```sh
# Build everything
cargo build

# Run the CLI
cargo run -- version

# Format + lint + test before pushing
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

MSRV is **Rust 1.88** (`edition = "2021"` workspace, individual crates may
adopt `edition = "2024"` later).

## Repo layout

```
crates/
├── krypt-cli/        # bin: `krypt`
├── krypt-core/       # lib: engine
├── krypt-pkg/        # lib: package manager abstraction
└── krypt-platform/   # lib: cfg-gated OS abstractions
```

When in doubt, put new code in `krypt-core` and re-export through the CLI.
The CLI crate stays thin.

## Issues + PRs

- Pick an open issue, comment that you're starting.
- Branch off `main`, name like `feat/setup-wizard` or `fix/link-conflict`.
- Squash on merge unless commits are individually meaningful.
- Reference issues with `Closes #NN` in the body of the merge commit.

## Release secrets

The release workflow publishes to three external destinations. Secrets are
provisioned **at the kryptic.sh org level** and inherited by this repo:

| Secret                 | Used by          | Visibility                          |
| ---------------------- | ---------------- | ----------------------------------- |
| `CARGO_REGISTRY_TOKEN` | `publish-crates` | org-wide                            |
| `AUR_SSH_KEY`          | `aur-bin`        | selected repos (krypt is allowlisted) |
| `BREW_SSH_KEY`         | `brew-tap`       | selected repos (krypt is allowlisted) |

Org admins manage these at `kryptic-sh/.github` org settings. Individual repo
contributors don't need to do anything.

The first tagged release will:

1. Build all 6 target archives + sha256 sidecars
2. Create a GitHub Release
3. Publish all 4 workspace crates to crates.io (idempotent — skips already-published versions)
4. Render PKGBUILD + push to `aur.archlinux.org/krypt-bin.git`
5. Render Homebrew formula + push to `kryptic-sh/homebrew-tap@main`

If any step fails, retry with `gh workflow run release.yml --ref v<version>`.

## License

By contributing, you agree your changes are MIT-licensed.
