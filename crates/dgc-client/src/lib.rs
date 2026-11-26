//! Client for the dgc orchestrator daemon socket.
//!
//! A [`Client`] owns one connection: it performs the `hello` handshake on
//! connect, then sends one request at a time with [`Client::call`]. Use
//! [`Client::connect_or_spawn`] to start the daemon on demand.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use dgc_daemon::paths::{DaemonPaths, ensure_private_dir};
use dgc_protocol::methods::{HELLO, HelloParams, SUBSCRIBE, SubscribeParams};
use dgc_protocol::rpc::{Notification, Request, Response, RpcError};
use dgc_protocol::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, timeout};

/// How long `connect_or_spawn` keeps retrying after starting the daemon.
/// The daemon binds its socket early but probes the provider CLIs
/// (`dgc_daemon::provider::resolve::VERSION_TIMEOUT`, 5 s) before accepting,
/// so this must comfortably exceed that bound plus the rest of startup.
const SPAWN_WAIT: Duration = Duration::from_secs(15);
/// Delay between connection attempts while the daemon starts.
const RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// Upper bound for the handshake on a freshly opened connection. A connection
/// made while the daemon is still starting waits in its listen backlog until
/// the provider probes (`VERSION_TIMEOUT`, 5 s) finish, so this too must
/// comfortably exceed that bound plus the rest of startup.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
/// Capacity of the channel [`Client::subscribe`] returns.
const SUBSCRIBE_CAPACITY: usize = 256;
/// Notifications kept while waiting for a response; the oldest are dropped.
const PENDING_LIMIT: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon error {}: {}", .0.code, .0.message)]
    Rpc(RpcError),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("daemon sent a line longer than {MAX_MESSAGE_BYTES} bytes")]
    MessageTooLong,
    #[error("daemon closed the connection")]
    Closed,
    #[error("daemon did not answer the handshake in time")]
    HelloTimeout,
    #[error("daemon speaks protocol version {daemon}, this client speaks {client}")]
    VersionMismatch { daemon: u32, client: u32 },
    #[error("daemon did not become reachable within 15 s (last error: {last}); see {}", .log.display())]
    SpawnTimeout { last: String, log: PathBuf },
}

impl ClientError {
    /// True when nothing is listening: the socket file is missing or refuses
    /// connections (a stale file left by a crashed daemon).
    pub fn is_not_running(&self) -> bool {
        matches!(
            self,
            ClientError::Io(err) if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            )
        )
    }
}

#[derive(serde::Deserialize)]
struct HelloResult {
    protocol_version: u32,
    daemon_pid: u32,
}

/// One framed message from the daemon.
enum Incoming {
    Response(Response),
    Notification(Notification),
}

pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    pending: VecDeque<Notification>,
    daemon_pid: u32,
}

