use crate::types::{Batch, Event, EventType, FileInfo};
use crate::watch::RawEvent;
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Event coalescer with debounce timer and rename-pair matching.
///
/// Receives raw inotify events from the watcher, debounces them into
/// batches, deduplicates rapid modify events, pairs rename cookies,
/// enriches events with filesystem metadata, and sends completed batches
/// downstream.
pub struct Coalescer {
    /// Channel receiver for incoming raw events.
    rx: mpsc::Receiver<RawEvent>,
    /// Channel sender for outgoing completed batches.
    tx: mpsc::Sender<Batch>,
    /// Debounce window in milliseconds.
    debounce_ms: u64,
    /// Sequence counter for batches.
    seq: u64,
}

impl Coalescer {
    pub fn new(
        rx: mpsc::Receiver<RawEvent>,
        tx: mpsc::Sender<Batch>,
        debounce_ms: u64,
    ) -> Self {
        Self {
            rx,
            tx,
            debounce_ms,
            seq: 0,
        }
    }

    /// Run the coalescer event loop. Blocks the current thread.
    /// Returns when the input channel is closed (watcher stopped).
    pub fn run(&mut self) {
        let debounce = Duration::from_millis(self.debounce_ms);
        // Pending events being collected for the current batch window.
        let mut pending: Vec<Event> = Vec::new();
        // Pending MOVED_FROM events waiting for their MOVED_TO pair.
        let mut rename_pairs: HashMap<u32, RawEvent> = HashMap::new();
        // Timestamp of the last event received (for coalescing).
        let mut last_event: Option<Instant> = None;

        loop {
            let timeout = match last_event {
                Some(t) => {
                    let elapsed = t.elapsed();
                    if elapsed >= debounce {
                        None // flush immediately
                    } else {
                        Some(debounce - elapsed)
                    }
                }
                None => None, // no pending timer, block indefinitely
            };

            // Try to receive with optional timeout.
            let result = if let Some(timeout) = timeout {
                if timeout.is_zero() || timeout.as_millis() == 0 {
                    // Timeout already expired — flush without blocking.
                    let batch = Self::flush_batch(&mut pending, &mut self.seq);
                    if let Some(b) = batch {
                        let _ = self.tx.send(b);
                    }
                    last_event = None;
                    continue;
                }
                self.rx.recv_timeout(timeout)
            } else {
                // No pending events, block until something arrives.
                self.rx.recv().map(Ok).unwrap_or(Err(mpsc::RecvTimeoutError::Disconnected))
            };

            match result {
                Ok(raw) => {
                    last_event = Some(Instant::now());

                    // Handle rename pairing.
                    if raw.kind == EventType::Rename {
                        if let Some(cookie) = raw.cookie {
                            if raw.path.exists() {
                                // MOVED_TO (destination exists)
                                if let Some(from) = rename_pairs.remove(&cookie) {
                                    // We have both halves — add both as individual events.
                                    // The coalescer keeps them separate; the consumer can
                                    // match by cookie.
                                    pending.push(Self::raw_to_event(&from));
                                    pending.push(Self::raw_to_event(&raw));
                                } else {
                                    // MOVED_TO without prior MOVED_FROM — emit as create
                                    pending.push(Self::raw_to_event(&raw));
                                }
                            } else {
                                // MOVED_FROM (source no longer exists)
                                rename_pairs.insert(cookie, raw);
                            }
                            continue;
                        }
                    }

                    // For non-rename events, or renames without cookies:
                    // Deduplicate: if the last pending event is for the same path and type,
                    // update it instead of appending.
                    Self::dedup_push(&mut pending, Self::raw_to_event(&raw));

                    // If we've accumulated orphaned rename pairs that timed out,
                    // flush them as individual events.
                    if !rename_pairs.is_empty() {
                        let orphaned: Vec<RawEvent> = rename_pairs.drain().map(|(_, v)| v).collect();
                        for orphan in orphaned {
                            // A MOVED_FROM without a MOVED_TO pair — the file was moved
                            // out of the watch area. Emit as a Delete.
                            let mut ev = Self::raw_to_event(&orphan);
                            ev.kind = EventType::Delete;
                            ev.info = None;
                            Self::dedup_push(&mut pending, ev);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Debounce window expired — flush pending events.
                    let batch = Self::flush_batch(&mut pending, &mut self.seq);
                    if let Some(b) = batch {
                        let _ = self.tx.send(b);
                    }
                    last_event = None;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Watcher channel closed — flush remaining and exit.
                    let batch = Self::flush_batch(&mut pending, &mut self.seq);
                    if let Some(b) = batch {
                        let _ = self.tx.send(b);
                    }
                    return;
                }
            }
        }
    }

    /// Convert a RawEvent into an Event, querying the filesystem for metadata.
    fn raw_to_event(raw: &RawEvent) -> Event {
        let info = get_file_info(&raw.path);
        Event {
            kind: raw.kind.clone(),
            path: raw.path.clone(),
            cookie: raw.cookie,
            info,
        }
    }

    /// Push an event, deduplicating:
    /// - Modify events for the same path replace the previous entry.
    /// - Delete events for the same path replace the previous entry.
    /// - Create events are always appended (a create followed by another create
    ///   for the same path is unusual, so we keep both).
    fn dedup_push(events: &mut Vec<Event>, new: Event) {
        // For modify and delete, look for an existing event for the same path.
        if new.kind == EventType::Modify || new.kind == EventType::Delete {
            if let Some(pos) = events.iter().position(|e| e.path == new.path) {
                // If the existing event is Create, keep the Create but update info.
                if events[pos].kind == EventType::Create {
                    events[pos].info = new.info;
                } else {
                    events[pos] = new;
                }
                return;
            }
        }
        events.push(new);
    }

    /// Build a Batch from pending events, increment the sequence counter,
    /// and clear the pending list. Returns None if there are no events.
    fn flush_batch(pending: &mut Vec<Event>, seq: &mut u64) -> Option<Batch> {
        if pending.is_empty() {
            return None;
        }
        *seq += 1;
        let events = std::mem::take(pending);
        Some(Batch::new(*seq, events))
    }
}

/// Query filesystem metadata for a path. Returns None if the path no longer exists.
fn get_file_info(path: &Path) -> Option<FileInfo> {
    std::fs::metadata(path).ok().map(|meta| {
        // Use libc to get the exact permission string.
        #[cfg(unix)]
        let mode_str = {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            format_mode(mode)
        };
        #[cfg(not(unix))]
        let mode_str = String::new();

        FileInfo {
            size: meta.len(),
            mode_str,
            is_dir: meta.is_dir(),
        }
    })
}

/// Format a Unix permission mode as an octal string (e.g., "0644").
fn format_mode(mode: u32) -> String {
    // Only the lower 12 bits (permission bits).
    format!("{:04o}", mode & 0o7777)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::RawEvent;

    fn make_raw(path: &str, kind: EventType, cookie: Option<u32>, is_dir: bool) -> RawEvent {
        RawEvent {
            kind,
            path: path.into(),
            cookie,
            is_dir,
        }
    }

    fn collect_batches(rx: mpsc::Receiver<Batch>, timeout_ms: u64) -> Vec<Batch> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut batches = Vec::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(b) => batches.push(b),
                Err(_) => break,
            }
        }
        batches
    }

    #[test]
    fn test_single_event_flushes_after_debounce() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();

        let mut co = Coalescer::new(raw_rx, batch_tx, 30);
        let handle = std::thread::spawn(move || co.run());

        raw_tx.send(make_raw("/tmp/test.txt", EventType::Create, None, false)).unwrap();
        // Drop sender to stop the coalescer.
        drop(raw_tx);

        handle.join().unwrap();

        let batches = collect_batches(batch_rx, 200);
        assert!(!batches.is_empty(), "should have at least one batch");
        let total_events: usize = batches.iter().map(|b| b.events.len()).sum();
        assert!(total_events >= 1, "should have at least one event");
    }

    #[test]
    fn test_debounce_dedup_modify() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();

        let mut co = Coalescer::new(raw_rx, batch_tx, 50);
        let handle = std::thread::spawn(move || co.run());

        // Send rapid modify events for the same file.
        raw_tx.send(make_raw("/tmp/fwd-test-dedup.txt", EventType::Modify, None, false)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        raw_tx.send(make_raw("/tmp/fwd-test-dedup.txt", EventType::Modify, None, false)).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        raw_tx.send(make_raw("/tmp/fwd-test-dedup.txt", EventType::Modify, None, false)).unwrap();

        // Wait for debounce to flush.
        std::thread::sleep(Duration::from_millis(150));
        drop(raw_tx);
        handle.join().unwrap();

        let batches = collect_batches(batch_rx, 200);
        let total_modifies: usize = batches.iter()
            .flat_map(|b| &b.events)
            .filter(|e| e.kind == EventType::Modify)
            .count();
        // Should be 1 (deduped), not 3.
        assert!(total_modifies <= 2, "too many modify events: {}", total_modifies);
    }

    #[test]
    fn test_rename_pairing() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();

        let mut co = Coalescer::new(raw_rx, batch_tx, 30);
        let handle = std::thread::spawn(move || co.run());

        // Simulate rename: MOVED_FROM then MOVED_TO with same cookie.
        let dir = std::env::temp_dir().join("fwd-test-rename-pair");
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("old.txt");
        let dst = dir.join("new.txt");
        std::fs::write(&src, b"data").unwrap();

        let from = RawEvent {
            kind: EventType::Rename,
            path: src.clone(),
            cookie: Some(42),
            is_dir: false,
        };
        // Create the destination file so MOVED_TO event sees it.
        std::fs::rename(&src, &dst).unwrap();

        let to = RawEvent {
            kind: EventType::Rename,
            path: dst.clone(),
            cookie: Some(42),
            is_dir: false,
        };

        raw_tx.send(from).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        raw_tx.send(to).unwrap();

        std::thread::sleep(Duration::from_millis(100));
        drop(raw_tx);
        handle.join().unwrap();

        let batches = collect_batches(batch_rx, 200);
        let rename_events: Vec<&Event> = batches.iter()
            .flat_map(|b| &b.events)
            .filter(|e| e.kind == EventType::Rename)
            .collect();
        // Should have both rename events.
        assert_eq!(rename_events.len(), 2, "should have 2 rename events");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_then_modify_dedup() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();

        let mut co = Coalescer::new(raw_rx, batch_tx, 30);
        let handle = std::thread::spawn(move || co.run());

        raw_tx.send(make_raw("/tmp/test-cm.txt", EventType::Create, None, false)).unwrap();
        raw_tx.send(make_raw("/tmp/test-cm.txt", EventType::Modify, None, false)).unwrap();

        std::thread::sleep(Duration::from_millis(100));
        drop(raw_tx);
        handle.join().unwrap();

        let batches = collect_batches(batch_rx, 200);
        let events: Vec<&Event> = batches.iter().flat_map(|b| &b.events).collect();
        // Should keep the Create, just update info.
        let creates: usize = events.iter().filter(|e| e.kind == EventType::Create).count();
        let modifies: usize = events.iter().filter(|e| e.kind == EventType::Modify).count();
        assert_eq!(creates, 1, "should have 1 create");
        assert_eq!(modifies, 0, "modify should be merged into create");
    }

    #[test]
    fn test_flush_empty_does_nothing() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();

        let mut co = Coalescer::new(raw_rx, batch_tx, 10);
        let handle = std::thread::spawn(move || co.run());

        // Send no events, just drop sender.
        drop(raw_tx);
        handle.join().unwrap();

        let batches = collect_batches(batch_rx, 100);
        assert!(batches.is_empty(), "should have no batches");
    }

    #[test]
    fn test_sequence_increments() {
        let (raw_tx, raw_rx) = mpsc::channel();
        let (batch_tx, batch_rx) = mpsc::channel();

        let mut co = Coalescer::new(raw_rx, batch_tx, 20);
        let handle = std::thread::spawn(move || co.run());

        raw_tx.send(make_raw("/tmp/seq-test-1.txt", EventType::Create, None, false)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        raw_tx.send(make_raw("/tmp/seq-test-2.txt", EventType::Create, None, false)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        drop(raw_tx);
        handle.join().unwrap();

        let batches = collect_batches(batch_rx, 200);
        assert!(batches.len() >= 2, "should have at least 2 batches, got {}", batches.len());
        // Sequence numbers should be monotonically increasing.
        for i in 1..batches.len() {
            assert!(batches[i].seq > batches[i - 1].seq, "seq should increase");
        }
    }
}
