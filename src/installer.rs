use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Supported system package managers, in detection-priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Brew,
    Apt,
    Pacman,
    Dnf,
    Apk,
}

impl PackageManager {
    pub fn name(self) -> &'static str {
        match self {
            PackageManager::Brew => "Homebrew",
            PackageManager::Apt => "apt",
            PackageManager::Pacman => "pacman",
            PackageManager::Dnf => "dnf",
            PackageManager::Apk => "apk",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            PackageManager::Brew => "brew",
            PackageManager::Apt => "apt-get",
            PackageManager::Pacman => "pacman",
            PackageManager::Dnf => "dnf",
            PackageManager::Apk => "apk",
        }
    }

    /// Commands run sequentially inside the installer terminal. `sudo` is
    /// included where the manager requires root: the external terminal
    /// provides a real TTY so the password prompt works.
    pub fn commands(self) -> Vec<&'static str> {
        match self {
            PackageManager::Brew => vec!["brew install libpq"],
            PackageManager::Apt => vec![
                "sudo apt-get update",
                "sudo apt-get install -y postgresql-client",
            ],
            PackageManager::Pacman => vec!["sudo pacman -S --needed postgresql-libs"],
            PackageManager::Dnf => vec!["sudo dnf install -y postgresql"],
            PackageManager::Apk => vec!["sudo apk add postgresql-client"],
        }
    }
}

/// Detects an available package manager. Order matters: platform-specific
/// tools are probed first so a machine with several gets the native one.
pub fn detect_package_manager() -> Option<PackageManager> {
    detect_with(&|bin| which::which(bin).is_ok())
}

pub fn detect_with(exists: &impl Fn(&str) -> bool) -> Option<PackageManager> {
    use PackageManager::*;
    const ALL: [PackageManager; 5] = [Brew, Apt, Pacman, Dnf, Apk];
    ALL.into_iter().find(|pm| exists(pm.binary()))
}

/// Human-readable one-line suggestion, e.g. for error messages.
pub fn suggest_command(pm: PackageManager) -> String {
    pm.commands().join(" && ")
}

/// Builds the shell script executed in the external terminal.
pub fn build_install_script(pm: PackageManager) -> String {
    let mut s = String::from("#!/bin/sh\nset -e\n\n");
    s.push_str("echo 'pg-studio needs pg_dump (Postgres client tools).'\n");
    s.push_str(&format!("echo 'Installing via {}...'\n\n", pm.name()));
    for cmd in pm.commands() {
        s.push_str(&format!("echo '$ {cmd}'\n{cmd}\n"));
    }
    if pm == PackageManager::Brew {
        // libpq is keg-only: brew never puts pg_dump on PATH by itself.
        s.push_str("\necho\necho 'NOTE: Homebrew installs libpq keg-only.'\n");
        s.push_str("echo 'pg-studio finds pg_dump there automatically; to use it yourself:'\n");
        s.push_str("echo '  export PATH=\"$(brew --prefix)/opt/libpq/bin:$PATH\"'\n");
    }
    s.push_str(
        "\necho\necho 'Done. You can close this window and retry your dump in pg-studio.'\n",
    );
    s.push_str("read -r -p 'Press Enter to close...' _\n");
    s
}

/// Writes the installer script to a temp file and returns its path.
pub fn write_install_script(pm: PackageManager) -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("pg-studio-install-{nanos}.sh"));
    std::fs::write(&path, build_install_script(pm))
        .with_context(|| format!("Failed to write installer script {:?}", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .context("Failed to make installer script executable")?;
    }
    Ok(path)
}

/// Opens the installer script in a NEW terminal window so package-manager
/// prompts (e.g. sudo passwords) stay interactive. Spawned detached: we do
/// not wait for the install to finish.
pub fn open_installer_terminal(pm: PackageManager) -> Result<PathBuf> {
    let script = write_install_script(pm)?;
    spawn_external_terminal(&script)?;
    Ok(script)
}

/// Opens a new terminal window running the installer script.
/// Spawned detached: we do not wait for the install to finish.
#[cfg(target_os = "macos")]
fn spawn_external_terminal(script: &Path) -> Result<()> {
    Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(script)
        .spawn()
        .context("Failed to launch Terminal.app")?;
    Ok(())
}

/// Opens a new terminal window running the installer script.
/// Spawned detached: we do not wait for the install to finish.
#[cfg(not(target_os = "macos"))]
fn spawn_external_terminal(script: &Path) -> Result<()> {
    use anyhow::anyhow;
    let script_str = script.as_os_str().to_owned();
    let candidates: &[(&str, Vec<std::ffi::OsString>)] = &[
        ("gnome-terminal", vec!["--".into(), script_str.clone()]),
        ("konsole", vec!["-e".into(), script_str.clone()]),
        (
            "xfce4-terminal",
            vec!["-x".into(), "sh".into(), script_str.clone()],
        ),
        (
            "alacritty",
            vec!["-e".into(), "sh".into(), script_str.clone()],
        ),
        ("kitty", vec!["-e".into(), "sh".into(), script_str.clone()]),
        ("foot", vec!["-e".into(), "sh".into(), script_str.clone()]),
        (
            "x-terminal-emulator",
            vec!["-e".into(), "sh".into(), script_str.clone()],
        ),
        ("xterm", vec!["-e".into(), "sh".into(), script_str]),
    ];
    for (bin, args) in candidates {
        if which::which(bin).is_ok() {
            Command::new(bin)
                .args(args)
                .spawn()
                .with_context(|| format!("Failed to launch {bin}"))?;
            return Ok(());
        }
    }
    Err(anyhow!(
        "No supported terminal emulator found. Run the installer manually: sh {}",
        script.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_follows_priority_order() {
        // Nothing installed.
        assert_eq!(detect_with(&|_| false), None);
        // pacman alone wins even though it is late in the order.
        assert_eq!(
            detect_with(&|b| b == "pacman"),
            Some(PackageManager::Pacman)
        );
        // brew beats everything when present.
        assert_eq!(
            detect_with(&|b| b == "brew" || b == "apt-get"),
            Some(PackageManager::Brew)
        );
        assert_eq!(
            detect_with(&|b| b == "dnf" || b == "apk"),
            Some(PackageManager::Dnf)
        );
    }

    #[test]
    fn sudo_only_where_needed() {
        assert!(!PackageManager::Brew.commands().join(" ").contains("sudo"));
        for pm in [
            PackageManager::Apt,
            PackageManager::Pacman,
            PackageManager::Dnf,
            PackageManager::Apk,
        ] {
            assert!(
                pm.commands().iter().any(|c| c.starts_with("sudo ")),
                "{pm:?} needs sudo"
            );
        }
    }

    #[test]
    fn script_mentions_commands_pause_and_brew_caveat() {
        let brew = build_install_script(PackageManager::Brew);
        assert!(brew.starts_with("#!/bin/sh"));
        assert!(brew.contains("brew install libpq"));
        assert!(!brew.contains("sudo"));
        assert!(brew.contains("Press Enter to close"));
        assert!(brew.contains("opt/libpq/bin"));

        let apt = build_install_script(PackageManager::Apt);
        assert!(apt.contains("sudo apt-get install -y postgresql-client"));
        assert!(apt.contains("sudo apt-get update"));

        let pacman = build_install_script(PackageManager::Pacman);
        assert!(pacman.contains("sudo pacman -S --needed postgresql-libs"));
    }
}
