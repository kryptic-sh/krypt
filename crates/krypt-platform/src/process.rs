//! Spawning programs by name the same way on every OS.
//!
//! `krypt` decides whether a program exists with [`which`], which honours
//! `PATHEXT` on Windows and so finds `.cmd` / `.bat` shims — how npm global
//! tools and scoop install themselves. [`std::process::Command::new`] does its
//! own lookup that only tries `<name>.exe` there, so a program could pass
//! `command_exists:` and then fail to spawn with "program not found".
//! [`command`] closes that gap.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

/// Build a [`Command`] for `program`.
///
/// On Windows a bare program name is resolved through `PATH` and `PATHEXT`
/// with [`resolve_program`] before the command is built, so script shims
/// spawn. Elsewhere the OS lookup (`execvp`) already searches `PATH` exactly
/// as [`which`] does, so the name is passed through untouched and the child
/// keeps its usual `argv[0]`.
///
/// The lookup uses this process's `PATH`, read when `command` is called; a
/// `PATH` set later on the returned [`Command`] does not change which program
/// it runs on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let program = program.as_ref();
    if cfg!(windows) {
        Command::new(resolve_program(program, std::env::var_os("PATH")))
    } else {
        Command::new(program)
    }
}

/// Build a [`Command`] for `program` as [`command`] does, but looked up in
/// `path` instead of this process's `PATH`, and with `path` as the child's
/// `PATH`, so what the child runs in turn is found there as well.
pub fn command_in_path(program: impl AsRef<OsStr>, path: &OsStr) -> Command {
    let program = program.as_ref();
    let mut command = if cfg!(windows) {
        Command::new(resolve_program(program, Some(path.to_owned())))
    } else {
        Command::new(program)
    };
    command.env("PATH", path);
    command
}

/// `current`, a `PATH`-style list, with each directory of `extra` that it does
/// not already name appended, in order. Directories compare as Windows
/// compares them there (case and a trailing separator ignored), and exactly
/// elsewhere.
pub fn append_new_paths<'a>(current: &OsStr, extra: impl IntoIterator<Item = &'a str>) -> OsString {
    fn key(dir: &Path) -> String {
        let dir = dir.to_string_lossy();
        if cfg!(windows) {
            dir.trim_end_matches(['\\', '/']).to_lowercase()
        } else {
            dir.into_owned()
        }
    }
    let mut dirs: Vec<std::path::PathBuf> = std::env::split_paths(current).collect();
    let mut seen: std::collections::HashSet<String> = dirs.iter().map(|d| key(d)).collect();
    for list in extra {
        for dir in std::env::split_paths(list) {
            if !dir.as_os_str().is_empty() && seen.insert(key(&dir)) {
                dirs.push(dir);
            }
        }
    }
    // Every directory came out of `split_paths`, which strips what
    // `join_paths` rejects (the separator on Unix, `"` on Windows).
    std::env::join_paths(dirs).expect("directories from split_paths rejoin")
}

/// Resolve a bare program name against `paths`, a `PATH`-style list, using
/// the same rules as `command_exists:` (including `PATHEXT` on Windows).
///
/// Anything that is not a bare name (`./tool`, `C:\bin\tool.exe`) and any name
/// that is not found is returned unchanged, so spawning it reports the
/// ordinary not-found error.
pub fn resolve_program(program: &OsStr, paths: Option<OsString>) -> OsString {
    if Path::new(program).components().count() != 1 {
        return program.to_owned();
    }
    // `which_in` only consults the working directory for names containing a
    // path separator, which were returned above.
    match which::which_in(program, paths, Path::new(".")) {
        Ok(path) => path.into_os_string(),
        Err(_) => program.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const SHIM_OUTPUT: &str = "krypt-shim-ok";

    /// Held by every test here that writes a shim or spawns a process. On
    /// Linux a child forked while another thread still has a shim open for
    /// writing inherits that descriptor until it execs, and running the shim
    /// meanwhile fails with "Text file busy" (ETXTBSY).
    static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
        // A test that panicked while holding it leaves nothing to clean up.
        SPAWN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Write a script shim named `name` into `dir` that prints [`SHIM_OUTPUT`].
    fn write_shim(dir: &Path, name: &str) {
        #[cfg(windows)]
        fs::write(
            dir.join(format!("{name}.cmd")),
            format!("@echo off\r\necho {SHIM_OUTPUT}\r\n"),
        )
        .expect("write cmd shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = dir.join(name);
            fs::write(&script, format!("#!/bin/sh\necho {SHIM_OUTPUT}\n")).expect("write shim");
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
        }
    }

    #[test]
    fn resolves_a_script_shim_that_then_spawns() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        write_shim(dir.path(), "krypt-shim");

        let resolved = resolve_program(
            OsStr::new("krypt-shim"),
            Some(dir.path().as_os_str().to_owned()),
        );
        assert_eq!(
            Path::new(&resolved).parent(),
            Some(dir.path()),
            "resolved to {resolved:?}"
        );

        let out = Command::new(&resolved).output().expect("spawn shim");
        assert!(out.status.success(), "shim exited with {}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), SHIM_OUTPUT);
    }

    #[test]
    fn unresolvable_names_and_paths_pass_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Some(dir.path().as_os_str().to_owned());

        let missing = OsStr::new("krypt-no-such-program");
        assert_eq!(resolve_program(missing, paths.clone()), missing);

        let relative = OsStr::new("./krypt-shim");
        assert_eq!(resolve_program(relative, paths), relative);
    }

    #[test]
    fn append_new_paths_adds_only_unseen_dirs_in_order() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let (a, b, c) = if cfg!(windows) {
            (r"C:\a", r"C:\b", r"C:\c")
        } else {
            ("/a", "/b", "/c")
        };
        let current = OsString::from([a, b].join(sep));
        let extra = [c, a, "", b].join(sep);
        assert_eq!(
            append_new_paths(&current, [extra.as_str()]),
            OsString::from([a, b, c].join(sep))
        );
    }

    #[cfg(windows)]
    #[test]
    fn append_new_paths_ignores_case_and_trailing_separator_on_windows() {
        let current = OsString::from(r"C:\Users\Me\scoop\shims");
        assert_eq!(
            append_new_paths(&current, [r"c:\users\me\SCOOP\shims\;C:\new"]),
            OsString::from(r"C:\Users\Me\scoop\shims;C:\new")
        );
    }

    #[test]
    fn command_in_path_finds_and_passes_on_the_given_path() {
        let _spawning = spawn_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        write_shim(dir.path(), "krypt-shim-in-path");
        let out = command_in_path("krypt-shim-in-path", dir.path().as_os_str())
            .output()
            .expect("spawn shim from the given path");
        assert!(out.status.success(), "shim exited with {}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), SHIM_OUTPUT);
    }

    #[test]
    fn command_spawns_a_program_from_path() {
        let _spawning = spawn_lock();
        let out = command("git").arg("--version").output().expect("spawn git");
        assert!(out.status.success(), "git exited with {}", out.status);
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
    }
}
