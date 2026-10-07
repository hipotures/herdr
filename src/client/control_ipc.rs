//! Private control channel for requests that must be handled by a running client.
//!
//! The server owns the session state, but only the client owns endpoint selection and
//! presentation.  This channel therefore delivers a request to the client's event loop and
//! waits for its acknowledgement instead of sending a second request to the server.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use tokio::sync::oneshot;

/// A request that must be interpreted by the running Herdr client.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientControlRequest {
    pub(crate) endpoint_id: String,
    pub(crate) target: String,
    pub(crate) check: bool,
    #[serde(default)]
    pub(crate) expected_boot_id: Option<String>,
}

/// The identity returned after the client has accepted a control request.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientControlReply {
    pub(crate) client_id: String,
    pub(crate) window_token: String,
    pub(crate) boot_id: String,
}

/// A control request delivered to the client's main event loop.
pub(super) struct ControlMessage {
    pub(super) request: ClientControlRequest,
    pub(super) reply: oneshot::Sender<Result<ClientControlReply, String>>,
}

/// Owns one client's control listener and removes its socket on shutdown.
pub(crate) struct ClientControlRegistration {
    pub(crate) client_id: String,
    pub(crate) window_token: String,
    #[cfg(unix)]
    socket_path: std::path::PathBuf,
    #[cfg(unix)]
    socket_identity: crate::ipc::SocketFileIdentity,
    #[cfg(unix)]
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    listener_thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for ClientControlRegistration {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        self.running.store(false, Ordering::Release);
        if let Err(error) =
            crate::ipc::remove_socket_file_if_owned(&self.socket_path, &self.socket_identity)
        {
            tracing::debug!(
                path = %self.socket_path.display(),
                %error,
                "failed to remove client control socket"
            );
        }
        if let Some(thread) = self.listener_thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;

    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use interprocess::local_socket::traits::{Listener as _, Stream as _};
    use interprocess::local_socket::ListenerNonblockingMode;
    use tokio::sync::{mpsc, oneshot};

    use super::super::endpoint::ProfileId;
    use super::super::events::ClientLoopEvent;

    const CONTROL_DIRECTORY: &str = "client-control";
    const CONTROL_SOCKET_PREFIX: &str = "client-";
    const CONTROL_SOCKET_SUFFIX: &str = ".sock";
    const CONTROL_DIRECTORY_MODE: u32 = 0o700;
    const CONTROL_SOCKET_MODE: u32 = 0o600;
    const MAX_CONTROL_LINE_BYTES: usize = 64 * 1024;
    const CONTROL_TIMEOUT: Duration = Duration::from_secs(7);
    const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);
    const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// Start a private control listener for the running client.
    pub(super) fn start(
        event_tx: mpsc::Sender<ClientLoopEvent>,
        local_socket_path: &Path,
    ) -> io::Result<ClientControlRegistration> {
        let directory = control_directory();
        start_at(&directory, event_tx, local_socket_path)
    }

