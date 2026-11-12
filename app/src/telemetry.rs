//! Log and crash reporting to Sentry, and the tags that make reports sortable.
//!
//! Every log line goes to Sentry Logs with attributes, so one computer, one
//! part of the program, or one kind of event can be picked out:
//!
//! | Attribute | Meaning |
//! | --- | --- |
//! | `component` | The part that logged: `app`, `p2p`, `work`, `gpu`, `store`, ... (the line's `[tag]`) |
//! | `event` | The kind of line: `dial_failed`, `reservation_ok`, `block_won`, ... (see `classify`) |
//! | `device` | The label the user gave this computer, if any |
//! | `install_id` | A random id made on first run; stable for one data folder |
//! | `run` | A fresh id made at each program start; selects one run of one node |
//! | `peer_id` | The node's libp2p peer id (matches the relay's log) |
//! | `network` | `mainnet` or `regtest` |
//! | `address` | The address of the key in use |
//! | `gpu` | The GPU in use while mining |
//!
//! A line may also carry its own fields (`peer`, `reason`, `via`, `height`,
//! ...), passed through the log macros; `before_send_log` turns them into
//! attributes on that one line. A repeated failed dial (same `peer` and
//! `reason`) is logged to the file but sent to Sentry only once per run.
//!
//! The same values are set as tags on crash reports. Sentry adds the release
//! (`jcoin-app@<version>`) and the operating system itself.
//!
//! Reporting is off when `JCOIN_APP_NO_TELEMETRY=1` is set, and in tests
//! (nothing initializes it there).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use jcoin::log::Field;

/// The project's client key. It is public by design: it only allows sending.
const DSN: &str =
    "https://74dd551989b3007f97fc2a2d2d9eb331@o4512410815234048.ingest.us.sentry.io/4512410839351296";

const INSTALL_ID_FILE: &str = "install_id";
const DEVICE_FILE: &str = "device_name";

/// Tags attached to every log line and crash report.
static TAGS: OnceLock<Arc<RwLock<BTreeMap<String, String>>>> = OnceLock::new();

fn tags() -> &'static Arc<RwLock<BTreeMap<String, String>>> {
    TAGS.get_or_init(Default::default)
}

/// Start reporting. Keep the returned guard alive for the life of the
/// program: dropping it sends what is still queued. Returns `None` when
/// reporting is turned off.
pub fn init(data_dir: &Path, regtest: bool) -> Option<sentry::ClientInitGuard> {
    if std::env::var("JCOIN_APP_NO_TELEMETRY").is_ok_and(|v| v == "1") {
        return None;
    }
    let network = if regtest { "regtest" } else { "mainnet" };
    let guard = sentry::init(
        sentry::ClientOptions::new()
            .dsn(DSN)
            .maybe_release(sentry::release_name!())
            .environment(network)
            // JCOIN_APP_TELEMETRY_DEBUG=1 prints what the SDK sends to stderr.
            .debug(std::env::var("JCOIN_APP_TELEMETRY_DEBUG").is_ok_and(|v| v == "1"))
            // Scope tags do not reach log lines, so add them here, then the
            // line's own structured fields (which win over a tag of the same
            // name).
            .before_send_log(|mut log| {
                if let Ok(tags) = tags().read() {
                    for (key, value) in tags.iter() {
                        log.attributes
                            .entry(key.clone())
                            .or_insert_with(|| value.clone().into());
                    }
                }
                PENDING_FIELDS.with(|fields| {
                    for (key, value) in fields.borrow().iter() {
                        log.attributes.insert((*key).to_string(), value.clone().into());
                    }
                });
                Some(log)
            }),
    );
    set_tag("network", network);
    // A fresh id for this run of the program, so one run's lines can be
    // selected without a time window.
    set_tag("run", &hex::encode(rand::random::<[u8; 4]>()));
    let install_id = install_id(data_dir);
    set_tag("install_id", &install_id);
    let device = device_name(data_dir);
    if !device.is_empty() {
        set_tag("device", &device);
    }
    sentry::configure_scope(|scope| {
        scope.set_user(Some(sentry::User {
            id: Some(install_id),
            username: (!device.is_empty()).then_some(device),
            ..Default::default()
        }));
    });
    Some(guard)
}

