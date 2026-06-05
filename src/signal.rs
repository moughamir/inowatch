use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

static STOPPED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: i32) {
    STOPPED.store(true, Ordering::Relaxed);
}

pub fn is_stopped() -> bool {
    STOPPED.load(Ordering::Relaxed)
}

pub fn install_handlers() -> io::Result<()> {
    unsafe {
        let handler = handle_signal as *const () as libc::sighandler_t;
        if libc::signal(libc::SIGINT, handler) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
        if libc::signal(libc::SIGTERM, handler) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
        if libc::signal(libc::SIGPIPE, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        assert!(!is_stopped());
    }

    #[test]
    fn test_install_handlers() {
        let result = install_handlers();
        assert!(result.is_ok());
    }
}
