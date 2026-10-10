//! Version module: one [`VersionInfo`] struct, one [`ReleaseSource`] seam,
//! one [`format_version`] site owning the `v` prefix.
//!
//! Updates run resolve-then-install in a single metadata fetch:
//! [`ReleaseSource::resolve`] snapshots the newest release (version plus the
//! asset URL for this target) and [`ReleaseSource::install`] downloads exactly
//! that snapshot. Check and install therefore cannot disagree, unlike the old
//! flow where each of them re-resolved "latest" from GitHub.

use anyhow::{Context, Result};
use self_update::{Status, cargo_crate_version};
use std::sync::atomic::AtomicBool;

/// Display form of a bare version (`0.5.3`, or a `v0.5.3` tag passed
/// through idempotently). This is the single owner of the `v` prefix: every
/// user-facing version string goes through here.
pub fn format_version(version: &str) -> String {
    let bare = version.strip_prefix('v').unwrap_or(version);
    if bare.is_empty() {
        String::new()
    } else {
        format!("v{bare}")
    }
}

/// Semver "is `latest` newer than `current`?", tolerant of the `v` prefixes
/// GitHub tags carry (`v0.5.3` vs crate version `0.5.3`) and of pre-releases.
pub fn is_newer(current: &str, latest: &str) -> Result<bool> {
    let current = current.strip_prefix('v').unwrap_or(current);
    let latest = latest.strip_prefix('v').unwrap_or(latest);
    self_update::version::bump_is_greater(current, latest).context("Failed to compare versions")
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
            UpdateOutcome::UpToDate(v) => {
                format!("Already on the latest version ({}).", format_version(v))
            }
            UpdateOutcome::Updated(v) => {
                format!(
                    "Successfully updated to {}! Please restart pg-studio.",
                    format_version(v)
                )
            }
            UpdateOutcome::Cancelled => "Update cancelled; this installation is unchanged.".into(),
        }
    }
}

/// The current binary version plus the newest released version, if newer.
/// `latest` is `None` when already current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub current: String,
    pub latest: Option<String>,
}

impl VersionInfo {
    pub fn is_update_available(&self) -> bool {
        self.latest.is_some()
    }
}

/// A release already resolved to a downloadable asset: everything
/// [`ReleaseSource::install`] needs without another metadata fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRelease {
    /// Bare version (`v` prefix stripped, as reported by GitHub metadata).
    pub version: String,
    pub asset_name: String,
    pub download_url: String,
}

/// Seam between update orchestration and the release backend. The contract
/// is one metadata fetch per update: [`ReleaseSource::resolve`] snapshots,
/// [`ReleaseSource::install`] consumes the snapshot (asset download and
/// binary swap only).
pub trait ReleaseSource {
    fn current_version(&self) -> String;
    /// Exactly one metadata fetch: newest release for this target. The
    /// asset is `None` when already current, or when the release ships no
    /// binary for this target (callers must surface that instead of
    /// offering an install that cannot succeed).
    fn resolve(&self) -> Result<(VersionInfo, Option<ResolvedRelease>)>;
    /// Install an already-resolved release. Performs no metadata fetches.
    fn install(&self, release: &ResolvedRelease) -> Result<Status>;
}

/// [`ReleaseSource`] backed by GitHub Releases via `self_update`.
#[derive(Debug, Clone)]
pub struct GithubSource {
    owner: String,
    repo: String,
    bin_name: String,
}

impl Default for GithubSource {
    fn default() -> Self {
        Self {
            owner: "ujjwal6792".to_owned(),
            repo: "pg-studio".to_owned(),
            bin_name: "pg-studio".to_owned(),
        }
    }
}

/// Binary name as stored at the top level of each release archive.
fn archive_member() -> &'static str {
    if cfg!(windows) {
        "pg-studio.exe"
    } else {
        "pg-studio"
    }
}

impl GithubSource {
    fn configured_update(&self) -> Result<Box<dyn self_update::update::ReleaseUpdate>> {
        self_update::backends::github::Update::configure()
            .repo_owner(&self.owner)
            .repo_name(&self.repo)
            .bin_name(&self.bin_name)
            .bin_path_in_archive(archive_member())
            .show_download_progress(false)
            .show_output(false)
            .no_confirm(true)
            .current_version(cargo_crate_version!())
            .build()
            .context("Failed to configure update checker")
    }
}

