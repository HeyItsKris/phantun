use super::model::{ControlMessage, ControlState, MessageType};
use log::{debug, info, warn};
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::watch;
use tokio::time::{Instant, sleep};

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

pub async fn run_unix_sink(target: UnixTarget, mut rx: watch::Receiver<ControlState>) {
    let mut message_id: u64 = 0;
    let mut backoff = MIN_BACKOFF;
    let mut next_warn_at = Instant::now();

    loop {
        match UnixStream::connect(&target.path).await {
            Ok(mut stream) => {
                info!("control-plane connected to {}", target.label);
                backoff = MIN_BACKOFF;
                next_warn_at = Instant::now();

                if let Err(err) =
                    send_latest(&mut stream, &rx, MessageType::Snapshot, &mut message_id).await
                {
                    warn!(
                        "control-plane failed to send snapshot to {}: {}",
                        target.label, err
                    );
                    continue;
                }

                loop {
                    if rx.changed().await.is_err() {
                        debug!("control-plane state publisher closed, sink stopping");
                        return;
                    }

                    if let Err(err) =
                        send_latest(&mut stream, &rx, MessageType::Event, &mut message_id).await
                    {
                        warn!(
                            "control-plane failed to send event to {}: {}",
                            target.label, err
                        );
                        break;
                    }
                }
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

        sleep(backoff + jitter(backoff)).await;
        backoff = next_backoff(backoff);
    }
}

async fn send_latest(
    stream: &mut UnixStream,
    rx: &watch::Receiver<ControlState>,
    message_type: MessageType,
    message_id: &mut u64,
) -> io::Result<()> {
    let message = ControlMessage::new(
        message_type,
        format!("m{}", *message_id),
        rx.borrow().clone(),
    );
    *message_id = message_id.wrapping_add(1);

    let mut payload = serde_json::to_vec(&message)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
    payload.push(b'\n');

    stream.write_all(&payload).await
}

fn next_backoff(current: Duration) -> Duration {
    current.checked_mul(2).unwrap_or(MAX_BACKOFF).min(MAX_BACKOFF)
}

fn jitter(base: Duration) -> Duration {
    let max_jitter_ms = (base.as_millis() as u64).saturating_div(4).max(1);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    Duration::from_millis(seed % max_jitter_ms)
}
