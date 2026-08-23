use anyhow::{Context, Result};
use self_update::update::ReleaseUpdate;
use self_update::{Status, cargo_crate_version};
use std::sync::atomic::AtomicBool;

fn build_update() -> Result<Box<dyn ReleaseUpdate>> {
    self_update::backends::github::Update::configure()
        .repo_owner("ujjwal6792")
        .repo_name("pg-studio")
        .bin_name("pg-studio")
        .show_download_progress(false)
        .show_output(false)
        .no_confirm(true)
        .current_version(cargo_crate_version!())
        .build()
        .context("Failed to configure update checker")
}

/// Phases of a self-update, reported through [`update_with_progress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    /// Contacting GitHub Releases for metadata.
    Checking,
    /// Downloading the new release and replacing the binary.
    Downloading,
}

impl UpdatePhase {
    pub fn label(&self) -> &'static str {
        match self {
            UpdatePhase::Checking => "Checking GitHub Releases...",
            UpdatePhase::Downloading => "Downloading & installing update...",
        }
    }
}

/// How a self-update ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    UpToDate(String),
    Updated(String),
    /// Cancelled at a phase boundary; nothing was modified.
    Cancelled,
}

impl UpdateOutcome {
    pub fn message(&self) -> String {
        match self {
            UpdateOutcome::UpToDate(v) => format!("Already on the latest version (v{v})."),
            UpdateOutcome::Updated(v) => {
                format!("Successfully updated to v{v}! Please restart pg-studio.")
            }
            UpdateOutcome::Cancelled => "Update cancelled; this installation is unchanged.".into(),
        }
    }
}

/// Looks up the latest GitHub release. Returns `(current, Some(latest))` when
/// an upgrade exists, `(current, None)` when already current.
pub fn latest_version_info() -> Result<(String, Option<String>)> {
    let update = build_update()?;
    let current = update.current_version();
    let latest = update
        .get_latest_release()
        .context("Failed to fetch the latest release")?;
    if self_update::version::bump_is_greater(&current, &latest.version)? {
        Ok((current, Some(latest.version)))
    } else {
        Ok((current, None))
    }
}

fn perform_install() -> Result<Status> {
    build_update()?
        .update()
        .context("Failed to perform self update")
}

/// One-shot check without installing (used by `--check`).
pub fn check_for_update() -> Result<String> {
    match latest_version_info()? {
        (current, None) => Ok(format!("You are on the latest version (v{current}).")),
        (_, Some(latest)) => Ok(format!(
            "A new version is available: v{latest} (currently v{}). Run `pg-studio --update` to install it.",
            cargo_crate_version!()
        )),
    }
}

/// Runs a self-update in reportable phases. `report` receives each phase as
/// it begins (plus the target version once known); `cancelled` is polled at
/// every phase boundary so callers can abort before anything is modified.
pub fn update_with_progress(
    report: &dyn Fn(UpdatePhase, &str),
    cancelled: &AtomicBool,
) -> Result<UpdateOutcome> {
    use std::sync::atomic::Ordering;

    report(UpdatePhase::Checking, "");
    let found = match latest_version_info() {
        Ok((current, None)) => return Ok(UpdateOutcome::UpToDate(current)),
        Err(e) if cancelled.load(Ordering::Relaxed) => {
            let _ = e;
            return Ok(UpdateOutcome::Cancelled);
        }
        Err(e) => return Err(e),
        Ok((_, Some(version))) => version,
    };

    if cancelled.load(Ordering::Relaxed) {
        return Ok(UpdateOutcome::Cancelled);
    }
    report(UpdatePhase::Downloading, &found);
    // Once the download starts we no longer abort: a half-replaced binary
    // would be worse than finishing the swap.
    let status = perform_install()?;
    Ok(match status {
        Status::UpToDate(v) => UpdateOutcome::UpToDate(v),
        Status::Updated(v) => UpdateOutcome::Updated(v),
    })
}