impl ReleaseSource for GithubSource {
    fn current_version(&self) -> String {
        cargo_crate_version!().to_owned()
    }

    fn resolve(&self) -> Result<(VersionInfo, Option<ResolvedRelease>)> {
        let update = self.configured_update()?;
        let current = update.current_version();
        let release = update
            .get_latest_release()
            .context("Failed to fetch the latest release")?;
        if !is_newer(&current, &release.version)? {
            return Ok((
                VersionInfo {
                    current,
                    latest: None,
                },
                None,
            ));
        }
        let version = release.version.clone();
        let asset = release
            .asset_for(self_update::get_target(), None)
            .map(|a| ResolvedRelease {
                version: version.clone(),
                asset_name: a.name,
                download_url: a.download_url,
            });
        Ok((
            VersionInfo {
                current,
                latest: Some(version),
            },
            asset,
        ))
    }

    fn install(&self, release: &ResolvedRelease) -> Result<Status> {
        // Mirrors the tail of `self_update`'s `update_extended` for an
        // already-fetched release: download the pinned asset, extract the
        // binary, swap it in. No metadata fetch happens here.
        let tmp_dir =
            self_update::TempDir::new().context("Failed to create temp dir for update")?;
        let archive_path = tmp_dir.path().join(&release.asset_name);
        let mut archive_file =
            std::fs::File::create(&archive_path).context("Failed to create temp download file")?;
        self_update::Download::from_url(&release.download_url)
            .download_to(&mut archive_file)
            .context("Failed to download update")?;
        drop(archive_file);

        let member = archive_member();
        self_update::Extract::from_source(&archive_path)
            .extract_file(tmp_dir.path(), member)
            .context("Failed to extract update archive")?;
        let new_exe = tmp_dir.path().join(member);
        self_update::self_replace::self_replace(&new_exe).context("Failed to replace binary")?;
        Ok(Status::Updated(release.version.clone()))
    }
}

/// Looks up the latest GitHub release. Returns `(current, Some(latest))` when
/// an upgrade exists, `(current, None)` when already current.
///
/// Kept as a compatibility wrapper; the implementation uses the same
/// single-resolution path as the update flow.
pub fn latest_version_info() -> Result<(String, Option<String>)> {
    let (info, _) = GithubSource::default().resolve()?;
    Ok((info.current, info.latest))
}

/// One-shot check without installing (used by `--check`).
pub fn check_for_update() -> Result<String> {
    let (info, _) = GithubSource::default().resolve()?;
    match info.latest {
        None => Ok(format!(
            "You are on the latest version ({}).",
            format_version(&info.current)
        )),
        Some(latest) => Ok(format!(
            "A new version is available: {} (currently {}). Run `pg-studio --update` to install it.",
            format_version(&latest),
            format_version(&info.current),
        )),
    }
}

/// Runs a self-update in reportable phases against any [`ReleaseSource`].
/// `report` receives each phase as it begins (plus the target version once
/// known); `cancelled` is polled at every phase boundary so callers can
/// abort before anything is modified.
pub fn update_with_source(
    source: &dyn ReleaseSource,
    report: &dyn Fn(UpdatePhase, &str),
    cancelled: &AtomicBool,
) -> Result<UpdateOutcome> {
    use std::sync::atomic::Ordering;

    report(UpdatePhase::Checking, "");
    // The single metadata fetch of this update.
    let (info, asset) = match source.resolve() {
        Ok(resolved) => resolved,
        Err(e) if cancelled.load(Ordering::Relaxed) => {
            let _ = e;
            return Ok(UpdateOutcome::Cancelled);
        }
        Err(e) => return Err(e),
    };
    let Some(latest) = info.latest else {
        return Ok(UpdateOutcome::UpToDate(info.current));
    };

    if cancelled.load(Ordering::Relaxed) {
        return Ok(UpdateOutcome::Cancelled);
    }
    let Some(asset) = asset else {
        anyhow::bail!(
            "Release {} has no binary for target `{}`; see GitHub Releases for a manual install.",
            format_version(&latest),
            self_update::get_target(),
        );
    };
    report(UpdatePhase::Downloading, &latest);
    // Once the download starts we no longer abort: a half-replaced binary
    // would be worse than finishing the swap.
    let status = source.install(&asset)?;
    Ok(match status {
        Status::UpToDate(v) => UpdateOutcome::UpToDate(v),
        Status::Updated(v) => UpdateOutcome::Updated(v),
    })
}

