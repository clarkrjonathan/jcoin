//! Updates from the project's GitHub releases.
//!
//! The app lists the releases that carry a build for this system, lets the
//! user pick one, downloads it, checks it against the release's SHA-256 list,
//! replaces the running program file, and starts the new one.
//!
//! The checksum catches a damaged or incomplete download. It does not prove
//! who made the file: both come from the same release page.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// The GitHub repository whose releases are offered, as `owner/name`. The
/// releases must be readable without signing in.
const REPOSITORY: &str = "clarkrjonathan/jcoin";

/// The version of this build. A release is tagged `v` plus this.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
impl Release {
    /// A release with no download, for tests.
    pub fn for_test(tag: &str) -> Release {
        Release {
            tag: tag.to_string(),
            date: "2026-01-01".to_string(),
            program_url: String::new(),
            sums_url: String::new(),
        }
    }
}

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

/// The tag and date (`YYYY-MM-DD`) of each release in GitHub's Atom feed of
/// releases, in feed order. The tag is read from the entry's link
/// (`.../releases/tag/<tag>`), since the title is the release's name.
fn feed_entries(feed: &str) -> Vec<(String, String)> {
    let between = |text: &str, start: &str, end: &str| -> Option<String> {
        let from = text.find(start)? + start.len();
        Some(text[from..][..text[from..].find(end)?].to_string())
    };
    feed.split("<entry>")
        .skip(1)
        .filter_map(|entry| {
            let tag = between(entry, "/releases/tag/", "\"")?;
            let date = between(entry, "<updated>", "</updated>").unwrap_or_default();
            Some((tag, date.chars().take(10).collect()))
        })
        .collect()
}

/// Where a repository's releases are read from: the API's release list, the
/// releases feed, and the base of the file download addresses.
struct Source {
    api: String,
    feed: String,
    downloads: String,
}

impl Source {
    fn github() -> Source {
        Source {
            api: format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=30"),
            feed: format!("https://github.com/{REPOSITORY}/releases.atom"),
            downloads: format!("https://github.com/{REPOSITORY}/releases/download"),
        }
    }
}

/// Ask GitHub for the releases that have a build for this system. A failure
/// is logged as well as returned, so it reaches the reports.
pub fn fetch_releases() -> Result<Vec<Release>, String> {
    let result = fetch_from(&Source::github());
    if let Err(e) = &result {
        jcoin::log_error!(
            [("event", "update_check_failed".to_string()), ("reason", e.clone())];
            "[app] update check failed: {e}"
        );
    }
    result
}

/// The API's release list first. Without signing in, GitHub allows 60 API
/// requests an hour from one internet address, shared by everyone behind it
/// (a dormitory or campus network uses them up). When it refuses (403 or
/// 429), the releases feed is read instead: it is not limited that way, but
/// it lists only the newest 10 releases, does not say which files they have,
/// and does not mark prereleases (this project publishes none).
fn fetch_from(source: &Source) -> Result<Vec<Release>, String> {
    let (program, sums) = asset_names().ok_or("there are no builds for this system")?;
    let http = client(Duration::from_secs(15))?;
    let refused = match http
        .get(&source.api)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<serde_json::Value>())
    {
        Ok(reply) => return Ok(releases_from(&reply, program, sums)),
        Err(e) if matches!(e.status().map(|s| s.as_u16()), Some(403 | 429)) => e,
        Err(e) => return Err(format!("cannot read the release list: {e}")),
    };
    jcoin::log_info!(
        [("event", "update_check_limited".to_string()), ("reason", refused.to_string())];
        "[app] update check: GitHub refused the release list ({refused}); reading the releases feed"
    );
    let get = |url: &str| http.get(url).send().and_then(|r| r.error_for_status());
    let feed = get(&source.feed)
        .and_then(|r| r.text())
        .map_err(|e| format!("GitHub refused the release list (rate limit) and the releases feed cannot be read: {e}"))?;
    let mut list = Vec::new();
    for (tag, date) in feed_entries(&feed) {
        if parse_version(&tag).is_none() {
            continue;
        }
        let url_of = |name: &str| format!("{}/{tag}/{name}", source.downloads);
        // A release has a build for this system if its checksum list for this
        // system names the program. A missing list (404) means it has none.
        let listed = match get(&url_of(sums)).and_then(|r| r.text()) {
            Ok(text) => expected_hash(&text, program).is_some(),
            Err(e) if e.status().map(|s| s.as_u16()) == Some(404) => false,
            Err(e) => return Err(format!("cannot read the checksum list of {tag}: {e}")),
        };
        if listed {
            list.push(Release { program_url: url_of(program), sums_url: url_of(sums), tag, date });
        }
    }
    list.sort_by_key(|r| std::cmp::Reverse(parse_version(&r.tag)));
    Ok(list)
}

