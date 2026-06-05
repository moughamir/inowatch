use crate::types::EventType;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

/// Size of the buffer for reading inotify events — enough for many events per read.
const EVENT_BUF_SIZE: usize = 65_536;
/// Maximum directory recursion depth.
const MAX_DEPTH: usize = 64;
/// inotify watch mask.
const IN_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MODIFY
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_ATTRIB
    | libc::IN_EXCL_UNLINK;

/// A raw event from the inotify subsystem, before any coalescing.
#[derive(Debug, Clone)]
pub struct RawEvent {
    pub kind: EventType,
    pub path: PathBuf,
    pub cookie: Option<u32>,
    pub is_dir: bool,
}

/// The inotify-based directory watcher.
pub struct Watcher {
    fd: RawFd,
    /// Map from watch descriptor → canonical path being watched.
    wd_to_path: HashMap<i32, PathBuf>,
    /// Map from path → watch descriptor (for dedup).
    path_to_wd: HashMap<PathBuf, i32>,
    /// Whether to watch recursively.
    recursive: bool,
}

impl Watcher {
    /// Create a new watcher. Opens the inotify fd.
    pub fn new(recursive: bool) -> std::io::Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            wd_to_path: HashMap::new(),
            path_to_wd: HashMap::new(),
            recursive,
        })
    }

    /// The raw inotify file descriptor.
    #[allow(dead_code)]
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Add a watch on `path`. If `recursive` is true and path is a directory,
    /// watches the entire subtree (up to MAX_DEPTH).
    pub fn add_watch<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        if !path.exists() {
            eprintln!("Warning: path does not exist, skipping: {}", path.display());
            return Ok(());
        }

        let canonical = std::fs::canonicalize(path)?;

        if self.recursive && canonical.is_dir() {
            self.add_recursive(&canonical, 0)
        } else {
            self.add_single_watch(&canonical)?;
            Ok(())
        }
    }

    fn add_recursive(&mut self, dir: &Path, depth: usize) -> std::io::Result<()> {
        if depth > MAX_DEPTH {
            eprintln!(
                "Warning: max depth ({}) reached at {}, skipping deeper entries",
                MAX_DEPTH,
                dir.display()
            );
            return Ok(());
        }

        self.add_single_watch(dir)?;

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Warning: cannot read directory {}: {}", dir.display(), e);
                return Ok(());
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };

            if file_type.is_dir() {
                // Skip symlinks to directories
                if file_type.is_symlink() {
                    continue;
                }
                self.add_recursive(&path, depth + 1)?;
            }
            // Regular files, FIFOs, sockets etc. are not added as watches —
            // only directories need inotify watches.
        }

        Ok(())
    }

    fn add_single_watch(&mut self, path: &Path) -> std::io::Result<Option<i32>> {
        // Skip non-directories (inotify only watches directories for child events,
        // but we also want to watch individual files if not recursive).
        // Actually inotify can watch any filesystem object, but for recursive
        // mode we only need directories.
        if self.recursive && !path.is_dir() {
            return Ok(None);
        }

        // Deduplicate: if already watching, return existing wd.
        if let Some(&wd) = self.path_to_wd.get(path) {
            return Ok(Some(wd));
        }

        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains null byte"))?;

        let wd = unsafe { libc::inotify_add_watch(self.fd, cpath.as_ptr(), IN_MASK) };
        if wd == -1 {
            let err = std::io::Error::last_os_error();
            eprintln!("Warning: failed to add watch on {}: {}", path.display(), err);
            return Err(err);
        }

        let canonical = path.to_path_buf();
        self.wd_to_path.insert(wd, canonical.clone());
        self.path_to_wd.insert(canonical, wd);
        Ok(Some(wd))
    }

    /// Remove a watch by watch descriptor.
    pub fn remove_watch(&mut self, wd: i32) {
        unsafe {
            libc::inotify_rm_watch(self.fd, wd);
        }
        if let Some(path) = self.wd_to_path.remove(&wd) {
            self.path_to_wd.remove(&path);
        }
    }

    /// Remove a watch by path.
    pub fn remove_watch_by_path(&mut self, path: &Path) {
        if let Some(&wd) = self.path_to_wd.get(path) {
            self.remove_watch(wd);
        }
    }

    const POLL_TIMEOUT_MS: i32 = 500;

    /// Read available inotify events from the fd.
    /// Uses `poll()` with a timeout so the caller can detect inactivity.
    /// Returns `Ok(None)` on timeout (no events available).
    /// Returns `Ok(Some(events))` when events are read.
    pub fn read_events_poll(&self) -> std::io::Result<Option<Vec<RawEvent>>> {
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, Self::POLL_TIMEOUT_MS) };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if ret == 0 {
            return Ok(None);
        }
        self.read_events_raw().map(Some)
    }

    /// Actually read inotify events from the fd (blocking, assumes data is ready).
    fn read_events_raw(&self) -> std::io::Result<Vec<RawEvent>> {
        let mut buf = [0u8; EVENT_BUF_SIZE];
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, EVENT_BUF_SIZE) };
        if n == -1 {
            return Err(std::io::Error::last_os_error());
        }
        if n == 0 {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        let mut offset = 0;
        while offset < n as usize {
            // SAFETY: inotify guarantees the buffer contains aligned inotify_event structs.
            let event = unsafe { &*(buf[offset..].as_ptr() as *const libc::inotify_event) };
            let event_size = std::mem::size_of::<libc::inotify_event>() + event.len as usize;

            if event.mask & libc::IN_Q_OVERFLOW != 0 {
                eprintln!("Warning: inotify event queue overflow — events may have been lost");
                offset += event_size;
                continue;
            }

            // Get the filename from the event (may be empty — the event is for the watched dir itself).
            let name_bytes = if event.len > 0 {
                let name_ptr = buf[offset..].as_ptr() as *const u8;
                // name starts after the fixed struct
                let name_start = unsafe { name_ptr.add(std::mem::size_of::<libc::inotify_event>()) };
                let name_slice = unsafe { std::slice::from_raw_parts(name_start, event.len as usize) };
                // Name may be padded with null bytes — trim them.
                let end = name_slice.iter().position(|&b| b == 0).unwrap_or(name_slice.len());
                &name_slice[..end]
            } else {
                &[] // event for the watched directory itself
            };

            // Build the full path.
            let dir_path = self.wd_to_path.get(&event.wd);
            let full_path = match (dir_path, name_bytes.is_empty()) {
                (Some(dir), false) => {
                    let name_os = OsStr::from_bytes(name_bytes);
                    dir.join(name_os)
                }
                (Some(dir), true) => dir.clone(),
                (None, _) => {
                    // Watch descriptor not found — may have been removed.
                    offset += event_size;
                    continue;
                }
            };

            // Determine event type from mask.
            let kind = if event.mask & libc::IN_CREATE != 0 {
                EventType::Create
            } else if event.mask & libc::IN_DELETE != 0 {
                EventType::Delete
            } else if event.mask & libc::IN_MODIFY != 0 {
                EventType::Modify
            } else if event.mask & (libc::IN_MOVED_FROM | libc::IN_MOVED_TO) != 0 {
                EventType::Rename
            } else if event.mask & libc::IN_ATTRIB != 0 {
                // Treat attribute changes as modify events for simplicity.
                EventType::Modify
            } else {
                offset += event_size;
                continue;
            };

            let cookie = if event.cookie > 0 {
                Some(event.cookie)
            } else {
                None
            };

            let is_dir = event.mask & libc::IN_ISDIR != 0;

            events.push(RawEvent {
                kind,
                path: full_path,
                cookie,
                is_dir,
            });

            offset += event_size;
        }

        Ok(events)
    }

    /// Get the current number of active watches.
    pub fn watch_count(&self) -> usize {
        self.wd_to_path.len()
    }

    /// Get a reference to the wd→path map.
    #[allow(dead_code)]
    pub fn watched_paths(&self) -> &HashMap<i32, PathBuf> {
        &self.wd_to_path
    }
}

