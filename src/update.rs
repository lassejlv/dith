//! Self-update command (`dith update`) pulling prebuilt release tarballs from GitHub Releases.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

/// GitHub repository slug hosting official releases (`owner/repo`).
pub const GITHUB_REPO: &str = "lassejlv/dith";

/// Options controlling `dith update`.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Only check for a newer release without modifying the installed binary.
    pub check_only: bool,
    /// Optional explicit release tag (e.g. `v0.2.0`); defaults to the latest published release.
    pub version: Option<String>,
    /// Reinstall even when the installed binary already matches the target release version.
    pub force: bool,
    /// Skip refreshing agent skills after updating the binary.
    pub no_skills: bool,
}

/// Structured result emitted by `dith update` (human-readable or `--json`).
#[derive(Debug, Clone, Serialize)]
pub struct UpdateReport {
    pub status: &'static str,
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub updated: bool,
    pub target: &'static str,
    pub install_path: String,
    pub release_url: String,
    pub skills_updated: bool,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// Return the official prebuilt target triple supported for the current OS & CPU architecture.
///
/// # Errors
/// Returns an error on platforms without prebuilt release artifacts (only macOS ARM and Linux
/// Intel `x86_64` are shipped as prebuilt binaries).
pub const fn detect_target() -> Result<&'static str, &'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Ok("aarch64-apple-darwin")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Ok("x86_64-unknown-linux-gnu")
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        Err(
            "prebuilt binaries are only published for macOS ARM (aarch64-apple-darwin) and Linux Intel (x86_64-unknown-linux-gnu); update from source via `cargo install --git https://github.com/lassejlv/dith --force`",
        )
    }
}

/// Parse a `MAJOR.MINOR.PATCH` semantic version string (with optional leading `v` or prerelease suffix).
#[must_use]
pub fn parse_semver(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    let core = trimmed.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

/// Return `true` when `candidate` represents a strictly newer semantic version than `current`.
#[must_use]
pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    match (parse_semver(current), parse_semver(candidate)) {
        (Some(cur), Some(cand)) => cand > cur,
        _ => normalize_tag(current) != normalize_tag(candidate),
    }
}

fn normalize_tag(tag: &str) -> &str {
    tag.trim().strip_prefix('v').unwrap_or(tag.trim())
}

/// Look up `asset_name` inside a standard `sha256sum` / `shasum -a 256` checksums file and verify
/// that `actual_hash` matches.
///
/// # Errors
/// Returns an error if the asset is listed in `checksums_text` and the hash does not match.
pub fn verify_sha256_in_checksums(
    checksums_text: &str,
    asset_name: &str,
    actual_hash: &str,
) -> Result<()> {
    for line in checksums_text.lines() {
        let mut parts = line.split_whitespace();
        let Some(expected) = parts.next() else {
            continue;
        };
        let Some(name_raw) = parts.next() else {
            continue;
        };
        let name = name_raw.trim_start_matches('*');
        if name == asset_name {
            if !expected.eq_ignore_ascii_case(actual_hash.trim()) {
                bail!(
                    "checksum mismatch for '{asset_name}': expected {expected}, got {}",
                    actual_hash.trim()
                );
            }
            return Ok(());
        }
    }
    Ok(())
}

/// Execute `dith update`: query GitHub Releases, verify checksum, atomically replace the running
/// binary, and silently refresh agent skills unless disabled.
///
/// # Errors
/// Returns an error if the platform is unsupported, GitHub cannot be reached, checksum verification
/// fails, or the executable cannot be replaced.
pub fn run_update(opts: &UpdateOptions) -> Result<UpdateReport> {
    let target = detect_target().map_err(|msg| anyhow!("{msg}"))?;
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let install_path = std::env::current_exe().context("locating current dith executable")?;

    let release = fetch_release(opts.version.as_deref())?;
    let latest_version = normalize_tag(&release.tag_name).to_string();
    let update_available = if opts.version.is_some() {
        latest_version != current_version
    } else {
        is_newer_version(&current_version, &latest_version)
    };

    if opts.check_only || (!update_available && !opts.force) {
        return Ok(UpdateReport {
            status: "ok",
            current_version,
            latest_version,
            update_available,
            updated: false,
            target,
            install_path: install_path.display().to_string(),
            release_url: release.html_url,
            skills_updated: false,
        });
    }

    let asset_name = format!("dith-{target}.tar.gz");
    let archive_asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| {
            anyhow!(
                "release '{}' does not contain asset '{asset_name}' yet",
                release.tag_name
            )
        })?;

    let stage_dir = create_staging_dir()?;
    let result = install_release_binary(
        &stage_dir,
        &asset_name,
        &archive_asset.browser_download_url,
        release
            .assets
            .iter()
            .find(|a| a.name == "checksums.txt")
            .map(|a| a.browser_download_url.as_str()),
        &install_path,
    );
    let _ = fs::remove_dir_all(&stage_dir);
    result?;

    let skills_updated = if opts.no_skills {
        false
    } else {
        refresh_agent_skills_silently()
    };

    Ok(UpdateReport {
        status: "ok",
        current_version,
        latest_version,
        update_available,
        updated: true,
        target,
        install_path: install_path.display().to_string(),
        release_url: release.html_url,
        skills_updated,
    })
}