/// Download `release`, check it, and put it in place of the running program
/// file. Returns the program's path, for `restart`. On any error the running
/// file is left as it was.
pub fn install(release: &Release) -> Result<PathBuf, String> {
    jcoin::log_info!(
        [("event", "update_started".to_string()), ("from", CURRENT.to_string()), ("to", release.tag.clone())];
        "[app] update started: {CURRENT} to {}", release.tag
    );
    match install_inner(release) {
        Ok(exe) => {
            jcoin::log_info!(
                [("event", "update_installed".to_string()), ("to", release.tag.clone())];
                "[app] update installed: {}, restarting", release.tag
            );
            Ok(exe)
        }
        Err(e) => {
            jcoin::log_error!(
                [("event", "update_failed".to_string()), ("to", release.tag.clone()), ("reason", e.clone())];
                "[app] update to {} failed: {e}", release.tag
            );
            Err(e)
        }
    }
}

fn install_inner(release: &Release) -> Result<PathBuf, String> {
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
    // Read the path before the swap: on Linux, `current_exe` follows the
    // running file, so after it is renamed it names `<program>.old`.
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    replace_program(&exe, &bytes)?;
    Ok(exe)
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

/// What the update check or an install is doing.
#[derive(Clone)]
pub enum UpdateState {
    Checking,
    /// The releases with a build for this system, newest first.
    Listed(Vec<Release>),
    Installing(String),
    Failed(String),
}

/// How often the release list is read again while the app stays open. The
/// result is information for the user; nothing is installed or stopped by it.
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);

/// The release list and any install in progress. Background threads write the
/// state; the window and the local API read it. Clones share it.
#[derive(Clone)]
pub struct Updates {
    state: Arc<Mutex<UpdateState>>,
    /// When the release list was last asked for.
    checked: Arc<Mutex<Instant>>,
}

impl Updates {
    /// Clean up after a previous update and start the first check.
    pub fn start() -> Updates {
        clean_up();
        let updates = Updates {
            state: Arc::new(Mutex::new(UpdateState::Checking)),
            checked: Arc::new(Mutex::new(Instant::now())),
        };
        updates.check();
        updates
    }

    /// Updates holding `state`, with no check started, for tests.
    #[cfg(test)]
    pub fn with_state(state: UpdateState) -> Updates {
        Updates {
            state: Arc::new(Mutex::new(state)),
            checked: Arc::new(Mutex::new(Instant::now())),
        }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }

    /// Read the release list again if it is due and nothing is in progress.
    pub fn check_if_due(&self) {
        let idle = matches!(self.state(), UpdateState::Listed(_) | UpdateState::Failed(_));
        if idle && self.checked.lock().unwrap().elapsed() >= CHECK_EVERY {
            self.check();
        }
    }

    /// The newest release, if it is newer than this build.
    pub fn newer_release(&self) -> Option<String> {
        match self.state() {
            UpdateState::Listed(list) => list.first().filter(|r| r.is_newer()).map(|r| r.tag.clone()),
            _ => None,
        }
    }