/// Set (or with an empty value, remove) a tag on every later log line and
/// crash report.
pub fn set_tag(key: &str, value: &str) {
    if let Ok(mut tags) = tags().write() {
        if value.is_empty() {
            tags.remove(key);
        } else {
            tags.insert(key.to_string(), value.to_string());
        }
    }
    sentry::configure_scope(|scope| {
        if value.is_empty() {
            scope.remove_tag(key);
        } else {
            scope.set_tag(key, value);
        }
    });
}

/// Send what is queued, waiting up to two seconds. Call before the process
/// exits by a path that skips the guard (the updater's restart).
pub fn flush() {
    if let Some(client) = sentry::Hub::current().client() {
        client.flush(Some(Duration::from_secs(2)));
    }
}

/// The random id of this install: read from the data folder, or made and
/// stored there on first run.
fn install_id(data_dir: &Path) -> String {
    let path = data_dir.join(INSTALL_ID_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let id = text.trim();
        if id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()) {
            return id.to_string();
        }
    }
    let id = hex::encode(rand::random::<[u8; 8]>());
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&path, &id);
    id
}

/// The label the user gave this computer, or an empty string.
pub fn device_name(data_dir: &Path) -> String {
    std::fs::read_to_string(data_dir.join(DEVICE_FILE))
        .map(|s| clean_label(&s))
        .unwrap_or_default()
}

/// Store the label for this computer and tag later reports with it. An empty
/// label removes the tag. Returns the label as stored.
pub fn set_device_name(data_dir: &Path, name: &str) -> Result<String, String> {
    let label = clean_label(name);
    std::fs::write(data_dir.join(DEVICE_FILE), &label)
        .map_err(|e| format!("cannot save the label: {e}"))?;
    set_tag("device", &label);
    Ok(label)
}

/// A label as it is stored: trimmed, at most 32 characters, and only letters,
/// digits, spaces, `-`, and `_`, so it is safe to search for.
fn clean_label(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .take(32)
        .collect()
}

/// How a log line is filed.
#[derive(Debug, PartialEq)]
pub struct Class {
    /// The part of the program, from the line's leading `[tag]`.
    pub component: &'static str,
    /// The kind of line.
    pub event: &'static str,
    /// Routine trouble that needs no attention by itself (a peer that cannot
    /// be dialed): reported as a warning, not an error.
    pub routine: bool,
}

