use super::protocol::{ControlRequest, ControlResponse, MessageKind, PROTOCOL_VERSION};
use log::{debug, info, warn};
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::{Instant, sleep, timeout};

const MIN_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(10);
const WARN_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct UnixTarget {
    pub path: PathBuf,
    pub label: String,
}

pub fn parse_unix_target(raw: &str) -> io::Result<UnixTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "control target cannot be empty",
        ));
    }

    let path = trimmed.strip_prefix("unix:").unwrap_or(trimmed);
    if path.is_empty() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("invalid control target: {raw}"),
        ));
    }

    Ok(UnixTarget {
        path: PathBuf::from(path),
        label: format!("unix:{path}"),
    })
}

#[derive(Clone, Debug)]
pub struct AgentHandle {
    label: String,
    connected: Arc<AtomicBool>,
    tx: mpsc::Sender<AgentCommand>,
}

impl AgentHandle {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub async fn request(
        &self,
        request: ControlRequest,
        timeout_at: Duration,
    ) -> io::Result<ControlResponse> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(AgentCommand::Request {
                request,
                timeout: timeout_at,
                reply: reply_tx,
            })
            .await
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "agent command loop stopped"))?;

        timeout(timeout_at, reply_rx)
            .await
            .map_err(|_| io::Error::new(ErrorKind::TimedOut, "agent request timed out"))?
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, "agent response loop stopped"))?
    }
}

#[derive(Debug)]
enum AgentCommand {
    Request {
        request: ControlRequest,
        timeout: Duration,
        reply: oneshot::Sender<io::Result<ControlResponse>>,
    },
}

pub fn spawn_agent(target: UnixTarget, connectivity_notify: Arc<Notify>) -> AgentHandle {
    let (tx, rx) = mpsc::channel(8);
    let connected = Arc::new(AtomicBool::new(false));
    let agent = AgentHandle {
        label: target.label.clone(),
        connected: connected.clone(),
        tx,
    };

    tokio::spawn(run_agent(target, rx, connected, connectivity_notify));

    agent
}

async fn run_agent(
    target: UnixTarget,
    mut rx: mpsc::Receiver<AgentCommand>,
    connected: Arc<AtomicBool>,
    connectivity_notify: Arc<Notify>,
) {
    let mut backoff = MIN_BACKOFF;
    let mut next_warn_at = Instant::now();

    while !rx.is_closed() {
        match UnixStream::connect(&target.path).await {
            Ok(stream) => {
                info!("control-plane connected to {}", target.label);
                set_connected(&connected, &connectivity_notify, true);
                backoff = MIN_BACKOFF;
                next_warn_at = Instant::now();

                let (read_half, write_half) = stream.into_split();
                if run_connected_loop(&target, &mut rx, read_half, write_half)
                    .await
                    .is_err()
                {
                    warn!("control-plane disconnected from {}", target.label);
                }

                set_connected(&connected, &connectivity_notify, false);
            }
            Err(err) => {
                let now = Instant::now();
                if now >= next_warn_at {
                    warn!(
                        "control-plane failed to connect to {}: {}",
                        target.label, err
                    );
                    next_warn_at = now + WARN_INTERVAL;
                } else {
                    debug!(
                        "control-plane connect retry pending for {}: {}",
                        target.label, err
                    );
                }
            }
        }

        if rx.is_closed() {
            break;
        }

        sleep(backoff + jitter(backoff)).await;
        backoff = next_backoff(backoff);
    }
}

async fn run_connected_loop(
    target: &UnixTarget,
    rx: &mut mpsc::Receiver<AgentCommand>,
    read_half: OwnedReadHalf,
    mut write_half: OwnedWriteHalf,
) -> io::Result<()> {
    let mut reader = BufReader::new(read_half);

    loop {
        tokio::select! {
            maybe_command = rx.recv() => {
                let Some(command) = maybe_command else {
                    return Ok(());
                };

                match command {
                    AgentCommand::Request { request, timeout, reply } => {
                        let result = perform_request(
                            target,
                            &request,
                            timeout,
                            &mut write_half,
                            &mut reader,
                        ).await;

                        let is_connection_error = result
                            .as_ref()
                            .err()
                            .is_some_and(is_connection_error);
                        let _ = reply.send(result);

                        if is_connection_error {
                            return Err(io::Error::new(
                                ErrorKind::BrokenPipe,
                                format!("connection to {} lost during request", target.label),
                            ));
                        }
                    }
                }
            }
            inbound = read_response(&mut reader) => {
                match inbound {
                    Ok(Some(response)) => {
                        warn!(
                            "ignoring unexpected control-plane response from {} (session_id={}, id={})",
                            target.label,
                            response.session_id,
                            response.id
                        );
                    }
                    Ok(None) => {}
                    Err(err) => return Err(err),
                }
            }
        }
    }
}

async fn perform_request(
    target: &UnixTarget,
    request: &ControlRequest,
    wait_timeout: Duration,
    write_half: &mut OwnedWriteHalf,
    reader: &mut BufReader<OwnedReadHalf>,
) -> io::Result<ControlResponse> {
    let mut payload = serde_json::to_vec(request)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
    payload.push(b'\n');
    write_half.write_all(&payload).await?;

    let deadline = Instant::now() + wait_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("timed out waiting for {}", target.label),
            ));
        }

        let inbound = timeout(remaining, read_response(reader))
            .await
            .map_err(|_| {
                io::Error::new(
                    ErrorKind::TimedOut,
                    format!("timed out waiting for {}", target.label),
                )
            })??;

        let Some(response) = inbound else {
            continue;
        };

        if response.v != PROTOCOL_VERSION || response.kind != MessageKind::Response {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("invalid response envelope from {}", target.label),
            ));
        }

        if response.session_id != request.session_id || response.id != request.id {
            debug!(
                "ignoring late response from {} for session_id={}, id={}",
                target.label, response.session_id, response.id
            );
            continue;
        }

        return Ok(response);
    }
}

async fn read_response(
    reader: &mut BufReader<OwnedReadHalf>,
) -> io::Result<Option<ControlResponse>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(io::Error::new(
            ErrorKind::BrokenPipe,
            "control agent closed connection",
        ));
    }

    if line.trim().is_empty() {
        return Ok(None);
    }

    serde_json::from_str::<ControlResponse>(line.trim())
        .map(Some)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))
}

fn is_connection_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::BrokenPipe
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::UnexpectedEof
            | ErrorKind::NotConnected
    )
}

fn set_connected(connected: &Arc<AtomicBool>, notify: &Arc<Notify>, new_state: bool) {
    let old_state = connected.swap(new_state, Ordering::AcqRel);
    if old_state != new_state {
        notify.notify_waiters();
    }
}

fn next_backoff(current: Duration) -> Duration {
    current
        .checked_mul(2)
        .unwrap_or(MAX_BACKOFF)
        .min(MAX_BACKOFF)
}

fn jitter(base: Duration) -> Duration {
    let max_jitter_ms = (base.as_millis() as u64).saturating_div(4).max(1);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    Duration::from_millis(seed % max_jitter_ms)
}