    /// Read the release list again, in the background.
    pub fn check(&self) {
        *self.checked.lock().unwrap() = Instant::now();
        let state = self.state.clone();
        *state.lock().unwrap() = UpdateState::Checking;
        std::thread::spawn(move || {
            let result = match fetch_releases() {
                Ok(list) => UpdateState::Listed(list),
                Err(e) => UpdateState::Failed(e),
            };
            *state.lock().unwrap() = result;
        });
    }

    /// Download and install `release` in the background, then restart into it.
    pub fn install(&self, release: Release) {
        let state = self.state.clone();
        *state.lock().unwrap() = UpdateState::Installing(release.tag.clone());
        std::thread::spawn(move || {
            // `restart` only returns on failure.
            let result = install(&release).and_then(|exe| restart(&exe));
            if let Err(e) = result {
                *state.lock().unwrap() = UpdateState::Failed(e);
            }
        });
    }
}

/// Why "install `release`" is refused now, if it is: the same reasons the
/// window greys out its button for. `not_installed`: this is not the
/// installed copy (updates apply to it).
pub fn install_refusal(release: &Release, not_installed: bool, mining: bool) -> Option<&'static str> {
    if not_installed {
        Some("install JeffyCoin first (see the top of the window); updates apply to the installed copy.")
    } else if release.relation() == "this version" {
        Some("this version is already running.")
    } else if mining {
        Some("mining is running.")
    } else {
        None
    }
}

/// Remove the file a previous update left behind. Call at startup.
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(with_suffix(&exe, ".old"));
    }
}

