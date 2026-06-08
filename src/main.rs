mod types;
mod watch;
mod coalesce;
mod emit;
mod signal;
mod mcp;

use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(name = "fwd", version = VERSION, about)]
struct Args {
    /// Directories/files to watch (not needed in --mcp mode).
    #[arg(required_unless_present = "mcp")]
    paths: Vec<PathBuf>,

    /// Fork into background (daemon mode).
    #[arg(short = 'd', long)]
    daemon: bool,

    /// Write PID to this file (implies --daemon).
    #[arg(short = 'p', long)]
    pidfile: Option<PathBuf>,

    /// Event debounce/coalescing window in milliseconds.
    #[arg(long, default_value = "500")]
    debounce: u64,

    /// Do not watch subdirectories recursively.
    #[arg(long)]
    no_recursive: bool,

    /// Suppress the startup banner record.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// MCP (Model Context Protocol) mode — speak JSON-RPC 2.0 over stdin/stdout.
    #[arg(long, conflicts_with_all = &["daemon", "pidfile", "quiet"])]
    mcp: bool,
}

fn main() {
    if let Err(e) = signal::install_handlers() {
        eprintln!("Error: failed to block signals: {}", e);
        std::process::exit(1);
    }

    let args = Args::parse();

    // ── MCP mode: serve Model Context Protocol over stdin/stdout ──────────
    if args.mcp {
        let mut server = mcp::McpServer::new();
        if let Err(e) = server.serve() {
            eprintln!("MCP server error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // ── NDJSON mode: watch files and emit events to stdout ────────────────

    if args.debounce == 0 {
        eprintln!("Error: debounce must be > 0");
        std::process::exit(1);
    }

    let pidfile_path = if args.daemon || args.pidfile.is_some() {
        let pf = args.pidfile.clone();
        daemonize(pf.as_deref());
        pf
    } else {
        None
    };

    let pid = std::process::id();

    let recursive = !args.no_recursive;
    let mut watcher = match watch::Watcher::new(recursive) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Error: failed to create inotify watcher: {}", e);
            cleanup_pidfile(pidfile_path.as_deref());
            std::process::exit(1);
        }
    };

    let watch_paths: Vec<String> = args.paths.iter().map(|p| p.display().to_string()).collect();
    for path in &args.paths {
        if let Err(e) = watcher.add_watch(path) {
            eprintln!("Warning: could not watch {}: {}", path.display(), e);
        }
    }

    check_watch_limit(watcher.watch_count());

    let (raw_tx, raw_rx) = mpsc::channel::<watch::RawEvent>();
    let (batch_tx, batch_rx) = mpsc::channel::<types::Batch>();

    let stop = Arc::new(AtomicBool::new(false));

    let watcher_handle = {
        let stop = stop.clone();
        let raw_tx = raw_tx.clone();
        thread::Builder::new()
            .name("fwd-watcher".into())
            .spawn(move || {
                if let Err(e) = watch::run_event_loop(&mut watcher, raw_tx, stop) {
                    eprintln!("Watcher error: {}", e);
                }
            })
            .expect("failed to spawn watcher thread")
    };

    let coalescer_handle = {
        thread::Builder::new()
            .name("fwd-coalescer".into())
            .spawn(move || {
                let mut coalescer =
                    coalesce::Coalescer::new(raw_rx, batch_tx, args.debounce);
                coalescer.run();
            })
            .expect("failed to spawn coalescer thread")
    };

    if !args.quiet {
        let banner = types::Banner::new(VERSION, pid, &watch_paths, args.debounce);
        let json = serde_json::to_string(&banner).unwrap_or_default();
        println!("{}", json);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    let emitter_handle = {
        thread::Builder::new()
            .name("fwd-emitter".into())
            .spawn(move || {
                let stdout = std::io::stdout();
                let mut emitter = emit::Emitter::new(stdout.lock(), batch_rx);
                if let Err(e) = emitter.run() {
                    eprintln!("Emitter error: {}", e);
                }
            })
            .expect("failed to spawn emitter thread")
    };

    while !signal::is_stopped() {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    stop.store(true, Ordering::Relaxed);
    drop(raw_tx);

    let _ = thread::scope(|s| {
        s.spawn(|| {
            let _ = watcher_handle.join();
        });
        s.spawn(|| {
            let _ = coalescer_handle.join();
        });
        s.spawn(|| {
            let _ = emitter_handle.join();
        });
    });

    cleanup_pidfile(pidfile_path.as_deref());
    std::process::exit(0);
}

/// Daemonize the process via double-fork.
fn daemonize(pidfile: Option<&std::path::Path>) {
    match unsafe { libc::fork() } {
        -1 => {
            eprintln!("Error: first fork failed");
            std::process::exit(1);
        }
        0 => {}
        _ => std::process::exit(0),
    }

    unsafe {
        libc::setsid();
    }

    match unsafe { libc::fork() } {
        -1 => {
            eprintln!("Error: second fork failed");
            std::process::exit(1);
        }
        0 => {}
        _ => std::process::exit(0),
    }

    unsafe {
        libc::chdir("/\0".as_ptr() as *const libc::c_char);
    }

    let devnull = std::fs::File::open("/dev/null").expect("cannot open /dev/null");
    let fd = devnull.as_raw_fd();
    unsafe {
        libc::dup2(fd, libc::STDIN_FILENO);
    }

    if let Some(pf) = pidfile {
        let pid = std::process::id();
        if let Err(e) = std::fs::write(pf, format!("{}\n", pid)) {
            eprintln!("Warning: could not write pidfile {}: {}", pf.display(), e);
        }
    }
}

/// Clean up the pidfile on exit.
fn cleanup_pidfile(path: Option<&std::path::Path>) {
    if let Some(pf) = path {
        let _ = std::fs::remove_file(pf);
    }
}

/// Check if the inotify max_user_watches limit might be an issue.
fn check_watch_limit(required: usize) {
    let limit_path = "/proc/sys/fs/inotify/max_user_watches";
    match std::fs::read_to_string(limit_path) {
        Ok(s) => {
            let current: usize = s.trim().parse().unwrap_or(0);
            if required > current {
                eprintln!(
                    "Warning: watching {} directories exceeds fs.inotify.max_user_watches ({})",
                    required, current
                );
                eprintln!(
                    "  Increase it with: sudo sh -c 'echo {} > {}'",
                    required + required / 2,
                    limit_path
                );
            }
        }
        Err(_) => {}
    }
}

use std::os::unix::io::AsRawFd;