fn fetch_release(explicit_version: Option<&str>) -> Result<GithubRelease> {
    let url = match explicit_version {
        Some(ver) => {
            let tag = if ver.starts_with('v') {
                ver.to_string()
            } else {
                format!("v{ver}")
            };
            format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{tag}")
        }
        None => format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest"),
    };

    let output = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            concat!("User-Agent: dith/", env!("CARGO_PKG_VERSION")),
            &url,
        ])
        .output()
        .context("running curl to query GitHub Releases")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("failed to query GitHub Releases ({url}): {}", stderr.trim());
    }

    serde_json::from_slice(&output.stdout).context("parsing GitHub Releases API response")
}

fn download_file(url: &str, dest: &Path) -> Result<()> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            concat!("User-Agent: dith/", env!("CARGO_PKG_VERSION")),
            url,
            "-o",
        ])
        .arg(dest)
        .output()
        .with_context(|| format!("running curl to download '{url}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("failed to download '{url}': {}", stderr.trim());
    }
    Ok(())
}

fn compute_sha256(path: &Path) -> Option<String> {
    if let Ok(out) = Command::new("sha256sum").arg(path).output()
        && out.status.success()
    {
        return String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .map(ToString::to_string);
    }
    if let Ok(out) = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        && out.status.success()
    {
        return String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .map(ToString::to_string);
    }
    None
}

fn install_release_binary(
    stage_dir: &Path,
    asset_name: &str,
    archive_url: &str,
    checksums_url: Option<&str>,
    install_path: &Path,
) -> Result<()> {
    let archive_path = stage_dir.join(asset_name);
    download_file(archive_url, &archive_path)?;

    if let Some(cs_url) = checksums_url {
        let checksums_path = stage_dir.join("checksums.txt");
        if download_file(cs_url, &checksums_path).is_ok()
            && let Ok(text) = fs::read_to_string(&checksums_path)
            && let Some(actual) = compute_sha256(&archive_path)
        {
            verify_sha256_in_checksums(&text, asset_name, &actual)?;
        }
    }

    let tar_out = Command::new("tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(stage_dir)
        .output()
        .context("running tar to unpack release archive")?;
    if !tar_out.status.success() {
        let stderr = String::from_utf8_lossy(&tar_out.stderr);
        bail!("failed to unpack '{asset_name}': {}", stderr.trim());
    }

    let extracted = stage_dir.join("dith");
    if !extracted.is_file() {
        bail!("release archive '{asset_name}' did not contain a 'dith' binary");
    }

    // Stage inside target directory so rename is atomic on POSIX filesystems.
    let parent = install_path.parent().ok_or_else(|| {
        anyhow!(
            "cannot determine parent directory of '{}'",
            install_path.display()
        )
    })?;
    let staged_dest = parent.join(format!(".dith.update.{}", std::process::id()));
    fs::copy(&extracted, &staged_dest).with_context(|| {
        format!(
            "copying updated binary into '{}' (check write permissions)",
            parent.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staged_dest, fs::Permissions::from_mode(0o755))
            .context("setting executable permissions on updated binary")?;
    }

    if let Err(err) = fs::rename(&staged_dest, install_path) {
        let _ = fs::remove_file(&staged_dest);
        return Err(err)
            .with_context(|| format!("replacing executable '{}'", install_path.display()));
    }

    Ok(())
}

fn create_staging_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "dith-update-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating staging directory '{}'", dir.display()))?;
    Ok(dir)
}

/// Silently refresh global agent skills using the skills CLI without echoing runner output.
fn refresh_agent_skills_silently() -> bool {
    Command::new("npx")
        .args(["--yes", "skills", "add", GITHUB_REPO, "-g", "-y"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parsing_and_comparison_work() {
        assert_eq!(parse_semver("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("v2.0.0-rc.1"), Some((2, 0, 0)));
        assert!(is_newer_version("0.1.0", "0.2.0"));
        assert!(is_newer_version("0.1.0", "v0.1.1"));
        assert!(!is_newer_version("0.2.0", "0.2.0"));
        assert!(!is_newer_version("0.2.0", "0.1.9"));
    }

    #[test]
    fn checksum_verification_accepts_match_and_rejects_mismatch() {
        let manifest = "\
aabbccdd  dith-aarch64-apple-darwin.tar.gz
11223344  *dith-x86_64-unknown-linux-gnu.tar.gz
";
        assert!(
            verify_sha256_in_checksums(manifest, "dith-aarch64-apple-darwin.tar.gz", "aabbccdd")
                .is_ok()
        );
        assert!(
            verify_sha256_in_checksums(
                manifest,
                "dith-x86_64-unknown-linux-gnu.tar.gz",
                "11223344"
            )
            .is_ok()
        );
        assert!(
            verify_sha256_in_checksums(manifest, "dith-aarch64-apple-darwin.tar.gz", "deadbeef")
                .is_err()
        );
    }
}
