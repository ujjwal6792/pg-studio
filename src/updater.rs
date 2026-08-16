use anyhow::{Context, Result};
use self_update::{Status, cargo_crate_version};

pub fn update_cli() -> Result<String> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner("ujjwal6792")
        .repo_name("pg-studio")
        .bin_name("pg-studio")
        .show_download_progress(false)
        .current_version(cargo_crate_version!())
        .build()
        .context("Failed to configure update checker")?
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