    /// Send a request to exactly one live client.
    pub(super) fn request(
        request: &ClientControlRequest,
        client_id: Option<&str>,
        local_socket: Option<&Path>,
    ) -> io::Result<ClientControlReply> {
        let directory = control_directory();
        request_at(&directory, request, client_id, local_socket)
    }
    fn start_at(
        directory: &Path,
        event_tx: mpsc::Sender<ClientLoopEvent>,
        local_socket_path: &Path,
    ) -> io::Result<ClientControlRegistration> {
        crate::platform::client_control_unix::ensure_private_directory(
            directory,
            CONTROL_DIRECTORY_MODE,
        )?;

        let client_id = ProfileId::generate().to_string();
        let window_token = format!("[herdr-client:{client_id}]");
        let socket_path = directory.join(format!(
            "{CONTROL_SOCKET_PREFIX}{client_id}{CONTROL_SOCKET_SUFFIX}"
        ));
        let listener = crate::ipc::bind_private_local_listener(&socket_path)?;
        if let Err(error) =
            crate::ipc::restrict_socket_permissions(&socket_path, CONTROL_SOCKET_MODE)
        {
            let _ = fs::remove_file(&socket_path);
            return Err(error);
        }
        let socket_identity = match crate::ipc::socket_file_identity(&socket_path) {
            Ok(identity) => identity,
            Err(error) => {
                drop(listener);
                let _ = fs::remove_file(&socket_path);
                return Err(error);
            }
        };

        let descriptor = ClientControlDescriptor {
            client_id: client_id.clone(),
            window_token: window_token.clone(),
            local_socket: local_socket_path.to_string_lossy().into_owned(),
        };
        let running = Arc::new(AtomicBool::new(true));
        let listener_running = running.clone();
        let listener_thread = std::thread::Builder::new()
            .name(format!("herdr-control-{client_id}"))
            .spawn(move || {
                if let Err(error) = listener.set_nonblocking(ListenerNonblockingMode::Accept) {
                    tracing::warn!(%error, "failed to make client control listener nonblocking");
                    return;
                }
                accept_loop(listener, descriptor, listener_running, event_tx);
            })
            .map_err(|error| {
                let _ = fs::remove_file(&socket_path);
                io::Error::other(format!("failed to start client control listener: {error}"))
            })?;

        Ok(ClientControlRegistration {
            client_id,
            window_token,
            socket_path,
            socket_identity,
            running,
            listener_thread: Some(listener_thread),
        })
    }

