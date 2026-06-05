use serde::Serialize;
use std::path::PathBuf;

/// The kind of filesystem event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Create,
    Modify,
    Delete,
    Rename,
}

/// Post-event filesystem metadata (absent on Delete events).
#[derive(Debug, Clone, Serialize)]
pub struct FileInfo {
    pub size: u64,
    #[serde(rename = "mode")]
    pub mode_str: String,
    pub is_dir: bool,
}

/// A single filesystem event.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: EventType,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cookie: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<FileInfo>,
}

/// A batch of one or more coalesced events, emitted as a single NDJSON line.
#[derive(Debug, Clone, Serialize)]
pub struct Batch {
    pub timestamp: String, // RFC 3339 with nanos
    pub seq: u64,
    pub events: Vec<Event>,
}

/// Startup banner — the first record emitted on the stream.
#[derive(Debug, Clone, Serialize)]
pub struct Banner {
    #[serde(rename = "type")]
    pub kind: String, // "banner"
    pub version: String,
    pub pid: u32,
    pub watches: Vec<String>,
    pub debounce_ms: u64,
}

impl Banner {
    pub fn new(version: &str, pid: u32, watches: &[String], debounce_ms: u64) -> Self {
        Self {
            kind: "banner".into(),
            version: version.into(),
            pid,
            watches: watches.to_vec(),
            debounce_ms,
        }
    }
}

impl Batch {
    pub fn new(seq: u64, events: Vec<Event>) -> Self {
        let timestamp = format_now();
        Self {
            timestamp,
            seq,
            events,
        }
    }
}

fn format_now() -> String {
    // Manual RFC 3339 formatting with nanos to avoid pulling in chrono.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();

    // We format as ISO 8601 / RFC 3339 using the UTC epoch offset.
    // This is a simplified approach — for production, chrono would be better.
    // Compute year/month/day from secs since epoch (ignore leap seconds).
    let (y, m, d, hh, mm, ss) = rfc3339_parts(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        y, m, d, hh, mm, ss, nanos
    )
}

/// Convert Unix timestamp (seconds since epoch) to UTC date/time.
fn rfc3339_parts(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    // Days since epoch.
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hh = time_secs / 3600;
    let mm = (time_secs % 3600) / 60;
    let ss = time_secs % 60;

    // Civil date from days since 1970-01-01.
    let (y, m, d) = civil_from_days(days as i64);
    (y as u64, m as u64, d as u64, hh, mm, ss)
}

/// Convert days since 1970-01-01 to (year, month, day).
/// Uses the algorithm from Howard Hinnant (public domain).
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month phase [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u64, d.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_banner_serialization() {
        let banner = Banner::new("0.1.0", 12345, &["/tmp/test".into()], 50);
        let json = serde_json::to_string(&banner).unwrap();
        assert!(json.contains("\"type\":\"banner\""));
        assert!(json.contains("\"version\":\"0.1.0\""));
        assert!(json.contains("\"pid\":12345"));
    }

    #[test]
    fn test_batch_serialization() {
        let event = Event {
            kind: EventType::Create,
            path: "/tmp/test/foo.txt".into(),
            cookie: None,
            info: Some(FileInfo {
                size: 1024,
                mode_str: "0644".into(),
                is_dir: false,
            }),
        };
        let batch = Batch::new(1, vec![event]);
        let json = serde_json::to_string(&batch).unwrap();
        assert!(json.contains("\"type\":\"create\""));
        assert!(json.contains("\"path\":\"/tmp/test/foo.txt\""));
        assert!(json.contains("\"seq\":1"));
    }

    #[test]
    fn test_event_type_serialization() {
        assert_eq!(
            serde_json::to_string(&EventType::Create).unwrap(),
            "\"create\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::Modify).unwrap(),
            "\"modify\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::Delete).unwrap(),
            "\"delete\""
        );
        assert_eq!(
            serde_json::to_string(&EventType::Rename).unwrap(),
            "\"rename\""
        );
    }

    #[test]
    fn test_rfc3339_format() {
        let formatted = format_now();
        // Should look like 2026-06-05T12:34:56.123456789Z
        assert!(formatted.ends_with('Z'));
        assert_eq!(formatted.chars().filter(|&c| c == '-').count(), 2);
        assert_eq!(formatted.chars().filter(|&c| c == ':').count(), 2);
        assert!(formatted.contains('T'));
    }

    #[test]
    fn test_civil_from_days() {
        // 1970-01-01
        let (y, m, d) = civil_from_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));

        // 2026-06-05 is about 20613 days from epoch (approx)
        let (y, m, _d) = civil_from_days(20613);
        assert_eq!((y, m), (2026, 6));

        // 2000-01-01
        let (y, m, d) = civil_from_days(10957);
        assert_eq!((y, m, d), (2000, 1, 1));
    }

    #[test]
    fn test_rename_cookie_serialization() {
        let event = Event {
            kind: EventType::Rename,
            path: "/old/path".into(),
            cookie: Some(42),
            info: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"cookie\":42"));
    }
}
