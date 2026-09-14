//! Opt-in diagnostic session log.
//!
//! One JSONL file per application launch under `<data dir>/logs`, so a session
//! has a clean boundary: everything in the newest file belongs to the run the
//! user is describing.
//!
//! Off unless `LINKVAULT_DEBUG_LOG` says otherwise (`1`/`on`/`true`). Nothing is
//! created, and no thread is started, when it is off.
//!
//! Two properties are deliberate:
//!
//! - **Emitters never block.** The channel is bounded and a full channel drops
//!   the record and bumps a counter rather than stalling a UI thread. A
//!   diagnostic that pauses the app it is measuring is worse than no
//!   diagnostic. This is why `DatabaseWriter::execute` is not reused here: it
//!   blocks the caller by design.
//! - **Redaction happens in the writer, not at the call sites.** Every string
//!   that reaches this module is scrubbed before it is written, so a careless
//!   caller — including the frontend, which we do not control as tightly —
//!   cannot leak a session cookie into the file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use serde_json::{Map, Value};

const LEVEL_ENV: &str = "LINKVAULT_DEBUG_LOG";
const CHANNEL_CAPACITY: usize = 1024;
/// Sessions kept on disk, including the one being written.
const RETAINED_SESSIONS: usize = 10;
/// Deletions attempted per launch. Startup latency stays predictable even if a
/// directory somehow accumulates thousands of files.
const MAX_PRUNE_PER_LAUNCH: usize = 64;
const MAX_FIELD_CHARS: usize = 512;
const FILE_PREFIX: &str = "session-";
const FILE_SUFFIX: &str = ".jsonl";

/// Where an event came from. Not free-form, so the file stays groupable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A user action in the interface: navigation, a button press.
    Ui,
    /// A frontend call across the Tauri boundary.
    Ipc,
    /// Backend-originated: queue drains, shutdown summaries.
    Rust,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Ui => "ui",
            Source::Ipc => "ipc",
            Source::Rust => "rust",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "ui" => Source::Ui,
            "rust" => Source::Rust,
            _ => Source::Ipc,
        }
    }
}

/// One line of the log. There is no open-ended payload field: everything is a
/// named, bounded slot, which is the same structural discipline that keeps
/// `DatabaseDiagnosticEvent` free of secrets.
#[derive(Debug, Clone)]
pub struct Event {
    pub source: Source,
    pub name: String,
    pub duration_ms: Option<u64>,
    pub ok: Option<bool>,
    pub error: Option<String>,
    /// Short, non-secret context: a view name, a count, a reason code.
    pub detail: Option<String>,
}

