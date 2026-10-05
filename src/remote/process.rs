use super::progress::{
    diagnostic_progress, SshConnectionMonitor, SshConnectionProgress, SshDiagnosticProgress,
};
use std::io::{self, BufRead as _, Read as _};
use std::process::Output;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SIGNING_NOTICE_DELAY: Duration = Duration::from_millis(500);
const KEY_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn wait_with_output_timeout(
    mut child: std::process::Child,
    timeout: Duration,
    monitor: Option<&SshConnectionMonitor>,
) -> io::Result<Output> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("SSH command stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("SSH command stderr was not captured"))?;
    let stdout = thread::spawn(move || {
        let mut stdout = stdout;
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let (progress_tx, progress_rx) = std::sync::mpsc::channel();
    let observed = monitor.is_some();
    let stderr = thread::spawn(move || {
        let mut reader = io::BufReader::new(stderr);
        let mut bytes = Vec::new();
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 {
            let text = String::from_utf8_lossy(&line);
            if observed {
                if let Some(progress) = diagnostic_progress(text.trim_end()) {
                    let _ = progress_tx.send(progress);
                }
            }
            // Verbose SSH contains key paths and fingerprints. Use it only as
            // progress evidence; never include it in a user-facing diagnostic.
            if !observed
                || (!text.starts_with("debug1:")
                    && !text.starts_with("debug2:")
                    && !text.starts_with("debug3:")
                    && diagnostic_progress(text.trim_end()).is_none())
            {
                bytes.extend_from_slice(
                    &line[..line
                        .len()
                        .min((16 * 1024_usize).saturating_sub(bytes.len()))],
                );
            }
            line.clear();
        }
        Ok::<_, io::Error>(bytes)
    });
    let mut started = Instant::now();
    let mut signing_since = None;
    let mut notice_sent = false;
    let status = loop {
        for progress in progress_rx.try_iter() {
            match progress {
                SshDiagnosticProgress::Signing => {
                    signing_since = Some(Instant::now());
                    notice_sent = false;
                }
                SshDiagnosticProgress::PresenceRequired => {
                    signing_since = Some(Instant::now() - SIGNING_NOTICE_DELAY);
                    notice_sent = false;
                }
                SshDiagnosticProgress::SignatureSent | SshDiagnosticProgress::Authenticated => {
                    if progress == SshDiagnosticProgress::SignatureSent && signing_since.is_none() {
                        continue;
                    }
                    started = Instant::now();
                    signing_since = None;
                    notice_sent = false;
                    if let Some(monitor) = monitor {
                        monitor.report(SshConnectionProgress::Connecting);
                    }
                }
            }
        }
        if !notice_sent
            && signing_since.is_some_and(|since| since.elapsed() >= SIGNING_NOTICE_DELAY)
        {
            if let Some(monitor) = monitor {
                monitor.report(SshConnectionProgress::WaitingForKey);
            }
            notice_sent = true;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(error);
            }
        }
        let cancelled = monitor.and_then(|monitor| monitor.check_cancelled().err());
        let expired = signing_since.map_or_else(
            || started.elapsed() >= timeout,
            |since| since.elapsed() >= KEY_WAIT_TIMEOUT,
        );
        if cancelled.is_some() || expired {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout.join();
            let _ = stderr.join();
            return Err(cancelled.unwrap_or_else(|| {
                io::Error::new(
                    if notice_sent {
                        io::ErrorKind::PermissionDenied
                    } else {
                        io::ErrorKind::TimedOut
                    },
                    if notice_sent {
                        "SSH key confirmation timed out; click reconnect to try again"
                    } else {
                        "noninteractive SSH command timed out"
                    },
                )
            }));
        }
        thread::sleep(POLL_INTERVAL);
    };
    let stdout = stdout
        .join()
        .map_err(|_| io::Error::other("SSH stdout reader panicked"))??;
    let stderr = stderr
        .join()
        .map_err(|_| io::Error::other("SSH stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn timeout_kills_the_child() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("exec sleep 10")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let error =
            wait_with_output_timeout(command.spawn().unwrap(), Duration::from_millis(25), None)
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn agent_signing_reports_live_progress_and_extends_the_network_timeout() {
        use std::sync::{Arc, Mutex};
        let events = Arc::new(Mutex::new(Vec::new()));
        let reported = events.clone();
        let monitor = SshConnectionMonitor::new(
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            move |event| reported.lock().unwrap().push(event),
        );
        let child = Command::new("sh").arg("-c").arg("printf 'debug1: Server accepts key: private-path ED25519 example agent\\n' >&2; sleep 0.7; printf 'Authenticated to host using publickey.\\n' >&2; sleep 0.1; printf ready")
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let output =
            wait_with_output_timeout(child, Duration::from_millis(250), Some(&monitor)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"ready");
        assert!(output.stderr.is_empty());
        assert_eq!(
            *events.lock().unwrap(),
            [
                SshConnectionProgress::WaitingForKey,
                SshConnectionProgress::Connecting
            ]
        );
    }

    #[test]
    fn cancelling_key_confirmation_reaps_the_owned_child() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = cancelled.clone();
        let monitor = SshConnectionMonitor::new(cancelled, move |event| {
            assert_eq!(event, SshConnectionProgress::WaitingForKey);
            token.store(true, Ordering::Release);
        });
        let child = Command::new("sh")
            .arg("-c")
            .arg("printf 'Confirm user presence for key ED25519-SK example\\n' >&2; exec sleep 10")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let started = Instant::now();
        let error =
            wait_with_output_timeout(child, Duration::from_secs(2), Some(&monitor)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0);
    }

    #[test]
    fn a_sent_signature_restores_the_network_timeout() {
        use std::sync::{Arc, Mutex};
        let events = Arc::new(Mutex::new(Vec::new()));
        let reported = events.clone();
        let monitor = SshConnectionMonitor::new(
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            move |event| reported.lock().unwrap().push(event),
        );
        let child = Command::new("sh").arg("-c").arg("printf 'debug1: Server accepts key: token ED25519-SK agent\\n' >&2; sleep 0.7; printf 'debug3: send packet: type 50\\n' >&2; exec sleep 10")
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let started = Instant::now();
        let error = wait_with_output_timeout(child, Duration::from_millis(200), Some(&monitor))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            *events.lock().unwrap(),
            [
                SshConnectionProgress::WaitingForKey,
                SshConnectionProgress::Connecting
            ]
        );
    }

    #[test]
    fn verbose_key_details_are_withheld_but_authentication_errors_are_retained() {
        let monitor = SshConnectionMonitor::new(
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            |_| {},
        );
        let child = Command::new("sh").arg("-c").arg("printf 'debug1: Offering public key: secret-path SHA256:secret\\nPermission denied (publickey).\\n' >&2; exit 1")
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let output =
            wait_with_output_timeout(child, Duration::from_secs(2), Some(&monitor)).unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stderr, b"Permission denied (publickey).\n");
    }
}
