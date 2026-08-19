use anyhow::{Context, Result};
use self_update::update::ReleaseUpdate;
use self_update::{Status, cargo_crate_version};

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

pub fn update_cli() -> Result<String> {
    let status = build_update()?
        .update()
        .context("Failed to perform self update")?;

    match status {
        Status::UpToDate(v) => Ok(format!("Already on the latest version (v{}).", v)),
        Status::Updated(v) => Ok(format!(
            "Successfully updated to v{}! Please restart pg-studio.",
            v
        )),
    }
}

pub fn check_for_update() -> Result<String> {
    let update = build_update()?;
    let current = update.current_version();
    let latest = update
        .get_latest_release()
        .context("Failed to fetch the latest release")?;

    if self_update::version::bump_is_greater(&current, &latest.version)? {
        Ok(format!(
            "A new version is available: v{} (currently v{}). Run `pg-studio --update` to install it.",
            latest.version, current
        ))
    } else {
        Ok(format!("You are on the latest version (v{}).", current))
    }
}