impl Client {
    /// Connect to the daemon socket and complete the `hello` handshake.
    pub async fn connect(paths: &DaemonPaths) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(&paths.socket).await?;
        Self::handshake(stream).await
    }

    async fn handshake(stream: UnixStream) -> Result<Self, ClientError> {
        let (read, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(read),
            writer,
            // Id 0 is the server's "unknown id" in error replies.
            next_id: 1,
            pending: VecDeque::new(),
            daemon_pid: 0,
        };
        let params = HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client: format!("dgc-client/{}", env!("CARGO_PKG_VERSION")),
        };
        let hello: HelloResult = timeout(HELLO_TIMEOUT, client.call(HELLO, &params))
            .await
            .map_err(|_| ClientError::HelloTimeout)??;
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(ClientError::VersionMismatch {
                daemon: hello.protocol_version,
                client: PROTOCOL_VERSION,
            });
        }
        client.daemon_pid = hello.daemon_pid;
        Ok(client)
    }

    /// Connect, starting the daemon (`exe daemon run`, detached) when none is
    /// running, then retry every 50 ms for up to 15 s. The child is not waited
    /// for: if it loses the single-instance race it exits and the retry
    /// connects to the winner.
    pub async fn connect_or_spawn(paths: &DaemonPaths, exe: &Path) -> Result<Self, ClientError> {
        match Self::connect(paths).await {
            Ok(client) => return Ok(client),
            Err(err) if err.is_not_running() => {}
            Err(err) => return Err(err),
        }
        spawn_daemon(paths, exe)?;

        let deadline = Instant::now() + SPAWN_WAIT;
        loop {
            sleep(RETRY_INTERVAL).await;
            match Self::connect(paths).await {
                Ok(client) => return Ok(client),
                Err(err) if Instant::now() >= deadline => {
                    return Err(ClientError::SpawnTimeout {
                        last: err.to_string(),
                        log: daemon_log_path(paths),
                    });
                }
                // Not listening yet, or a daemon shutting down: keep trying.
                Err(_) => {}
            }
        }
    }

    /// Pid of the daemon that answered `hello`.
    pub fn daemon_pid(&self) -> u32 {
        self.daemon_pid
    }

    /// Send one request and wait for its response. Notifications that arrive
    /// meanwhile are buffered for [`Client::subscribe`]; if the client is never
    /// subscribed they are dropped with it.
    pub async fn call<P, R>(&mut self, method: &str, params: &P) -> Result<R, ClientError>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request::new(id, method, serde_json::to_value(params)?);
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        loop {
            match read_message(&mut self.reader).await? {
                Incoming::Notification(notification) => {
                    if self.pending.len() == PENDING_LIMIT {
                        self.pending.pop_front();
                    }
                    self.pending.push_back(notification);
                }
                Incoming::Response(response) => {
                    // The server answers unparseable input with id 0; with one
                    // request in flight that error is ours.
                    if response.id != id && !(response.id == 0 && response.error.is_some()) {
                        return Err(ClientError::Protocol(format!(
                            "response id {} does not match request id {id}",
                            response.id
                        )));
                    }
                    if let Some(error) = response.error {
                        return Err(ClientError::Rpc(error));
                    }
                    let result = response.result.unwrap_or(Value::Null);
                    return Ok(serde_json::from_value(result)?);
                }
            }
        }
    }

    /// Send `subscribe` and turn the connection into a notification stream.
    /// A background task forwards each notification into the returned bounded
    /// channel (capacity 256); it stops, closing the channel, when the daemon
    /// disconnects or sends an invalid line, and stops (closing the
    /// connection) as soon as the receiver is dropped.
    pub async fn subscribe(
        mut self,
        params: SubscribeParams,
    ) -> Result<mpsc::Receiver<Notification>, ClientError> {
        let _: Value = self.call(SUBSCRIBE, &params).await?;
        let (tx, rx) = mpsc::channel(SUBSCRIBE_CAPACITY);
        let Client {
            mut reader,
            writer,
            pending,
            ..
        } = self;
        tokio::spawn(async move {
            // Dropping the write half would close the connection.
            let _writer = writer;
            for notification in pending {
                if tx.send(notification).await.is_err() {
                    return;
                }
            }
            loop {
                // Stop as soon as the receiver is gone, even while the
                // daemon sends nothing (after `lagged`), closing the
                // connection; a partly read line is discarded with it.
                let message = tokio::select! {
                    message = read_message(&mut reader) => message,
                    _ = tx.closed() => return,
                };
                match message {
                    Ok(Incoming::Notification(notification)) => {
                        if tx.send(notification).await.is_err() {
                            return;
                        }
                    }
                    // A stray response has nobody waiting for it.
                    Ok(Incoming::Response(_)) => {}
                    Err(_) => return,
                }
            }
        });
        Ok(rx)
    }
}

fn daemon_log_path(paths: &DaemonPaths) -> PathBuf {
    paths.state_dir.join("daemon.log")
}

