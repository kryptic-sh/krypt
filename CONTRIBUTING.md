# Contributing

Thanks for taking a look. The project is in early development — read the
phase milestones in [Issues](https://github.com/mxaddict/files/issues)
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

MSRV is **Rust 1.85** (`edition = "2021"` workspace, individual crates may
adopt `edition = "2024"` later).

## Repo layout

```
crates/
├── files-cli/        # bin: `files`
├── files-core/       # lib: engine
├── files-pkg/        # lib: package manager abstraction
└── files-platform/   # lib: cfg-gated OS abstractions
```

When in doubt, put new code in `files-core` and re-export through the CLI.
The CLI crate stays thin.

## Issues + PRs

- Pick an open issue, comment that you're starting.
- Branch off `main`, name like `feat/setup-wizard` or `fix/link-conflict`.
- Squash on merge unless commits are individually meaningful.
- Reference issues with `Closes #NN` in the body of the merge commit.

## License

By contributing, you agree your changes are MIT-licensed.