    fn request_at(
        directory: &Path,
        request: &ClientControlRequest,
        client_id: Option<&str>,
        local_socket: Option<&Path>,
    ) -> io::Result<ClientControlReply> {
        crate::platform::client_control_unix::ensure_private_directory(
            directory,
            CONTROL_DIRECTORY_MODE,
        )?;

        let mut candidates = discover_clients(directory)?;
        if let Some(client_id) = client_id {
            candidates.retain(|candidate| candidate.descriptor.client_id == client_id);
        }
        if let Some(local_socket) = local_socket {
            let local_socket = local_socket.to_string_lossy();
            candidates.retain(|candidate| candidate.descriptor.local_socket == local_socket);
        }

        let candidate = match candidates.as_slice() {
            [] => {
                let selector = client_id
                    .map(|id| format!("client {id}"))
                    .or_else(|| local_socket.map(|path| format!("local socket {}", path.display())))
                    .unwrap_or_else(|| "focus request".into());
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no live Herdr client matches {selector}"),
                ));
            }
            [candidate] => candidate,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "multiple live Herdr clients match the focus request; specify a client identity",
                ));
            }
        };

        send_request(&candidate.socket_path, request)
    }

    fn control_directory() -> PathBuf {
        crate::config::state_dir().join(CONTROL_DIRECTORY)
    }

    fn accept_loop(
        listener: crate::ipc::LocalListener,
        descriptor: ClientControlDescriptor,
        running: Arc<AtomicBool>,
        event_tx: mpsc::Sender<ClientLoopEvent>,
    ) {
        while running.load(Ordering::Acquire) {
            match listener.accept() {
                Ok(stream) => {
                    let descriptor = descriptor.clone();
                    let event_tx = event_tx.clone();
                    let running = running.clone();
                    let _ = std::thread::Builder::new()
                        .name("herdr-control-request".into())
                        .spawn(move || handle_connection(stream, descriptor, running, event_tx));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) => {
                    if running.load(Ordering::Acquire) {
                        tracing::warn!(%error, "client control listener accept failed");
                    }
                    break;
                }
            }
        }
    }

    fn handle_connection(
        mut stream: crate::ipc::LocalStream,
        descriptor: ClientControlDescriptor,
        running: Arc<AtomicBool>,
        event_tx: mpsc::Sender<ClientLoopEvent>,
    ) {
        if stream.set_recv_timeout(Some(CONTROL_TIMEOUT)).is_err()
            || stream.set_send_timeout(Some(CONTROL_TIMEOUT)).is_err()
        {
            return;
        }

        let response = match read_json_line::<ControlWireRequest>(&mut stream) {
            Ok(ControlWireRequest::Describe) => ControlWireResponse::Descriptor {
                client_id: descriptor.client_id,
                window_token: descriptor.window_token,
                local_socket: descriptor.local_socket,
            },
            Ok(ControlWireRequest::Request { request }) => {
                let (reply, mut result) = oneshot::channel();
                if client_disconnected(&mut stream) {
                    return;
                }
                let message = ClientLoopEvent::ClientControl(ControlMessage { request, reply });
                if let Err(error) = event_tx.try_send(message) {
                    let message = match error {
                        mpsc::error::TrySendError::Full(_) => {
                            "Herdr client event loop is busy".to_owned()
                        }
                        mpsc::error::TrySendError::Closed(_) => {
                            "Herdr client event loop is unavailable".to_owned()
                        }
                    };
                    ControlWireResponse::Error { message }
                } else {
                    let deadline = std::time::Instant::now() + CONTROL_TIMEOUT;
                    let result = loop {
                        match result.try_recv() {
                            Ok(result) => break Ok(result),
                            Err(oneshot::error::TryRecvError::Closed) => {
                                break Err("Herdr client dropped the focus acknowledgement".into())
                            }
                            Err(oneshot::error::TryRecvError::Empty) => {
                                if !running.load(Ordering::Acquire)
                                    || std::time::Instant::now() >= deadline
                                {
                                    break Err(
                                        "timed out waiting for Herdr client acknowledgement".into(),
                                    );
                                }
                                if client_disconnected(&mut stream) {
                                    return;
                                }
                                std::thread::sleep(CONTROL_POLL_INTERVAL);
                            }
                        }
                    };
                    match result {
                        Ok(result) => ControlWireResponse::Result { result },
                        Err(message) => ControlWireResponse::Error { message },
                    }
                }
            }
            Err(error) => ControlWireResponse::Error {
                message: error.to_string(),
            },
        };

        let _ = write_json_line(&mut stream, &response);
    }

    fn client_disconnected(stream: &mut crate::ipc::LocalStream) -> bool {
        match crate::ipc::local_stream_peer_closed(stream) {
            Ok(disconnected) => disconnected,
            Err(error) => {
                tracing::debug!(%error, "failed to probe client control connection");
                true
            }
        }
    }

    fn discover_clients(directory: &Path) -> io::Result<Vec<DiscoveredClient>> {
        let mut clients = Vec::new();
        let entries = fs::read_dir(directory)?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            if !is_control_socket_path(&path) {
                continue;
            }
            let descriptor = match describe_client(&path) {
                Ok(descriptor) => descriptor,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out querying live Herdr client control socket {}",
                            path.display()
                        ),
                    ));
                }
                Err(error) if is_stale_socket_error(&error) => {
                    remove_stale_socket_if_unchanged(&path);
                    continue;
                }
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "cannot identify Herdr client control socket {}: {error}",
                            path.display()
                        ),
                    ))
                }
            };
            clients.push(DiscoveredClient {
                socket_path: path,
                descriptor,
            });
        }
        Ok(clients)
    }

    fn remove_stale_socket_if_unchanged(path: &Path) {
        let Ok(identity) = crate::ipc::socket_file_identity(path) else {
            return;
        };
        if let Err(error) = crate::ipc::remove_socket_file_if_owned(path, &identity) {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), %error, "failed to remove stale client control socket");
            }
        }
    }

    fn is_control_socket_path(path: &Path) -> bool {
        crate::platform::client_control_unix::is_socket_path(
            path,
            CONTROL_SOCKET_PREFIX,
            CONTROL_SOCKET_SUFFIX,
        )
    }

    fn describe_client(path: &Path) -> io::Result<ClientControlDescriptor> {
        let mut stream = connect_with_timeouts(path)?;
        write_json_line(&mut stream, &ControlWireRequest::Describe)?;
        match read_json_line(&mut stream)? {
            ControlWireResponse::Descriptor {
                client_id,
                window_token,
                local_socket,
            } => Ok(ClientControlDescriptor {
                client_id,
                window_token,
                local_socket,
            }),
            ControlWireResponse::Error { message } => Err(io::Error::other(message)),
            ControlWireResponse::Result { .. } => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "client control discovery returned a focus response",
            )),
        }
    }

    fn send_request(path: &Path, request: &ClientControlRequest) -> io::Result<ClientControlReply> {
        let mut stream = connect_with_timeouts(path)?;
        write_json_line(
            &mut stream,
            &ControlWireRequest::Request {
                request: request.clone(),
            },
        )?;
        match read_json_line(&mut stream)? {
            ControlWireResponse::Result { result } => result.map_err(io::Error::other),
            ControlWireResponse::Error { message } => Err(io::Error::other(message)),
            ControlWireResponse::Descriptor { .. } => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "client control request returned a discovery response",
            )),
        }
    }

    fn connect_with_timeouts(path: &Path) -> io::Result<crate::ipc::LocalStream> {
        let stream = crate::ipc::connect_local_stream(path)?;
        stream.set_recv_timeout(Some(CONTROL_TIMEOUT))?;
        stream.set_send_timeout(Some(CONTROL_TIMEOUT))?;
        Ok(stream)
    }

    fn is_stale_socket_error(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
        )
    }

    fn read_json_line<T: for<'de> Deserialize<'de>>(
        stream: &mut crate::ipc::LocalStream,
    ) -> io::Result<T> {
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            match stream.read(&mut byte) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "client control connection closed before a complete request",
                    ));
                }
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => {
                    bytes.push(byte[0]);
                    if bytes.len() > MAX_CONTROL_LINE_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "client control request is too large",
                        ));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid client control message: {error}"),
            )
        })
    }

    fn write_json_line<T: Serialize>(
        stream: &mut crate::ipc::LocalStream,
        value: &T,
    ) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        bytes.push(b'\n');
        stream.write_all(&bytes)
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
    enum ControlWireRequest {
        Describe,
        Request { request: ClientControlRequest },
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
    enum ControlWireResponse {
        Descriptor {
            client_id: String,
            window_token: String,
            local_socket: String,
        },
        Result {
            result: Result<ClientControlReply, String>,
        },
        Error {
            message: String,
        },
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct ClientControlDescriptor {
        client_id: String,
        window_token: String,
        local_socket: String,
    }

    struct DiscoveredClient {
        socket_path: PathBuf,
        descriptor: ClientControlDescriptor,
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        fn temporary_directory(name: &str) -> PathBuf {
            let directory = PathBuf::from(format!(
                "/tmp/hc-{name}-{}-{}",
                std::process::id(),
                ProfileId::generate()
            ));
            fs::create_dir_all(&directory).unwrap();
            directory
        }

        #[test]
        fn request_requires_exactly_one_matching_client() {
            let directory = temporary_directory("ambiguous");
            let (tokio_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
            let first =
                start_at(&directory, tokio_tx.clone(), Path::new("/tmp/local.sock")).unwrap();
            let second = start_at(&directory, tokio_tx, Path::new("/tmp/local.sock")).unwrap();

            let request = ClientControlRequest {
                endpoint_id: "local".into(),
                target: "w1:p3".into(),
                check: true,
                expected_boot_id: None,
            };
            let error = request_at(&directory, &request, None, None).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);

            let request_directory = directory.clone();
            let request_for_thread = request.clone();
            let first_id = first.client_id.clone();
            let first_window_token = first.window_token.clone();
            let first_id_for_request = first_id.clone();
            let reply = std::thread::spawn(move || {
                request_at(
                    &request_directory,
                    &request_for_thread,
                    Some(&first_id_for_request),
                    Some(Path::new("/tmp/local.sock")),
                )
            });
            let control = match event_rx.blocking_recv().unwrap() {
                ClientLoopEvent::ClientControl(control) => control,
                _ => panic!("unexpected client loop event"),
            };
            let _ = control.reply.send(Ok(ClientControlReply {
                client_id: first_id,
                window_token: first_window_token,
                boot_id: "boot-1".into(),
            }));
            assert_eq!(reply.join().unwrap().unwrap().boot_id, "boot-1");

            drop(first);
            drop(second);
            let _ = fs::remove_dir_all(directory);
        }

        #[test]
        fn disconnected_caller_cancels_a_queued_navigation_request() {
            let directory = temporary_directory("cancel");
            let (event_tx, mut events) = mpsc::channel(4);
            let registration =
                start_at(&directory, event_tx, Path::new("/tmp/local.sock")).unwrap();
            let mut stream = connect_with_timeouts(&registration.socket_path).unwrap();
            write_json_line(
                &mut stream,
                &ControlWireRequest::Request {
                    request: ClientControlRequest {
                        endpoint_id: "local".into(),
                        target: "w1:p3".into(),
                        check: false,
                        expected_boot_id: Some("boot-1".into()),
                    },
                },
            )
            .unwrap();
            let ClientLoopEvent::ClientControl(message) = events.blocking_recv().unwrap() else {
                panic!("unexpected client event");
            };
            assert!(!message.reply.is_closed());
            drop(stream);
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while !message.reply.is_closed() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                message.reply.is_closed(),
                "queued request must be cancelled before event dispatch"
            );
            drop(registration);
            assert!(fs::read_dir(&directory).unwrap().next().is_none());
            fs::remove_dir(directory).unwrap();
        }

        #[test]
        fn control_files_are_private_and_a_missing_client_never_falls_back() {
            use std::os::unix::fs::PermissionsExt;
            let directory = temporary_directory("private");
            let (event_tx, _events) = mpsc::channel(4);
            let registration =
                start_at(&directory, event_tx, Path::new("/tmp/local.sock")).unwrap();
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&registration.socket_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            let error = request_at(
                &directory,
                &ClientControlRequest {
                    endpoint_id: "local".into(),
                    target: "w1:p3".into(),
                    check: true,
                    expected_boot_id: None,
                },
                Some("missing-client"),
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            drop(registration);
            fs::remove_dir(directory).unwrap();
        }

        #[test]
        fn stale_control_socket_is_removed_during_discovery() {
            let directory = temporary_directory("stale");
            let path = directory.join("client-stale.sock");
            {
                let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            }
            let error = request_at(
                &directory,
                &ClientControlRequest {
                    endpoint_id: "local".into(),
                    target: "w1:p3".into(),
                    check: true,
                    expected_boot_id: None,
                },
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            assert!(!path.exists());
            let _ = fs::remove_dir_all(directory);
        }
    }
}

#[cfg(unix)]
pub(super) fn start(
    event_tx: tokio::sync::mpsc::Sender<super::events::ClientLoopEvent>,
    local_socket_path: &Path,
) -> io::Result<ClientControlRegistration> {
    unix::start(event_tx, local_socket_path)
}

#[cfg(not(unix))]
pub(super) fn start(
    _event_tx: tokio::sync::mpsc::Sender<super::events::ClientLoopEvent>,
    _local_socket_path: &Path,
) -> io::Result<ClientControlRegistration> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "client control IPC is unavailable on this platform",
    ))
}

#[cfg(unix)]
pub(crate) fn request(
    request: &ClientControlRequest,
    client_id: Option<&str>,
    local_socket: Option<&Path>,
) -> io::Result<ClientControlReply> {
    unix::request(request, client_id, local_socket)
}

#[cfg(not(unix))]
pub(crate) fn request(
    _request: &ClientControlRequest,
    _client_id: Option<&str>,
    _local_socket: Option<&Path>,
) -> io::Result<ClientControlReply> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "client control IPC is unavailable on this platform",
    ))
}
