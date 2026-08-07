use mio::unix::SourceFd;
use mio::{Interest, Registry, Token};
use std::os::fd::RawFd;

/// A mio event source that becomes readable when one of the specified signals is received.
/// Uses Linux `signalfd` — signals are blocked via `sigprocmask` so they are
/// delivered through the fd rather than as asynchronous signals.
pub struct SignalSource(RawFd);

impl SignalSource {
    pub fn new(signals: &[libc::c_int]) -> Self {
        unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            for &sig in signals {
                libc::sigaddset(&mut mask, sig);
            }
            libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
            assert!(fd >= 0);
            Self(fd)
        }
    }

    /// Drain all pending signal notifications so a subsequent signal triggers a
    /// new readable edge. Needed when this source is used for events that recur
    /// (e.g. SIGUSR1 ping requests) rather than one-shot shutdown signals.
    pub fn drain(&self) {
        unsafe {
            let mut buf = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
            while libc::read(self.0, buf.as_mut_ptr() as *mut libc::c_void, std::mem::size_of::<libc::signalfd_siginfo>()) > 0 {}
        }
    }
}

impl std::os::unix::io::AsRawFd for SignalSource {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for SignalSource {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

impl mio::event::Source for SignalSource {
    fn register(&mut self, registry: &Registry, token: Token, interests: Interest) -> std::io::Result<()> {
        SourceFd(&self.0).register(registry, token, interests)
    }
    fn reregister(&mut self, registry: &Registry, token: Token, interests: Interest) -> std::io::Result<()> {
        SourceFd(&self.0).reregister(registry, token, interests)
    }
    fn deregister(&mut self, registry: &Registry) -> std::io::Result<()> {
        SourceFd(&self.0).deregister(registry)
    }
}