/// File a log line by its text. The node logs plain lines; this is the one
/// place that knows their wording, so a new kind of line is added here.
pub fn classify(text: &str) -> Class {
    let (component, rest) = match text.strip_prefix('[').and_then(|t| t.split_once("] ")) {
        Some((tag, rest)) => (
            match tag {
                "app" => "app",
                "p2p" => "p2p",
                "work" => "work",
                "mine" => "mine",
                "gpu" => "gpu",
                "store" => "store",
                "chain" => "chain",
                "node" => "node",
                "relay" => "relay",
                _ => "other",
            },
            rest,
        ),
        None => ("app", text),
    };
    let has = |needle: &str| rest.contains(needle);
    let starts = |prefix: &str| rest.starts_with(prefix);
    let (event, routine) = match component {
        "p2p" if starts("dial to") || starts("cannot dial") => ("dial_failed", true),
        "p2p" if starts("could not open a protocol stream") => ("stream_failed", true),
        "p2p" if starts("hole punch") && has("succeeded") => ("hole_punch_ok", false),
        "p2p" if starts("hole punch") => ("hole_punch_failed", true),
        "p2p" if starts("relay reservation accepted") => ("reservation_ok", false),
        "p2p" if has("relay reservation") => ("reservation_lost", false),
        "p2p" if starts("registered with") => ("rendezvous_registered", false),
        "p2p" if has("rendezvous") => ("rendezvous_failed", false),
        "p2p" if starts("connected to") => ("peer_connected", false),
        "p2p" if starts("disconnected from") => ("peer_disconnected", false),
        "p2p" if starts("handshake with") => ("handshake_failed", true),
        "p2p" if starts("peer id") => ("identity", false),
        "p2p" if starts("listening on") => ("listening", false),
        "p2p" if starts("host stopped") => ("host_stopped", false),
        "p2p" if starts("stored new block") => ("block_received", false),
        "p2p" if starts("synced up to") => ("sync_done", false),
        "work" | "mine" if starts("won block") => ("block_won", false),
        "work" | "mine" if starts("found candidate") => ("block_candidate", false),
        "gpu" if has(": accepted") => ("block_accepted", false),
        "gpu" if starts("height") => ("block_not_accepted", false),
        "gpu" if starts("cannot get work") => ("work_unavailable", false),
        "gpu" if starts("submit failed") => ("submit_failed", false),
        "store" if starts("replayed") => ("store_replayed", false),
        "store" if has("from another chain") => ("chain_moved_aside", false),
        "chain" if has("newer rules") => ("newer_rules", false),
        "chain" if starts("reorg") => ("reorg", false),
        "app" if starts("status:") => ("status", false),
        "app" if starts("transfer") && has(" sent") => ("transfer_sent", false),
        "app" if starts("transfer") && has("confirmed") => ("transfer_confirmed", false),
        "app" if starts("transfer") && has("received") => ("transfer_received", false),
        "node" if starts("pending transfer") && has("dropped") => ("pending_dropped", false),
        "app" if starts("JeffyCoin") && has("starting") => ("startup", false),
        "app" if starts("mining started") => ("mining_started", false),
        "app" if starts("mining stopped") => ("mining_stopped", false),
        "app" if starts("cannot open the GPU") => ("gpu_unavailable", false),
        "app" if starts("uninstalling") => ("uninstalled", false),
        "app" if starts("install") || starts("running") => ("install", false),
        "app" if starts("GPU use limit") => ("gpu_limit_set", false),
        "app" if starts("update") => ("update", false),
        _ => ("other", false),
    };
    Class {
        component,
        event,
        routine,
    }
}

// Fields of the line currently being reported, picked up by `before_send_log`
// and attached to the Sentry log record. Set and cleared around each emit on
// the same thread, so a thread-local is enough.
thread_local! {
    static PENDING_FIELDS: RefCell<Vec<Field>> = const { RefCell::new(Vec::new()) };
}

/// `(peer, reason)` pairs of failed dials already reported this run. A repeat is
/// kept in the local log file but not sent again.
static DIAL_SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// The value of a field by key, if present.
fn field<'a>(fields: &'a [Field], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
}

/// True the first time this run a dial to a peer fails for a reason; false for a
/// repeat. Keeps Sentry from filling with the same unreachable address.
fn first_dial_failure(fields: &[Field]) -> bool {
    let key = format!(
        "{}/{}",
        field(fields, "peer").unwrap_or("?"),
        field(fields, "reason").unwrap_or("?")
    );
    match DIAL_SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock() {
        Ok(mut seen) => seen.insert(key),
        Err(_) => true,
    }
}