/// Start `exe daemon run` in a new session with stdin from `/dev/null` and
/// output appended to `<state_dir>/daemon.log`. The environment (including
/// `DGC_STATE_ROOT`) is inherited. The child is not waited for.
pub fn spawn_daemon(paths: &DaemonPaths, exe: &Path) -> Result<(), ClientError> {
    ensure_private_dir(&paths.state_dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(daemon_log_path(paths))?;
    let stderr_log = log.try_clone()?;

    let mut command = Command::new(exe);
    command
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr_log);
    // SAFETY: the closure runs between fork and exec and only calls setsid,
    // which is async-signal-safe and touches no memory shared with the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    // Dropping the handle neither kills nor waits for the child.
    command.spawn()?;
    Ok(())
}

/// Read one newline-terminated line of at most `MAX_MESSAGE_BYTES` (newline
/// excluded) and decode it. EOF is [`ClientError::Closed`].
async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Incoming, ClientError> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Err(ClientError::Closed);
        }
        match chunk.iter().position(|&byte| byte == b'\n') {
            Some(end) => {
                if line.len() + end > MAX_MESSAGE_BYTES {
                    return Err(ClientError::MessageTooLong);
                }
                line.extend_from_slice(&chunk[..end]);
                reader.consume(end + 1);
                break;
            }
            None => {
                if line.len() + chunk.len() > MAX_MESSAGE_BYTES {
                    return Err(ClientError::MessageTooLong);
                }
                line.extend_from_slice(chunk);
                let taken = chunk.len();
                reader.consume(taken);
            }
        }
    }
    decode(&line)
}

