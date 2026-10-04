//! Checks GitHub Releases for a newer Oynx and installs it.
//!
//! Releases are published by `.github/workflows/release.yml`, which attaches
//! the installer `oynx-<version>-windows-x86_64-setup.exe` and records its
//! SHA-256 in the release notes. An update is only installed if that checksum
//! matches.
//!
//! The verified installer is saved to the temp folder and run silently when
//! Oynx quits, so the new version starts next time. "Restart now" runs it
//! straight away instead, and the installer starts the new version when it
//! finishes. See `installer/oynx.iss` for the command-line switches.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/tacobellerontop-sudo/Oynx/releases/latest";
pub const RELEASES_PAGE_URL: &str = "https://github.com/tacobellerontop-sudo/Oynx/releases";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpdateMode {
    /// Check in the background and install updates for the next launch.
    Automatic,
    /// Only check when the user asks.
    #[default]
    Manual,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    asset_url: String,
    asset_size: u64,
    sha256: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        release: Release,
        downloaded: u64,
        total: u64,
    },
    /// The installer is downloaded and verified; it runs when Oynx quits.
    Ready(Release),
    Failed(String),
}

impl UpdateStatus {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Downloading { .. })
    }
}

pub struct Updater {
    status: Arc<Mutex<UpdateStatus>>,
    last_checked: Arc<Mutex<Option<Instant>>>,
    /// The verified installer waiting to be run.
    pending: Arc<Mutex<Option<PathBuf>>>,
}

impl Updater {
    pub fn new() -> Self {
        // Left behind by an update that has already been installed.
        let _ = std::fs::remove_file(installer_path());
        Self {
            status: Arc::new(Mutex::new(UpdateStatus::Idle)),
            last_checked: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(None)),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().map(|status| status.clone()).unwrap_or_default()
    }

    pub fn last_checked(&self) -> Option<Instant> {
        self.last_checked.lock().ok().and_then(|checked| *checked)
    }

    /// Checks for a newer release in the background. With `install`, a newer
    /// release is downloaded and installed straight away.
    pub fn check(&self, ctx: &eframe::egui::Context, install: bool) {
        if self.status().is_busy() {
            return;
        }
        let status = Arc::clone(&self.status);
        let last_checked = Arc::clone(&self.last_checked);
        let pending = Arc::clone(&self.pending);
        let ctx = ctx.clone();
        set(&status, &ctx, UpdateStatus::Checking);
        std::thread::spawn(move || {
            let result = latest_release();
            if let Ok(mut checked) = last_checked.lock() {
                *checked = Some(Instant::now());
            }
            match result {
                Ok(release) if is_newer(&release.version, CURRENT_VERSION) => {
                    if install && install_blocker().is_none() {
                        download_installer(release, &status, &pending, &ctx);
                    } else {
                        set(&status, &ctx, UpdateStatus::Available(release));
                    }
                }
                Ok(_) => set(&status, &ctx, UpdateStatus::UpToDate),
                Err(error) => set(&status, &ctx, UpdateStatus::Failed(error)),
            }
        });
    }

    pub fn install(&self, ctx: &eframe::egui::Context, release: Release) {
        if self.status().is_busy() {
            return;
        }
        let status = Arc::clone(&self.status);
        let pending = Arc::clone(&self.pending);
        let ctx = ctx.clone();
        std::thread::spawn(move || download_installer(release, &status, &pending, &ctx));
    }

    /// Runs the downloaded installer with its progress window showing; it
    /// closes this copy of Oynx and starts the new version when it finishes.
    pub fn install_now(&self) -> Result<(), String> {
        let setup = self
            .take_pending()
            .ok_or_else(|| "No update has been downloaded yet.".to_owned())?;
        run_installer(&setup, &["/SILENT", "/LAUNCH=1"])?;
        if let Ok(mut status) = self.status.lock() {
            *status = UpdateStatus::Idle;
        }
        Ok(())
    }

    /// Runs a downloaded installer without any window as Oynx quits, so the
    /// new version is in place for the next start.
    pub fn install_on_exit(&self) {
        if let Some(setup) = self.take_pending() {
            if let Err(error) = run_installer(&setup, &["/VERYSILENT", "/LAUNCH=0"]) {
                log::warn!("{error}");
            }
        }
    }

    fn take_pending(&self) -> Option<PathBuf> {
        self.pending.lock().ok().and_then(|mut pending| pending.take())
    }
}

/// Why this copy of Oynx can't replace itself, if it can't.
pub fn install_blocker() -> Option<String> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return Some(format!("Could not locate the running Oynx: {error}")),
    };
    // A binary under Cargo's `target\debug` or `target\release` is a development
    // build; overwriting it with a release download would be surprising.
    let in_target = exe
        .parent()
        .and_then(|dir| dir.parent())
        .and_then(Path::file_name)
        .is_some_and(|name| name == "target");
    in_target.then(|| {
        "This is a development build (run from Cargo's target folder), so updates are not installed here. \
         Install Oynx with the setup program from the releases page to get updates."
            .to_owned()
    })
}

/// Where a downloaded installer waits until it is run.
fn installer_path() -> PathBuf {
    std::env::temp_dir().join("oynx-update-setup.exe")
}