/// Start the program at `exe` (the path `install` returned) and end this
/// process.
pub fn restart(exe: &Path) -> Result<(), String> {
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| format!("the update is installed, but the program could not be restarted: {e}"))?;
    crate::telemetry::flush();
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_install_is_refused_for_the_reasons_the_window_shows() {
        let newer = Release::for_test("v99.0.0");
        let same = Release::for_test(&format!("v{CURRENT}"));
        assert_eq!(install_refusal(&newer, false, false), None);
        assert!(install_refusal(&newer, true, false).unwrap().contains("install JeffyCoin first"));
        assert!(install_refusal(&same, false, false).unwrap().contains("already running"));
        assert!(install_refusal(&newer, false, true).unwrap().contains("mining"));
    }

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
    fn reading_the_release_list_twice_keeps_a_newer_release_installable() {
        let asset = |name: &str| json!({ "name": name, "browser_download_url": format!("https://x/{name}") });
        let reply = json!([
            { "tag_name": "v2.10.0", "published_at": "2026-12-01T00:00:00Z",
              "assets": [asset("App"), asset("SUMS")] },
            { "tag_name": "v2.0.0", "published_at": "2026-11-11T00:00:00Z",
              "assets": [asset("App"), asset("SUMS")] },
        ]);
        // "Check again" re-reads the list; the newest must still be selectable
        // and marked newer afterwards (the button's grey state keyed off this).
        let first = releases_from(&reply, "App", "SUMS");
        let second = releases_from(&reply, "App", "SUMS");
        assert_eq!(
            first.iter().map(|r| r.tag.clone()).collect::<Vec<_>>(),
            second.iter().map(|r| r.tag.clone()).collect::<Vec<_>>(),
        );
        let newest = second.first().expect("a release");
        assert_eq!(newest.tag, "v2.10.0");
        assert!(newest.is_newer(), "v2.10.0 must read as newer than {CURRENT}");
        assert_ne!(newest.relation(), "this version", "a newer release is installable");
        // The default selection (0) stays in range and points at the newer one.
        let selected = 0usize.min(second.len().saturating_sub(1));
        assert!(second[selected].is_newer());
    }

    /// A local HTTP server that answers each path from `routes` (status,
    /// body), and 404 for any other. Returns its base address.
    fn serve(routes: Vec<(String, u16, String)>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&request);
                let path = text.split(' ').nth(1).unwrap_or("").to_string();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, "Not Found".to_string()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    fn feed_entry(tag: &str, date: &str) -> String {
        format!(
            "<entry>\n<id>tag:github.com,2008:Repository/1/{tag}</id>\n<updated>{date}T09:50:13Z</updated>\n\
             <link rel=\"alternate\" type=\"text/html\" href=\"https://github.com/o/r/releases/tag/{tag}\"/>\n\
             <title>Release {tag}</title>\n<content>No content.</content>\n</entry>\n"
        )
    }

    #[test]
    fn the_feed_gives_tags_from_links_and_dates() {
        let feed = format!(
            "<feed><id>x</id><updated>2026-11-12T00:00:00Z</updated>{}{}</feed>",
            feed_entry("v2.3.1", "2026-11-12"),
            feed_entry("v2.3.0", "2026-11-11")
        );
        assert_eq!(
            feed_entries(&feed),
            [("v2.3.1".to_string(), "2026-11-12".to_string()), ("v2.3.0".to_string(), "2026-11-11".to_string())]
        );
        assert!(feed_entries("<html>rate limited</html>").is_empty());
    }

    #[test]
    fn a_rate_limited_api_falls_back_to_the_feed() {
        let (program, sums) = asset_names().unwrap();
        let feed = format!(
            "<feed>{}{}{}{}</feed>",
            feed_entry("v2.3.1", "2026-11-12"),
            feed_entry("v2.3.0", "2026-11-11"),
            feed_entry("v2.2.0", "2026-11-10"),
            feed_entry("nightly", "2026-11-09"),
        );
        let hash = "a".repeat(64);
        let routes = |api_status: u16| {
            vec![
                ("/api".to_string(), api_status, r#"{"message":"API rate limit exceeded"}"#.to_string()),
                ("/feed".to_string(), 200, feed.clone()),
                // v2.3.1 has this system's build; v2.3.0's list names another
                // file; v2.2.0 has no list for this system (404).
                (format!("/dl/v2.3.1/{sums}"), 200, format!("{hash}  {program}\n")),
                (format!("/dl/v2.3.0/{sums}"), 200, format!("{hash}  other\n")),
            ]
        };
        let source = |base: &str| Source {
            api: format!("{base}/api"),
            feed: format!("{base}/feed"),
            downloads: format!("{base}/dl"),
        };
        for status in [403, 429] {
            let base = serve(routes(status));
            let list = fetch_from(&source(&base)).unwrap();
            let tags: Vec<&str> = list.iter().map(|r| r.tag.as_str()).collect();
            assert_eq!(tags, ["v2.3.1"], "after {status}");
            assert_eq!(list[0].date, "2026-11-12");
            assert_eq!(list[0].program_url, format!("{base}/dl/v2.3.1/{program}"));
            assert_eq!(list[0].sums_url, format!("{base}/dl/v2.3.1/{sums}"));
        }
        // Another failure of the API is reported, not hidden by the feed.
        let base = serve(routes(500));
        let e = fetch_from(&source(&base)).unwrap_err();
        assert!(e.starts_with("cannot read the release list"), "{e}");
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

    /// Why `install` returns the path read before the swap: on Linux a running
    /// program's own path (`/proc/<pid>/exe`, which `current_exe` reads)
    /// follows the file when it is renamed aside.
    #[cfg(target_os = "linux")]
    #[test]
    fn on_linux_the_running_path_follows_the_renamed_file() {
        let dir = std::env::temp_dir().join(format!("jcoin-update-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("prog");
        std::fs::copy("/bin/sleep", &exe).unwrap();
        // A process another test thread starts while the copy is open for
        // writing holds that handle until it runs its own program, and until
        // then starting `prog` fails with "text file busy". Try again.
        let mut tries = 0;
        let mut child = loop {
            match std::process::Command::new(&exe).arg("30").spawn() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 100 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => break result.unwrap(),
            }
        };
        let running = || std::fs::read_link(format!("/proc/{}/exe", child.id())).unwrap();
        // Right after spawn the child may not have started `prog` yet.
        let start = std::time::Instant::now();
        while running() != exe && start.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(running(), exe);
        replace_program(&exe, b"new").unwrap();
        assert_eq!(running(), with_suffix(&exe, ".old"));
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        child.kill().unwrap();
        child.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
