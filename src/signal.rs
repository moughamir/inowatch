use std::io;

/// Represents a received signal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Signal {
    Terminate,
    Interrupt,
    BrokenPipe,
    Unknown(i32),
}

impl Signal {
    fn from_raw(sig: i32) -> Self {
        match sig {
            libc::SIGTERM => Signal::Terminate,
            libc::SIGINT => Signal::Interrupt,
            libc::SIGPIPE => Signal::BrokenPipe,
            other => Signal::Unknown(other),
        }
    }
}

/// Block the signals we handle (SIGTERM, SIGINT, SIGPIPE) in the current
/// and all subsequently spawned threads.
///
/// Call this once at the start of `main()` before spawning any threads.
/// Then use `wait_for_signal()` in the main thread to receive signals.
pub fn block_signals() -> io::Result<()> {
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGPIPE);
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Wait for a signal. Blocks until one of our handled signals is received.
/// Must only be called after `block_signals()`.
pub fn wait_for_signal() -> Signal {
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGPIPE);
    }
    let mut sig: i32 = 0;
    let ret = unsafe { libc::sigwait(&set, &mut sig) };
    if ret != 0 {
        return Signal::Unknown(ret);
    }
    Signal::from_raw(sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_from_raw() {
        assert_eq!(Signal::from_raw(libc::SIGTERM), Signal::Terminate);
        assert_eq!(Signal::from_raw(libc::SIGINT), Signal::Interrupt);
        assert_eq!(Signal::from_raw(libc::SIGPIPE), Signal::BrokenPipe);
        assert_eq!(Signal::from_raw(99), Signal::Unknown(99));
    }

    #[test]
    fn test_block_signals() {
        // Should succeed without error.
        let result = block_signals();
        assert!(result.is_ok());
    }
}