/// Runs a self-update in reportable phases against GitHub Releases.
/// `report` receives each phase as it begins (plus the target version once
/// known); `cancelled` is polled at every phase boundary so callers can
/// abort before anything is modified.
pub fn update_with_progress(
    report: &dyn Fn(UpdatePhase, &str),
    cancelled: &AtomicBool,
) -> Result<UpdateOutcome> {
    update_with_source(&GithubSource::default(), report, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Canned [`ReleaseSource`]: no network, scripted resolve/install.
    struct FakeSource {
        current: &'static str,
        /// Newest release the "backend" knows about.
        newest: Option<&'static str>,
        /// When true, the release ships no asset for this target.
        no_asset: bool,
        installed: Mutex<Vec<String>>,
    }

    impl FakeSource {
        fn current(newest: Option<&'static str>) -> Self {
            Self {
                current: "0.5.3",
                newest,
                no_asset: false,
                installed: Mutex::new(Vec::new()),
            }
        }
    }

    impl ReleaseSource for FakeSource {
        fn current_version(&self) -> String {
            self.current.to_owned()
        }

        fn resolve(&self) -> Result<(VersionInfo, Option<ResolvedRelease>)> {
            // Like `Release::from_release`, the backend reports bare versions.
            let latest = match self.newest {
                Some(v) if is_newer(self.current, v).unwrap_or(false) => {
                    Some(v.strip_prefix('v').unwrap_or(v).to_owned())
                }
                _ => None,
            };
            let asset = match (&latest, self.no_asset) {
                (Some(v), false) => Some(ResolvedRelease {
                    version: v.clone(),
                    asset_name: "pg-studio-test.tar.gz".to_owned(),
                    download_url: "https://example.invalid/pg-studio-test.tar.gz".to_owned(),
                }),
                _ => None,
            };
            Ok((
                VersionInfo {
                    current: self.current.to_owned(),
                    latest,
                },
                asset,
            ))
        }

        fn install(&self, release: &ResolvedRelease) -> Result<Status> {
            self.installed
                .lock()
                .expect("installed lock")
                .push(release.version.clone());
            Ok(Status::Updated(release.version.clone()))
        }
    }

    fn run_update(
        source: &FakeSource,
        cancel_upfront: bool,
    ) -> (Result<UpdateOutcome>, Vec<UpdatePhase>) {
        let phases = Mutex::new(Vec::new());
        let cancelled = AtomicBool::new(cancel_upfront);
        let outcome = update_with_source(
            source,
            &|phase, _| {
                phases.lock().expect("phases lock").push(phase);
            },
            &cancelled,
        );
        let phases = phases.into_inner().expect("phases lock");
        (outcome, phases)
    }

    #[test]
    fn format_version_owns_the_v_prefix() {
        assert_eq!(format_version("0.5.3"), "v0.5.3");
        assert_eq!(format_version("v0.5.3"), "v0.5.3");
        assert_eq!(format_version(""), "");
    }

    #[test]
    fn is_newer_handles_plain_stable_versions() {
        assert!(is_newer("0.5.3", "0.5.4").expect("compare"));
        assert!(!is_newer("0.5.3", "0.5.3").expect("compare"));
        assert!(!is_newer("0.5.4", "0.5.3").expect("compare"));
    }

    #[test]
    fn is_newer_strips_v_prefix_like_github_tags() {
        assert!(is_newer("0.5.3", "v0.5.4").expect("compare"));
        assert!(is_newer("v0.5.3", "v0.5.4").expect("compare"));
        assert!(!is_newer("v0.5.3", "v0.5.3").expect("compare"));
    }

    #[test]
    fn is_newer_handles_pre_releases() {
        // Pre-release installed, stable released: upgrade.
        assert!(is_newer("0.5.4-beta.1", "0.5.4").expect("compare"));
        // Older pre-release available is still newer than an older stable.
        assert!(is_newer("0.5.3", "0.5.4-beta.1").expect("compare"));
        // Newer pre-release installed: no downgrade.
        assert!(!is_newer("0.5.4-beta.2", "0.5.4-beta.1").expect("compare"));
    }

    #[test]
    fn up_to_date_reports_checking_only_and_installs_nothing() {
        let source = FakeSource::current(None);
        let (outcome, phases) = run_update(&source, false);
        assert_eq!(
            outcome.expect("outcome"),
            UpdateOutcome::UpToDate("0.5.3".into())
        );
        assert_eq!(phases, vec![UpdatePhase::Checking]);
        assert!(source.installed.lock().expect("lock").is_empty());
    }

    #[test]
    fn outdated_installs_the_resolved_snapshot() {
        let source = FakeSource::current(Some("0.5.4"));
        let (outcome, phases) = run_update(&source, false);
        assert_eq!(
            outcome.expect("outcome"),
            UpdateOutcome::Updated("0.5.4".into())
        );
        assert_eq!(
            phases,
            vec![UpdatePhase::Checking, UpdatePhase::Downloading]
        );
        assert_eq!(
            *source.installed.lock().expect("lock"),
            vec!["0.5.4".to_owned()]
        );
    }

    #[test]
    fn outdated_with_v_prefixed_tag_installs_bare_version() {
        let source = FakeSource::current(Some("v0.5.4"));
        let (outcome, _) = run_update(&source, false);
        // The snapshot normalises through compare/display, never double-`v`.
        assert_eq!(
            outcome.expect("outcome"),
            UpdateOutcome::Updated("0.5.4".into())
        );
        assert_eq!(
            UpdateOutcome::Updated("0.5.4".into()).message(),
            "Successfully updated to v0.5.4! Please restart pg-studio."
        );
    }

    #[test]
    fn cancelled_upfront_installs_nothing() {
        let source = FakeSource::current(Some("0.5.4"));
        let (outcome, phases) = run_update(&source, true);
        assert_eq!(outcome.expect("outcome"), UpdateOutcome::Cancelled);
        assert_eq!(phases, vec![UpdatePhase::Checking]);
        assert!(source.installed.lock().expect("lock").is_empty());
    }

    #[test]
    fn cancelled_between_phases_installs_nothing() {
        let source = FakeSource::current(Some("0.5.4"));
        let phases = Mutex::new(Vec::new());
        let cancelled = AtomicBool::new(false);
        let outcome = update_with_source(
            &source,
            &|phase, _| {
                phases.lock().expect("phases lock").push(phase);
                if phase == UpdatePhase::Checking {
                    cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
            &cancelled,
        );
        assert_eq!(outcome.expect("outcome"), UpdateOutcome::Cancelled);
        assert_eq!(*phases.lock().expect("lock"), vec![UpdatePhase::Checking]);
        assert!(source.installed.lock().expect("lock").is_empty());
    }

    #[test]
    fn missing_target_asset_is_a_clear_error_not_a_failed_install() {
        let source = FakeSource {
            no_asset: true,
            ..FakeSource::current(Some("0.5.4"))
        };
        let (outcome, phases) = run_update(&source, false);
        let err = outcome.expect_err("missing asset must fail");
        assert!(
            format!("{err:#}").contains("has no binary for target"),
            "unexpected error: {err:#}"
        );
        assert_eq!(phases, vec![UpdatePhase::Checking]);
        assert!(source.installed.lock().expect("lock").is_empty());
    }

    #[test]
    fn outcome_messages_use_single_v_prefix() {
        assert_eq!(
            UpdateOutcome::UpToDate("0.5.3".into()).message(),
            "Already on the latest version (v0.5.3)."
        );
        assert_eq!(
            UpdateOutcome::Cancelled.message(),
            "Update cancelled; this installation is unchanged."
        );
    }
}
