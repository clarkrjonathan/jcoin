//! Updates from the project's GitHub releases.
//!
//! The app lists the releases that carry a build for this system, lets the
//! user pick one, downloads it, checks it against the release's SHA-256 list,
//! replaces the running program file, and starts the new one.
//!
//! The checksum catches a damaged or incomplete download. It does not prove
//! who made the file: both come from the same release page.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// The GitHub repository whose releases are offered, as `owner/name`. The
/// releases must be readable without signing in.
const REPOSITORY: &str = "clarkrjonathan/jcoin";

/// The version of this build. A release is tagged `v` plus this.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The release file names for this system: the program and the checksum list.
fn asset_names() -> Option<(&'static str, &'static str)> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some(("JeffyCoin-windows-x86_64.exe", "SHA256SUMS-windows-x86_64.txt"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("JeffyCoin-linux-x86_64", "SHA256SUMS-linux-x86_64.txt"))
    } else {
        None
    }
}

/// A release that has a build for this system.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    /// The tag, for example `v2.1.0`.
    pub tag: String,
    /// The release date, `YYYY-MM-DD`.
    pub date: String,
    program_url: String,
    sums_url: String,
}

impl Release {
    /// How this release relates to the running build.
    pub fn relation(&self) -> &'static str {
        match (parse_version(&self.tag), parse_version(CURRENT)) {
            (Some(theirs), Some(ours)) if theirs > ours => "newer",
            (Some(theirs), Some(ours)) if theirs == ours => "this version",
            (Some(_), Some(_)) => "older",
            _ => "",
        }
    }

    pub fn is_newer(&self) -> bool {
        self.relation() == "newer"
    }
}

/// `v2.10.3` or `2.10.3` as `(2, 10, 3)`. Anything else is `None`.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// The releases in a GitHub "list releases" reply that have both files for
/// this system, newest version first. Drafts and prereleases are left out.
fn releases_from(reply: &serde_json::Value, program: &str, sums: &str) -> Vec<Release> {
    let mut list: Vec<Release> = reply
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true && r["prerelease"] != true)
        .filter_map(|r| {
            let url_of = |name: &str| {
                r["assets"]
                    .as_array()?
                    .iter()
                    .find(|a| a["name"] == name)?["browser_download_url"]
                    .as_str()
                    .map(str::to_string)
            };
            let tag = r["tag_name"].as_str()?.to_string();
            parse_version(&tag)?;
            Some(Release {
                date: r["published_at"].as_str().unwrap_or_default().chars().take(10).collect(),
                program_url: url_of(program)?,
                sums_url: url_of(sums)?,
                tag,
            })
        })
        .collect();
    list.sort_by_key(|r| std::cmp::Reverse(parse_version(&r.tag)));
    list
}

/// The expected SHA-256 of `file` from a `sha256sum` list, as lowercase hex.
fn expected_hash(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file && hash.len() == 64)
            .then(|| hash.to_ascii_lowercase())
    })
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("jeffycoin-app")
        .timeout(timeout)
        .build()
        .map_err(|e| format!("cannot create the HTTP client: {e}"))
}

/// Ask GitHub for the releases that have a build for this system.
pub fn fetch_releases() -> Result<Vec<Release>, String> {
    let (program, sums) = asset_names().ok_or("there are no builds for this system")?;
    let reply: serde_json::Value = client(Duration::from_secs(15))?
        .get(format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30"))
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("cannot read the release list: {e}"))?;
    Ok(releases_from(&reply, program, sums))
}