impl Event {
    pub fn new(source: Source, name: impl Into<String>) -> Self {
        Self {
            source,
            name: name.into(),
            duration_ms: None,
            ok: None,
            error: None,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The frontend batch payload. Mirrors `Event` but arrives untrusted.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiEvent {
    pub source: String,
    pub name: String,
    pub duration_ms: Option<u64>,
    pub ok: Option<bool>,
    pub error: Option<String>,
    pub detail: Option<String>,
}

impl From<UiEvent> for Event {
    fn from(value: UiEvent) -> Self {
        Self {
            source: Source::parse(&value.source),
            name: value.name,
            duration_ms: value.duration_ms,
            ok: value.ok,
            error: value.error,
            detail: value.detail,
        }
    }
}

struct Sink {
    sender: SyncSender<String>,
    dropped: Arc<AtomicU64>,
    path: PathBuf,
}

static SINK: OnceLock<Option<Sink>> = OnceLock::new();

fn enabled_from_env() -> bool {
    match std::env::var(LEVEL_ENV) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "yes"
        ),
        Err(_) => false,
    }
}

/// Start the log if the environment asks for it. Safe to call once; later calls
/// return the already-resolved path. Returns `None` when logging is off or the
/// directory could not be prepared — a diagnostic facility must never be the
/// reason the app fails to start.
pub fn init() -> Option<PathBuf> {
    SINK.get_or_init(|| {
        if !enabled_from_env() {
            return None;
        }
        let root = crate::app::storage::resolve_diagnostics_log_root().ok()?;
        prune_old_sessions(&root, RETAINED_SESSIONS.saturating_sub(1));
        start_writer(root)
    })
    .as_ref()
    .map(|sink| sink.path.clone())
}

/// Path of the session file for this run, if logging is on.
pub fn current_path() -> Option<PathBuf> {
    SINK.get()
        .and_then(|sink| sink.as_ref())
        .map(|sink| sink.path.clone())
}

/// Record an event. A no-op when logging is off, and never blocking.
pub fn record(event: Event) {
    let Some(Some(sink)) = SINK.get() else {
        return;
    };
    let line = match serde_json::to_string(&encode(&event)) {
        Ok(line) => line,
        Err(_) => return,
    };
    if let Err(TrySendError::Full(_)) = sink.sender.try_send(line) {
        sink.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn start_writer(root: PathBuf) -> Option<Sink> {
    let path = root.join(format!(
        "{FILE_PREFIX}{}{FILE_SUFFIX}",
        chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S")
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    let (sender, receiver) = sync_channel::<String>(CHANNEL_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_for_thread = Arc::clone(&dropped);

    let spawned = std::thread::Builder::new()
        .name("linkvault-diagnostics-log".to_string())
        .spawn(move || {
            for line in receiver {
                // Flush per line: a log that loses its tail on a crash is least
                // useful exactly when it matters most. Volume is low enough that
                // buffering would buy nothing.
                if writeln!(file, "{line}").is_err() || file.flush().is_err() {
                    break;
                }
            }
            let lost = dropped_for_thread.load(Ordering::Relaxed);
            if lost > 0 {
                let _ = writeln!(
                    file,
                    "{{\"src\":\"rust\",\"ev\":\"diagnostics.dropped\",\"detail\":\"{lost}\"}}"
                );
                let _ = file.flush();
            }
        })
        .ok();
    spawned?;

    Some(Sink {
        sender,
        dropped,
        path,
    })
}

fn encode(event: &Event) -> Value {
    let mut map = Map::new();
    map.insert(
        "ts".to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );
    map.insert(
        "src".to_string(),
        Value::String(event.source.as_str().to_string()),
    );
    map.insert("ev".to_string(), Value::String(clean(&event.name)));
    if let Some(ms) = event.duration_ms {
        map.insert("ms".to_string(), Value::from(ms));
    }
    if let Some(ok) = event.ok {
        map.insert("ok".to_string(), Value::Bool(ok));
    }
    if let Some(error) = &event.error {
        map.insert("err".to_string(), Value::String(clean(error)));
    }
    if let Some(detail) = &event.detail {
        map.insert("detail".to_string(), Value::String(clean(detail)));
    }
    Value::Object(map)
}

/// Scrub then bound every string that reaches the file.
fn clean(value: &str) -> String {
    let redacted = redact(value);
    truncate(&redacted, MAX_FIELD_CHARS)
}

/// Replace the value that follows a secret-bearing key name.
///
/// Error strings are the realistic leak path: they are assembled far away from
/// here and can quote a request header or a query string back at us.
pub(crate) fn redact(value: &str) -> String {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(
            // Group 3 is an optional auth scheme. Without it `Authorization:
            // Bearer <token>` would redact the word "Bearer" and leave the
            // token in the clear.
            r#"(?i)(li_at|cauth|jsessionid|csrf[-_]?token|access[-_]?token|refresh[-_]?token|authorization|password|passwd|secret|cookie|token)(\s*["']?\s*[=:]\s*["']?)((?:bearer|basic|digest)\s+)?[^\s"',;&}\]]+"#,
        )
        .expect("diagnostics redaction pattern is a compile-time constant")
    });
    pattern
        .replace_all(value, "${1}${2}${3}[redacted]")
        .into_owned()
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let kept: String = value.chars().take(max_chars).collect();
    format!("{kept}…")
}

/// Delete the oldest sessions beyond `keep`. Filenames are ISO-8601, so
/// lexicographic order is chronological order.
fn prune_old_sessions(root: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut sessions: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Reject anything that is not a regular file, symlinks included, so a
        // planted link cannot redirect a delete outside the log directory.
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX) {
            sessions.push(path);
        }
    }
    if sessions.len() <= keep {
        return;
    }
    sessions.sort();
    let excess = sessions.len() - keep;
    for path in sessions.into_iter().take(excess.min(MAX_PRUNE_PER_LAUNCH)) {
        let _ = fs::remove_file(path);
    }
}

/// Upper bound on one frontend batch. The frontend buffers before flushing, so
/// a batch this large means something is wrong; drop the excess rather than
/// let the UI drive unbounded work.
const MAX_BATCH: usize = 256;

#[tauri::command]
pub fn log_ui_events(events: Vec<UiEvent>) -> Result<(), String> {
    for event in events.into_iter().take(MAX_BATCH) {
        record(event.into());
    }
    Ok(())
}

#[tauri::command]
pub fn diagnostics_log_path() -> Option<String> {
    current_path().map(|path| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts the property that matters — the secret is gone — rather than an
    /// exact output shape, so tightening the pattern later does not churn the
    /// test.
    #[test]
    fn redacts_secret_bearing_values_in_free_text() {
        let cases = [
            ("li_at=AQEDATestCookieValue", "AQEDATestCookieValue"),
            ("Cookie: li_at=abc123def; other=keep", "abc123def"),
            ("\"token\": \"sk-secret-value\"", "sk-secret-value"),
            ("CAUTH=coursera-session-value", "coursera-session-value"),
            // The scheme-prefixed header form: the token sits after "Bearer".
            ("Authorization: Bearer abcdefsecret", "abcdefsecret"),
            ("password=hunter2", "hunter2"),
        ];
        for (input, secret) in cases {
            let output = redact(input);
            assert!(
                !output.contains(secret),
                "redact({input:?}) = {output:?}, still contains {secret:?}"
            );
            assert!(
                output.contains("[redacted]"),
                "redact({input:?}) = {output:?}, expected a redaction marker"
            );
        }
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        let input = "queue drained 12 jobs in 340ms";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redaction_does_not_leak_the_original_secret() {
        let secret = "AQEDATestCookieValue";
        let output = redact(&format!("failed with li_at={secret} while saving"));
        assert!(!output.contains(secret));
        assert!(output.contains("while saving"));
    }

    #[test]
    fn encoded_events_are_scrubbed_and_bounded() {
        let event = Event {
            source: Source::Ipc,
            name: "save_li_at_token".to_string(),
            duration_ms: Some(842),
            ok: Some(false),
            error: Some("rejected li_at=AQEDATestCookieValue".to_string()),
            detail: None,
        };
        let encoded = serde_json::to_string(&encode(&event)).unwrap();
        assert!(!encoded.contains("AQEDATestCookieValue"));
        assert!(encoded.contains("save_li_at_token"));
        assert!(encoded.contains("\"ms\":842"));
        assert!(encoded.contains("\"ok\":false"));
    }

    #[test]
    fn long_fields_are_truncated() {
        let long = "a".repeat(MAX_FIELD_CHARS * 2);
        let cleaned = clean(&long);
        assert_eq!(cleaned.chars().count(), MAX_FIELD_CHARS + 1);
        assert!(cleaned.ends_with('…'));
    }

    #[test]
    fn recording_is_inert_when_logging_is_disabled() {
        // SINK is never initialised in unit tests, so this must not panic and
        // must not create anything.
        record(Event::new(Source::Ui, "navigate").with_detail("downloads"));
        assert!(current_path().is_none());
    }

    #[test]
    fn prune_keeps_the_newest_sessions_and_ignores_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for stamp in [
            "2026-01-01T00-00-00",
            "2026-02-01T00-00-00",
            "2026-03-01T00-00-00",
            "2026-04-01T00-00-00",
        ] {
            fs::write(root.join(format!("{FILE_PREFIX}{stamp}{FILE_SUFFIX}")), b"{}").unwrap();
        }
        fs::write(root.join("unrelated.txt"), b"keep me").unwrap();

        prune_old_sessions(root, 2);

        assert!(!root.join("session-2026-01-01T00-00-00.jsonl").exists());
        assert!(!root.join("session-2026-02-01T00-00-00.jsonl").exists());
        assert!(root.join("session-2026-03-01T00-00-00.jsonl").exists());
        assert!(root.join("session-2026-04-01T00-00-00.jsonl").exists());
        assert!(root.join("unrelated.txt").exists());
    }

    #[test]
    fn env_parsing_defaults_to_off() {
        // Guard against a truthy-looking value silently enabling the log.
        for value in ["", "0", "off", "false", "silly", "verbose"] {
            std::env::set_var(LEVEL_ENV, value);
            assert!(!enabled_from_env(), "{value:?} should not enable the log");
        }
        for value in ["1", "on", "true", "YES"] {
            std::env::set_var(LEVEL_ENV, value);
            assert!(enabled_from_env(), "{value:?} should enable the log");
        }
        std::env::remove_var(LEVEL_ENV);
        assert!(!enabled_from_env());
    }
}