fn decode(line: &[u8]) -> Result<Incoming, ClientError> {
    let value: Value = serde_json::from_slice(line)?;
    if value.get("id").is_some() {
        Ok(Incoming::Response(serde_json::from_value(value)?))
    } else if value.get("method").is_some() {
        Ok(Incoming::Notification(serde_json::from_value(value)?))
    } else {
        Err(ClientError::Protocol(
            "message is neither a response nor a notification".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dgc_daemon::config::DaemonConfig;
    use dgc_daemon::run_daemon;
    use dgc_protocol::methods::{DAEMON_SHUTDOWN, DAEMON_STATUS, DaemonStatus};
    use serde_json::json;
    use tempfile::{TempDir, tempdir};
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;
    use tokio::sync::watch;

    /// A scripted server: accepts one connection, answers `hello`, then runs
    /// `script` with the buffered reader and the write half.
    async fn fake_server<F, Fut>(script: F) -> (TempDir, DaemonPaths)
    where
        F: FnOnce(BufReader<OwnedReadHalf>, OwnedWriteHalf) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        std::fs::create_dir_all(paths.socket.parent().expect("parent")).expect("mkdir");
        let listener = UnixListener::bind(&paths.socket).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("hello line");
            let request: Request = serde_json::from_str(&line).expect("hello request");
            assert_eq!(request.method, HELLO);
            assert_eq!(request.id, 1, "requests are numbered from 1");
            send(
                &mut write,
                &Response::ok(1, json!({"protocol_version": 1, "daemon_pid": 4242})),
            )
            .await;
            script(reader, write).await;
        });
        (dir, paths)
    }

    async fn send<T: Serialize>(write: &mut OwnedWriteHalf, message: &T) {
        let mut line = serde_json::to_string(message).expect("serialize");
        line.push('\n');
        write.write_all(line.as_bytes()).await.expect("write");
    }

    async fn read_request(reader: &mut BufReader<OwnedReadHalf>) -> Request {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("request line");
        serde_json::from_str(&line).expect("request")
    }

    #[tokio::test]
    async fn connect_records_daemon_pid_and_call_returns_result() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let request = read_request(&mut reader).await;
            assert_eq!((request.id, request.method.as_str()), (2, "echo"));
            send(&mut write, &Response::ok(2, request.params)).await;
        })
        .await;
        let mut client = Client::connect(&paths).await.expect("connect");
        assert_eq!(client.daemon_pid(), 4242);
        let echoed: Value = client
            .call("echo", &json!({"a": 1}))
            .await
            .expect("call succeeds");
        assert_eq!(echoed, json!({"a": 1}));
    }

    #[tokio::test]
    async fn rpc_error_is_returned_with_code() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let request = read_request(&mut reader).await;
            let error = RpcError::new(dgc_protocol::rpc::NOT_FOUND, "no such thing");
            send(&mut write, &Response::err(request.id, error)).await;
        })
        .await;
        let mut client = Client::connect(&paths).await.expect("connect");
        let err = client
            .call::<_, Value>("agent.get", &json!({}))
            .await
            .expect_err("error response");
        assert!(
            matches!(&err, ClientError::Rpc(e) if e.code == dgc_protocol::rpc::NOT_FOUND),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn notification_during_call_is_buffered_for_subscribe() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let request = read_request(&mut reader).await;
            assert_eq!(request.method, SUBSCRIBE);
            // The event overtakes the response to `subscribe`.
            send(&mut write, &Notification::new("event", json!({"seq": 1}))).await;
            send(&mut write, &Response::ok(request.id, json!({}))).await;
            send(&mut write, &Notification::new("event", json!({"seq": 2}))).await;
            // Keep the connection open until the client hangs up.
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink).await;
        })
        .await;
        let client = Client::connect(&paths).await.expect("connect");
        let mut rx = client
            .subscribe(SubscribeParams {
                agent_id: None,
                since_seq: None,
            })
            .await
            .expect("subscribe");
        for seq in [1, 2] {
            let notification = timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("notification arrives")
                .expect("channel open");
            assert_eq!(notification.params, json!({"seq": seq}));
        }
    }

    #[tokio::test]
    async fn subscribe_channel_closes_when_daemon_disconnects() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let request = read_request(&mut reader).await;
            send(&mut write, &Response::ok(request.id, json!({}))).await;
        })
        .await;
        let client = Client::connect(&paths).await.expect("connect");
        let mut rx = client
            .subscribe(SubscribeParams {
                agent_id: None,
                since_seq: None,
            })
            .await
            .expect("subscribe");
        let end = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("channel closes");
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn dropping_the_subscription_closes_the_connection() {
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let request = read_request(&mut reader).await;
            send(&mut write, &Response::ok(request.id, json!({}))).await;
            // Silent from here on: only the client can end the connection.
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink).await;
            let _ = closed_tx.send(());
        })
        .await;
        let client = Client::connect(&paths).await.expect("connect");
        let rx = client
            .subscribe(SubscribeParams {
                agent_id: None,
                since_seq: None,
            })
            .await
            .expect("subscribe");
        drop(rx);
        timeout(Duration::from_secs(5), closed_rx)
            .await
            .expect("the connection closes once the receiver is gone")
            .expect("server saw EOF");
    }

    #[tokio::test]
    async fn oversized_line_is_rejected() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let _ = read_request(&mut reader).await;
            let filler = "x".repeat(MAX_MESSAGE_BYTES + 1);
            let _ = write.write_all(filler.as_bytes()).await;
            let _ = write.write_all(b"\n").await;
        })
        .await;
        let mut client = Client::connect(&paths).await.expect("connect");
        let err = client
            .call::<_, Value>("anything", &json!({}))
            .await
            .expect_err("oversized line");
        assert!(matches!(err, ClientError::MessageTooLong), "{err:?}");
    }

    #[tokio::test]
    async fn line_of_exactly_the_limit_is_accepted() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let request = read_request(&mut reader).await;
            // {"id":N,"result":"<fill>"} padded to exactly MAX_MESSAGE_BYTES.
            let prefix = format!("{{\"id\":{},\"result\":\"", request.id);
            let fill = MAX_MESSAGE_BYTES - prefix.len() - 2;
            let line = format!("{prefix}{}\"}}\n", "y".repeat(fill));
            assert_eq!(line.len(), MAX_MESSAGE_BYTES + 1);
            write.write_all(line.as_bytes()).await.expect("write");
        })
        .await;
        let mut client = Client::connect(&paths).await.expect("connect");
        let text: String = client.call("big", &json!({})).await.expect("accepted");
        let overhead = "{\"id\":2,\"result\":\"".len() + "\"}".len();
        assert_eq!(text.len(), MAX_MESSAGE_BYTES - overhead);
    }

    #[tokio::test]
    async fn mismatched_response_id_is_a_protocol_error() {
        let (_dir, paths) = fake_server(|mut reader, mut write| async move {
            let _ = read_request(&mut reader).await;
            send(&mut write, &Response::ok(99, json!({}))).await;
        })
        .await;
        let mut client = Client::connect(&paths).await.expect("connect");
        let err = client
            .call::<_, Value>("anything", &json!({}))
            .await
            .expect_err("mismatch");
        assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    }

    #[tokio::test]
    async fn closed_connection_is_reported() {
        let (_dir, paths) = fake_server(|mut reader, _write| async move {
            let _ = read_request(&mut reader).await;
        })
        .await;
        let mut client = Client::connect(&paths).await.expect("connect");
        let err = client
            .call::<_, Value>("anything", &json!({}))
            .await
            .expect_err("closed");
        assert!(matches!(err, ClientError::Closed), "{err:?}");
    }

    #[tokio::test]
    async fn version_mismatch_is_reported() {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        std::fs::create_dir_all(paths.socket.parent().expect("parent")).expect("mkdir");
        let listener = UnixListener::bind(&paths.socket).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let request = read_request(&mut reader).await;
            let result = json!({"protocol_version": 99, "daemon_pid": 1});
            send(&mut write, &Response::ok(request.id, result)).await;
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink).await;
        });
        let err = Client::connect(&paths)
            .await
            .err()
            .expect("version mismatch");
        assert!(
            matches!(err, ClientError::VersionMismatch { daemon: 99, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn missing_socket_is_not_running() {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        let err = Client::connect(&paths).await.err().expect("no daemon");
        assert!(err.is_not_running(), "{err:?}");
    }

    #[tokio::test]
    async fn stale_socket_file_is_not_running() {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        std::fs::create_dir_all(paths.socket.parent().expect("parent")).expect("mkdir");
        // Bind and drop: the file stays but nothing listens.
        drop(UnixListener::bind(&paths.socket).expect("bind"));
        let err = Client::connect(&paths).await.err().expect("stale socket");
        assert!(err.is_not_running(), "{err:?}");
    }

    #[tokio::test]
    async fn talks_to_the_real_daemon() {
        let dir = tempdir().expect("tempdir");
        let paths = DaemonPaths::under(dir.path());
        let (shutdown, _rx) = watch::channel(false);
        let task = tokio::spawn(run_daemon(
            paths.clone(),
            DaemonConfig::default(),
            shutdown.clone(),
        ));

        let mut client = loop {
            match Client::connect(&paths).await {
                Ok(client) => break client,
                Err(err) if err.is_not_running() => sleep(Duration::from_millis(10)).await,
                Err(err) => panic!("connect failed: {err}"),
            }
        };
        assert_eq!(client.daemon_pid(), std::process::id());
        let status: DaemonStatus = client
            .call(DAEMON_STATUS, &json!({}))
            .await
            .expect("status");
        assert_eq!(status.pid, std::process::id());
        let _: Value = client
            .call(DAEMON_SHUTDOWN, &json!({}))
            .await
            .expect("shutdown");
        timeout(Duration::from_secs(10), task)
            .await
            .expect("daemon stops")
            .expect("task joins")
            .expect("clean shutdown");
    }
}