/// Download `release`, check it, and put it in place of the running program
/// file. The caller then calls `restart`. On any error the running file is
/// left as it was.
pub fn install(release: &Release) -> Result<(), String> {
    let (program, _) = asset_names().ok_or("there are no builds for this system")?;
    let http = client(Duration::from_secs(600))?;
    let get = |url: &str| {
        http.get(url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .map_err(|e| format!("download failed: {e}"))
    };
    let sums = get(&release.sums_url)?;
    let expected = expected_hash(&String::from_utf8_lossy(&sums), program)
        .ok_or("the release's checksum list does not name this file")?;
    let bytes = get(&release.program_url)?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected {
        return Err("the download does not match its checksum; nothing was changed".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    replace_program(&exe, &bytes)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Write `bytes` as the program at `exe`. The old file is renamed to
/// `<exe>.old` first, which works even while it is running (Windows does not
/// allow deleting or overwriting a running program, but allows renaming it).
fn replace_program(exe: &Path, bytes: &[u8]) -> Result<(), String> {
    let fresh = with_suffix(exe, ".new");
    let old = with_suffix(exe, ".old");
    std::fs::write(&fresh, bytes).map_err(|e| format!("cannot write {}: {e}", fresh.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark the download as a program: {e}"))?;
    }
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("cannot move the current program aside: {e}"))?;
    if let Err(e) = std::fs::rename(&fresh, exe) {
        // Put the old program back so the next start still works.
        let _ = std::fs::rename(&old, exe);
        return Err(format!("cannot put the new program in place: {e}"));
    }
    Ok(())
}

/// Remove the file a previous update left behind. Call at startup.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(with_suffix(&exe, ".old"));
    }
}

/// Start the program file again and end this process.
pub fn restart() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| format!("the update is installed, but the program could not be restarted: {e}"))?;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(parse_version("v2.10.3"), Some((2, 10, 3)));
        assert_eq!(parse_version("2.1.0"), Some((2, 1, 0)));
        assert!(parse_version("v2.10.3") > parse_version("v2.9.9"));
        for bad in ["v2.1", "v2.1.0.1", "v2.x.0", "nightly", ""] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
        }
        assert!(parse_version(CURRENT).is_some(), "the package version must be x.y.z");
    }

    #[test]
    fn only_usable_releases_are_listed_newest_first() {
        let asset = |name: &str| json!({ "name": name, "browser_download_url": format!("https://x/{name}") });
        let reply = json!([
            { "tag_name": "v2.1.0", "published_at": "2026-11-12T01:02:03Z",
              "assets": [asset("App"), asset("SUMS")] },
            { "tag_name": "v2.0.0", "published_at": "2026-11-11T01:22:20Z",
              "assets": [asset("SUMS")] },
            { "tag_name": "v2.10.0", "published_at": "2026-12-01T00:00:00Z",
              "assets": [asset("App"), asset("SUMS")] },
            { "tag_name": "v3.0.0", "prerelease": true, "assets": [asset("App"), asset("SUMS")] },
            { "tag_name": "nightly", "assets": [asset("App"), asset("SUMS")] },
        ]);
        let list = releases_from(&reply, "App", "SUMS");
        let tags: Vec<&str> = list.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["v2.10.0", "v2.1.0"]);
        assert_eq!(list[1].date, "2026-11-12");
        assert_eq!(list[1].program_url, "https://x/App");
        assert!(releases_from(&json!({ "message": "Not Found" }), "App", "SUMS").is_empty());
    }

    #[test]
    fn checksum_list_is_read_by_file_name() {
        let hash = "A".repeat(64);
        let sums = format!("{}  other\n{hash}  JeffyCoin\n", "b".repeat(64));
        assert_eq!(expected_hash(&sums, "JeffyCoin"), Some("a".repeat(64)));
        assert_eq!(expected_hash(&format!("{hash} *JeffyCoin"), "JeffyCoin"), Some("a".repeat(64)));
        assert_eq!(expected_hash(&sums, "missing"), None);
        assert_eq!(expected_hash("short  JeffyCoin", "JeffyCoin"), None);
    }

    #[test]
    fn the_program_file_is_replaced_and_the_old_one_kept_aside() {
        let dir = std::env::temp_dir().join(format!("jcoin-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("JeffyCoin");
        std::fs::write(&exe, b"old").unwrap();
        replace_program(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(with_suffix(&exe, ".old")).unwrap(), b"old");
        assert!(!with_suffix(&exe, ".new").exists());
        // A second update replaces the leftover.
        replace_program(&exe, b"newer").unwrap();
        assert_eq!(std::fs::read(with_suffix(&exe, ".old")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
