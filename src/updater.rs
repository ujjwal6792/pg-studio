use anyhow::Result;
use self_update::cargo_crate_version;

pub fn update_cli() -> Result<()> {
    println!("Checking for updates...");

    let status = self_update::backends::github::Update::configure()
        .repo_owner("ujjwal6792")
        .repo_name("pg-studio")
        .bin_name("pg-studio")
        .show_download_progress(true)
        .current_version(cargo_crate_version!())
        .build()?
        .update()?;

    println!("Update status: `{}`!", status.version());
    Ok(())
}
