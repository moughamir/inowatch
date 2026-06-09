# Windows Support Implementation Plan

## Problem

`inowatch` is Linux-only due to inotify and POSIX-specific system calls:

- **`src/watch.rs`**: inotify initialization, watch descriptors, and raw `libc` syscalls.
- **`src/signal.rs`**: POSIX `signal()` for SIGINT/SIGTERM/SIGPIPE.
- **`src/main.rs`**: Unix double-fork daemonisation, `/dev/null` redirection, `/proc` inotify limit check.

## Approach

Add a **cross-platform filesystem watching backend for Windows** while preserving the existing inotify implementation for Linux.

The `notify` crate (v7.x) provides a `RecommendedWatcher` that uses `ReadDirectoryChangesW` on Windows. We will wrap it behind a `WatcherImpl` trait so the rest of the codebase does not change.

## Target Architecture

```rust
trait WatcherImpl
  ├─ InotifyWatcher    (Linux — existing code, gated by #[cfg(target_os = "linux")])
  └─ NotifyWatcher     (Windows — new code, gated by #[cfg(target_os = "windows")])
pub struct Watcher {
    inner: Box<dyn WatcherImpl>,
}
```

## Files to Change

| File                  | Change                                                                                                  |
| --------------------- | ------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`          | Add `notify` (v7.x) Windows-only dependency.                                                            |
| `src/watch_notify.rs` | **NEW** — `NotifyWatcher` implementing `WatcherImpl` via `notify::RecommendedWatcher`.                  |
| `src/watch.rs`        | Extract `WatcherImpl` trait, move inotify code into Linux-gated impl, add `pub struct Watcher` wrapper. |
| `src/signal.rs`       | Gate POSIX `signal()` calls behind `#[cfg(unix)]`; add Windows stub (Ctrl+C default is sufficient).     |
| `src/main.rs`         | Gate `daemonize()` and `check_watch_limit()` behind `#[cfg(unix)]`; show warning on Windows.            |
| `src/mcp.rs`          | Update tool description from "using inotify" to "filesystem changes".                                   |
| `README.md`           | Note Windows support and Unix-only daemon mode.                                                         |

## Implementation Notes

- The `notify` crate does **not** automatically recurse into subdirectories. `NotifyWatcher::add_watch` must manually traverse the directory tree and call `watcher.watch()` for each subdirectory, keeping an internal `HashMap<path, watch_handle>`.
- Linux tests under `#[cfg(test)]` will remain Linux-only (gated by target OS).
- `RawEvent::cookie` will be `None` on Windows; rename pairing is not available via all `notify` backends.
- `mode_str` in `FileInfo` will remain `""` on Windows (already handled via `#[cfg(not(unix))]` in `coalesce.rs`).

## Verification

1. `cargo build` on Linux — must pass (zero regression).
2. `cargo check --target x86_64-pc-windows-gnu` — must pass.
3. Optional Windows CI: add `windows-latest` to GitHub Actions matrix (requires adding `.github/workflows/ci.yml`).