/// Send one log line to Sentry Logs, filed by `classify` (or by an explicit
/// `event` field), with its structured `fields` as attributes. Does nothing
/// until `init` has run.
pub fn log(error: bool, text: &str, fields: &[Field]) {
    let class = classify(text);
    // Keep the node's identity as a tag, so its lines match the relay's log.
    if class.event == "identity" {
        if let Some(id) = text.rsplit(' ').next() {
            set_tag("peer_id", id);
        }
    }
    // An explicit `event` field (for a line `classify` does not know) wins.
    let event = field(fields, "event").unwrap_or(class.event);
    let component = class.component;

    // A repeated failed dial stays in the local file but is not resent.
    if event == "dial_failed" && !first_dial_failure(fields) {
        return;
    }

    PENDING_FIELDS.with(|f| *f.borrow_mut() = fields.to_vec());
    if error && class.routine {
        sentry::logger_warn!(component = component, event = event, "{}", text);
    } else if error {
        sentry::logger_error!(component = component, event = event, "{}", text);
    } else {
        sentry::logger_info!(component = component, event = event, "{}", text);
    }
    PENDING_FIELDS.with(|f| f.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_filed_by_component_and_event() {
        let filed = |text: &str| {
            let c = classify(text);
            (c.component, c.event, c.routine)
        };
        assert_eq!(filed("[p2p] dial to 12D3 failed: Timeout"), ("p2p", "dial_failed", true));
        assert_eq!(filed("[p2p] cannot dial /ip4/1.2.3.4: Dial error"), ("p2p", "dial_failed", true));
        assert_eq!(
            filed("[p2p] relay reservation accepted by 12D3; reachable at /ip4/1.2.3.4"),
            ("p2p", "reservation_ok", false)
        );
        assert_eq!(
            filed("[p2p] relay reservation lost (closed); this node cannot be reached until it is renewed"),
            ("p2p", "reservation_lost", false)
        );
        assert_eq!(filed("[p2p] cannot request a relay reservation: x"), ("p2p", "reservation_lost", false));
        assert_eq!(filed("[p2p] connected to 12D3 (out) (peer wire v2, port 8333)"), ("p2p", "peer_connected", false));
        assert_eq!(filed("[p2p] hole punch to 12D3 failed (x); staying on the relay"), ("p2p", "hole_punch_failed", true));
        assert_eq!(filed("[work] won block 4 0000"), ("work", "block_won", false));
        assert_eq!(filed("[gpu] height 4: accepted"), ("gpu", "block_accepted", false));
        assert_eq!(filed("[gpu] height 4: stale"), ("gpu", "block_not_accepted", false));
        assert_eq!(filed("[store] replayed 63 blocks; tip height 63"), ("store", "store_replayed", false));
        assert_eq!(filed("[app] JeffyCoin 2.1.1 starting on mainnet (windows)"), ("app", "startup", false));
        assert_eq!(filed("JeffyCoin node: REST on 127.0.0.1:3000"), ("app", "other", false));
        assert_eq!(filed("[new] something"), ("other", "other", false));
        // 2.2.2 events.
        assert_eq!(filed("[app] status: height 4, 2 peers, mining no, 0 h/s, 0 pending"), ("app", "status", false));
        assert_eq!(filed("[app] transfer abcd sent: 1 JEF to ef01"), ("app", "transfer_sent", false));
        assert_eq!(filed("[app] uninstalling"), ("app", "uninstalled", false));
        assert_eq!(filed("[p2p] stored new block 5 from a peer"), ("p2p", "block_received", false));
        assert_eq!(filed("[p2p] synced up to height 10 (3 block(s) in the last batch)"), ("p2p", "sync_done", false));
        assert_eq!(filed("[chain] reorg: tip moved to height 9, 2 block(s) abandoned"), ("chain", "reorg", false));
        assert_eq!(filed("[node] pending transfer abcd dropped without confirming (nonce 3)"), ("node", "pending_dropped", false));
    }

    #[test]
    fn labels_are_cleaned_and_the_install_id_is_stable() {
        assert_eq!(clean_label("  Noah's PC!  "), "Noahs PC");
        assert_eq!(clean_label(&"x".repeat(50)).len(), 32);
        let dir = std::env::temp_dir().join(format!("jcoin-telemetry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = install_id(&dir);
        assert_eq!(id.len(), 16);
        assert_eq!(install_id(&dir), id);
        assert_eq!(device_name(&dir), "");
        assert_eq!(set_device_name(&dir, "desk; top").unwrap(), "desk top");
        assert_eq!(device_name(&dir), "desk top");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repeated_failed_dial_is_reported_once() {
        let line = |peer: &str, reason: &str| {
            vec![
                ("event", "dial_failed".to_string()),
                ("peer", peer.to_string()),
                ("reason", reason.to_string()),
            ]
        };
        // First time for a (peer, reason) sends; a repeat does not.
        assert!(first_dial_failure(&line("AAAAAA", "timeout")));
        assert!(!first_dial_failure(&line("AAAAAA", "timeout")));
        // A different peer, or a different reason, is sent again.
        assert!(first_dial_failure(&line("BBBBBB", "timeout")));
        assert!(first_dial_failure(&line("AAAAAA", "refused")));
    }

    #[test]
    fn an_explicit_event_field_overrides_classify() {
        let fields = [("event", "status".to_string())];
        assert_eq!(field(&fields, "event"), Some("status"));
        // classify would call this an unknown line.
        assert_eq!(classify("[app] node heartbeat").event, "other");
    }
}
