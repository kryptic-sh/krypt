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
    fn command_spawns_a_program_from_path() {
        let out = command("git").arg("--version").output().expect("spawn git");
        assert!(out.status.success(), "git exited with {}", out.status);
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
    }
}
