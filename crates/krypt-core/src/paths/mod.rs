//! Path variable resolution.
//!
//! Translates `${VAR}` / `${env:VAR}` / `${env:VAR:-fallback}` expressions
//! into concrete paths. Used to expand `dst` and other path-like fields in
//! `.krypt.toml` once the platform + user overrides are known.
//!
//! See the [project README](https://github.com/kryptic-sh/krypt) for the
//! full variable reference. Brief recap:
//!
//! | Var                  | Resolution                                |
//! | -------------------- | ----------------------------------------- |
//! | `${HOME}`            | `$HOME` (Unix), `%USERPROFILE%` (Windows) |
//! | `${XDG_CONFIG}`      | `$XDG_CONFIG_HOME` or `${HOME}/.config`   |
//! | `${XDG_DATA}`        | `$XDG_DATA_HOME` or `${HOME}/.local/share` |
//! | `${XDG_STATE}`       | `$XDG_STATE_HOME` or `${HOME}/.local/state` |
//! | `${XDG_CACHE}`       | `$XDG_CACHE_HOME` or `${HOME}/.cache`     |
//! | `${XDG_RUNTIME}`     | `$XDG_RUNTIME_DIR` or platform fallback   |
//! | `${LOCAL_BIN}`       | `${HOME}/.local/bin`                      |
//! | `${DOCUMENTS}`       | `${HOME}/Documents`                       |
//! | `${WIN_LOCALAPPDATA}` | Windows only — errors elsewhere          |
//! | `${WIN_APPDATA}`     | Windows only — errors elsewhere           |
//! | `${MAC_LIBRARY}`     | macOS only — errors elsewhere             |
//! | `${env:VAR}`         | Reads env var (empty if unset)            |
//! | `${env:VAR:-FB}`     | Reads env var, falls back to FB if unset  |
//!
//! User overrides live in the `[paths]` section of `.krypt.toml` and shadow
//! built-in defaults. Override values may themselves contain `${...}`
//! expressions, resolved recursively (with cycle detection).

mod platform;
mod resolve;

pub use platform::Platform;
pub use resolve::{ResolveError, Resolver};