impl AsRawFd for Watcher {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

pub fn run_event_loop(
    watcher: &mut Watcher,
    sender: mpsc::Sender<RawEvent>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        let raw_events = match watcher.read_events_poll() {
            Ok(Some(events)) => events,
            Ok(None) => continue, // poll timed out, loop back to check stop
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("Error reading inotify events: {}", e);
                return Err(e);
            }
        };

        for event in &raw_events {
            if event.kind == EventType::Create && event.is_dir {
                if let Err(e) = watcher.add_watch(&event.path) {
                    eprintln!(
                        "Warning: failed to watch new directory {}: {}",
                        event.path.display(),
                        e
                    );
                }
            }

            if event.kind == EventType::Delete && event.is_dir {
                watcher.remove_watch_by_path(&event.path);
            }

            if sender.send(event.clone()).is_err() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_watcher() {
        let watcher = Watcher::new(true);
        assert!(watcher.is_ok());
    }

    #[test]
    fn test_add_watch_nonexistent() {
        let mut watcher = Watcher::new(false).unwrap();
        // Should not panic, just warn.
        let result = watcher.add_watch("/tmp/nonexistent-fwd-test-12345");
        assert!(result.is_ok());
    }

    #[test]
    fn test_add_watch_file() {
        let dir = std::env::temp_dir().join("fwd-test-file-watch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("test.txt");
        std::fs::write(&file, b"hello").unwrap();

        let mut watcher = Watcher::new(false).unwrap();
        watcher.add_watch(&file).unwrap();

        assert!(watcher.watch_count() >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_add_watch_recursive() {
        let dir = std::env::temp_dir().join("fwd-test-recursive");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir.join("a/b/c")).unwrap();
        std::fs::write(&dir.join("a/hello.txt"), b"hello").unwrap();

        let mut watcher = Watcher::new(true).unwrap();
        watcher.add_watch(&dir).unwrap();

        // Should have watches for dir, a, b, c — at least 4.
        assert!(watcher.watch_count() >= 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_events_create_file() {
        let dir = std::env::temp_dir().join("fwd-test-read-events");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut watcher = Watcher::new(true).unwrap();
        watcher.add_watch(&dir).unwrap();

        std::fs::write(&dir.join("newfile.txt"), b"data").unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));

        let maybe_events = watcher.read_events_poll().unwrap();
        assert!(maybe_events.is_some(), "should have events available");
        let events = maybe_events.unwrap();
        assert!(!events.is_empty(), "should have at least one event");

        let has_create = events.iter().any(|e| e.kind == EventType::Create);
        assert!(has_create, "should contain a create event");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_watch_count_zero_after_remove() {
        let dir = std::env::temp_dir().join("fwd-test-remove-count");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut watcher = Watcher::new(false).unwrap();

        // Add a watch on the dir.
        watcher.add_watch(&dir).unwrap();
        assert!(watcher.watch_count() > 0);

        // Find the wd for this dir.
        let wd = *watcher.path_to_wd.get(&std::fs::canonicalize(&dir).unwrap()).unwrap();

        // Remove the watch.
        watcher.remove_watch(wd);
        assert_eq!(watcher.watch_count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
