//! Client-local progress for OpenSSH authentication, including agent signing.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SshConnectionProgress {
    WaitingForKey,
    Connecting,
}

#[derive(Clone)]
pub(crate) struct SshConnectionMonitor {
    pub(crate) cancelled: Arc<AtomicBool>,
    report: Arc<dyn Fn(SshConnectionProgress) + Send + Sync>,
}

impl SshConnectionMonitor {
    pub(crate) fn new(
        cancelled: Arc<AtomicBool>,
        report: impl Fn(SshConnectionProgress) + Send + Sync + 'static,
    ) -> Self {
        Self {
            cancelled,
            report: Arc::new(report),
        }
    }

    pub(super) fn report(&self, progress: SshConnectionProgress) {
        (self.report)(progress);
    }

    pub(super) fn check_cancelled(&self) -> std::io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "SSH connection cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SshDiagnosticProgress {
    Signing,
    PresenceRequired,
    SignatureSent,
    Authenticated,
}

pub(super) fn diagnostic_progress(line: &str) -> Option<SshDiagnosticProgress> {
    // OpenSSH requests an agent signature only after the server accepts the key.
    // Agent signing can block for touch without producing a presence diagnostic.
    if line.starts_with("debug1: Server accepts key:") {
        Some(SshDiagnosticProgress::Signing)
    } else if line.starts_with("Confirm user presence for key ") {
        Some(SshDiagnosticProgress::PresenceRequired)
    } else if line == "debug3: send packet: type 50" || line.starts_with("User presence confirmed")
    {
        Some(SshDiagnosticProgress::SignatureSent)
    } else if line.starts_with("Authenticated to ")
        || line.starts_with("debug1: Authentication succeeded")
    {
        Some(SshDiagnosticProgress::Authenticated)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_is_observed_for_both_agent_and_direct_security_keys() {
        for line in [
            "debug1: Server accepts key: token ED25519-SK SHA256:example",
            "debug1: Server accepts key: token ED25519 SHA256:example agent",
        ] {
            assert_eq!(
                diagnostic_progress(line),
                Some(SshDiagnosticProgress::Signing)
            );
        }
        assert_eq!(
            diagnostic_progress("debug1: Offering public key: token"),
            None
        );
        assert_eq!(diagnostic_progress("debug1: Connecting to host"), None);
        assert_eq!(diagnostic_progress("Permission denied (publickey)."), None);
        assert_eq!(
            diagnostic_progress("Confirm user presence for key ED25519-SK example"),
            Some(SshDiagnosticProgress::PresenceRequired)
        );
        assert_eq!(
            diagnostic_progress("Authenticated to host using publickey."),
            Some(SshDiagnosticProgress::Authenticated)
        );
    }
}
