//! Stable, per-user launcher for Native Messaging, seafile://, and autostart.
//! The launcher itself is not updated in-place while running. An atomic
//! current.json selects a versioned pair of trusted native-host/desktop binaries.
use anyhow::{bail, Context, Result};
use cloudfile_core::update::{self, Binary};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn delegate(args: &[String]) -> Binary {
    match args.first().map(String::as_str) {
        Some("--autostart" | "--desktop") | None => Binary::Desktop,
        _ => Binary::NativeHost,
    }
}

fn target(root: &Path, target: Binary) -> Result<PathBuf> {
    // macOS signs an entire .app bundle as a unit. It does NOT mutate or
    // select executables in nested per-version folders inside the signed app.
    // The next signed bundle replaces this complete package.
    #[cfg(target_os = "macos")]
    if root
        .parent()
        .is_some_and(|contents| contents.join("Info.plist").is_file())
    {
        let direct = root.join(target.file_name(std::env::consts::OS, ""));
        let meta = std::fs::symlink_metadata(&direct)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!("signed app component is missing or not a regular file");
        }
        return Ok(direct);
    }
    let current = update::read_launcher_current(root)?;
    let folder = update::version_dir(root, &current.version)?;
    // Symlinks and unexpected files are not an acceptable execution target.
    let name = target.file_name(std::env::consts::OS, &current.version);
    let path = folder.join(name);
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("selected Agent binary is missing or not a regular file");
    }
    Ok(path)
}

fn run() -> Result<()> {
    let launcher = std::env::current_exe()?.canonicalize()?;
    let root = launcher
        .parent()
        .context("launcher has no installation directory")?;
    // Upgrade only on a fresh entry when no old desktop is running. The
    // previous version remains selected until activation succeeds.
    if let Ok(user_data) = cloudfile_core::storage::root() {
        if let Err(err) = update::try_activate_pending(root, &user_data) {
            eprintln!("CloudFile deferred upgrade unchanged: {err:#}");
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selection = delegate(&args);
    let program = target(root, selection)?;
    // All browser Native Messaging data is forwarded over stdio unchanged;
    // the versioned Host alone validates the Chrome origin and framed JSON.
    let mut command = Command::new(program);
    if selection == Binary::Desktop && args.first().is_some_and(|s| s == "--desktop") {
        command.args(&args[1..]);
    } else {
        command.args(&args);
    }
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn main() {
    if let Err(error) = run() {
        eprintln!("CloudFile Launcher: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_only_the_desktop_flags_to_the_ui() {
        for v in ["--autostart", "--desktop"] {
            assert_eq!(delegate(&[v.to_string()]), Binary::Desktop);
        }
        for v in [
            "--open-uri",
            "--update",
            "--register-host",
            "--status",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/",
        ] {
            assert_eq!(delegate(&[v.into()]), Binary::NativeHost);
        }
        assert_eq!(delegate(&[]), Binary::Desktop);
    }
}