/// Starts the installer and returns without waiting for it. The switches are
/// Inno Setup's; `/LAUNCH` is read by `installer/oynx.iss`.
fn run_installer(setup: &Path, mode: &[&str]) -> Result<(), String> {
    std::process::Command::new(setup)
        .args(mode)
        .args(["/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"])
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not start the Oynx installer: {error}"))
}

fn set(status: &Mutex<UpdateStatus>, ctx: &eframe::egui::Context, value: UpdateStatus) {
    if let Ok(mut current) = status.lock() {
        *current = value;
    }
    ctx.request_repaint();
}

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("Oynx/{CURRENT_VERSION}"))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|error| error.to_string())
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn latest_release() -> Result<Release, String> {
    let response = http()?
        .get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .map_err(|error| format!("Could not reach GitHub: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub returned HTTP {} while checking for updates.", response.status()));
    }
    let release: GithubRelease = response
        .json()
        .map_err(|error| format!("Could not read the release information: {error}"))?;
    let asset = release
        .assets
        .iter()
        .find(|asset| is_installer_asset(&asset.name))
        .ok_or_else(|| format!("Release {} has no Windows download yet.", release.tag_name))?;
    Ok(Release {
        version: release.tag_name.trim_start_matches('v').to_owned(),
        page_url: release.html_url,
        asset_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
        sha256: release.body.as_deref().and_then(find_sha256),
    })
}

fn is_installer_asset(name: &str) -> bool {
    name.starts_with("oynx-") && name.ends_with("-windows-x86_64-setup.exe")
}

/// The first 64-character hex string in the release notes.
fn find_sha256(notes: &str) -> Option<String> {
    notes
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|token| token.len() == 64)
        .map(str::to_ascii_lowercase)
}

/// Compares dotted numeric versions (`1.10.0` > `1.9.3`); a pre-release suffix
/// such as `-beta` sorts before the plain release.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(version: &str) -> (Vec<u64>, bool) {
        let (numbers, pre) = match version.split_once('-') {
            Some((numbers, _)) => (numbers, true),
            None => (version, false),
        };
        (numbers.split('.').map(|part| part.parse().unwrap_or(0)).collect(), pre)
    }
    let (mut a, a_pre) = parse(candidate);
    let (mut b, b_pre) = parse(current);
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => b_pre && !a_pre,
        ordering => ordering == std::cmp::Ordering::Greater,
    }
}

fn download_installer(
    release: Release,
    status: &Mutex<UpdateStatus>,
    pending: &Mutex<Option<PathBuf>>,
    ctx: &eframe::egui::Context,
) {
    let result = (|| -> Result<PathBuf, String> {
        if let Some(reason) = install_blocker() {
            return Err(reason);
        }
        let expected = release.sha256.clone().ok_or_else(|| {
            "This release does not list a SHA-256 checksum, so it was not installed.".to_owned()
        })?;
        let download = installer_path();

        let mut response = http()?
            .get(&release.asset_url)
            .send()
            .map_err(|error| format!("Download failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("Download failed: HTTP {}", response.status()));
        }
        let total = response.content_length().unwrap_or(release.asset_size);
        let mut file = std::fs::File::create(&download).map_err(|error| {
            format!("Could not save the update to {}: {error}", download.display())
        })?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        let mut downloaded = 0_u64;
        let mut last_report = Instant::now();
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("Download interrupted: {error}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("Could not save the download: {error}"))?;
            downloaded += read as u64;
            if last_report.elapsed() >= Duration::from_millis(100) {
                last_report = Instant::now();
                set(status, ctx, UpdateStatus::Downloading { release: release.clone(), downloaded, total });
            }
        }
        file.flush().map_err(|error| error.to_string())?;
        drop(file);

        let actual = format!("{:x}", hasher.finalize());
        if actual != expected {
            let _ = std::fs::remove_file(&download);
            return Err("The download did not match the published checksum, so it was discarded.".to_owned());
        }
        Ok(download)
    })();
    match result {
        Ok(setup) => {
            if let Ok(mut pending) = pending.lock() {
                *pending = Some(setup);
            }
            set(status, ctx, UpdateStatus::Ready(release));
        }
        Err(error) => set(status, ctx, UpdateStatus::Failed(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.2.0", "0.2.0-beta"));
        assert!(!is_newer("0.2.0-beta", "0.2.0"));
    }

    #[test]
    fn only_the_installer_is_picked_as_the_update() {
        assert!(is_installer_asset("oynx-0.1.3-windows-x86_64-setup.exe"));
        assert!(!is_installer_asset("oynx-0.1.2-windows-x86_64.exe"));
        assert!(!is_installer_asset("oynx-0.1.3-windows-x86_64-setup.exe.sha256"));
    }

    #[test]
    fn checksum_is_read_from_release_notes() {
        let notes = "SHA256 (oynx-0.1.3-windows-x86_64-setup.exe)\n\n```\nABCDEF0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789\n```";
        assert_eq!(
            find_sha256(notes).as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert_eq!(find_sha256("no checksum here"), None);
    }
}
