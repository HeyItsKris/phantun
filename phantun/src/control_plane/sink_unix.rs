use super::model::{ControlMessage, InboundMessage, MessageType, PublishedState, build_message_id};
use super::sync_state::ControlSyncState;
use log::{debug, info, warn};
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
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

pub async fn run_unix_sink(
    target: UnixTarget,
    mut rx: watch::Receiver<PublishedState>,
    sync: ControlSyncState,
) {
    let mut backoff = MIN_BACKOFF;
    let mut next_warn_at = Instant::now();
    let mut message_seq: u64 = 0;

    loop {
        match UnixStream::connect(&target.path).await {
            Ok(stream) => {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                // Mark current version as observed so reconnect does not emit
                // an immediate duplicate event for the same state.
                let snapshot = rx.borrow_and_update().clone();

                if let Err(err) =
                    send_published(
                        &mut write_half,
                        &snapshot,
                        MessageType::Snapshot,
                        target.label.as_str(),
                        &sync,
                        &mut message_seq,
                    )
                    .await
                {
                    warn!(
                        "control-plane failed to send snapshot to {}: {}",
                        target.label, err
                    );
                } else {
                    info!("control-plane connected to {}", target.label);
                    backoff = MIN_BACKOFF;
                    next_warn_at = Instant::now();

                    loop {
                        tokio::select! {
                            state_change = rx.changed() => {
                                if state_change.is_err() {
                                    debug!("control-plane state publisher closed, sink stopping");
                                    return;
                                }
                                let published = rx.borrow().clone();

                                if let Err(err) = send_published(
                                    &mut write_half,
                                    &published,
                                    MessageType::Event,
                                    target.label.as_str(),
                                    &sync,
                                    &mut message_seq,
                                ).await {
                                    warn!(
                                        "control-plane failed to send event to {}: {}",
                                        target.label, err
                                    );
                                    break;
                                }
                            }
                            inbound = read_inbound(&mut reader) => {
                                match inbound {
                                    Ok(Some(message)) => {
                                        if let Some(seq) = message.acked_seq() {
                                            sync.acked.mark(seq, target.label.as_str()).await;
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(err) => {
                                        warn!(
                                            "control-plane inbound stream error on {}: {}",
                                            target.label, err
                                        );
                                        break;
                                    }
                                }
                            }
                        }
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

async fn send_published(
    write_half: &mut OwnedWriteHalf,
    published: &PublishedState,
    message_type: MessageType,
    target_label: &str,
    sync: &ControlSyncState,
    message_seq: &mut u64,
) -> io::Result<()> {
    let message_id = build_message_id(published.seq, *message_seq);
    *message_seq = message_seq.wrapping_add(1);

    let message = ControlMessage::from_published(message_type, message_id, published);
    let mut payload = serde_json::to_vec(&message)
        .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
    payload.push(b'\n');

    write_half.write_all(&payload).await?;
    sync.delivered.mark(published.seq, target_label).await;
    Ok(())
}

async fn read_inbound<R>(reader: &mut BufReader<R>) -> io::Result<Option<InboundMessage>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(io::Error::new(
            ErrorKind::BrokenPipe,
            "control consumer closed connection",
        ));
    }

    if line.trim().is_empty() {
        return Ok(None);
    }

    match serde_json::from_str::<InboundMessage>(line.trim()) {
        Ok(message) => Ok(Some(message)),
        Err(err) => {
            debug!("Ignoring malformed control-plane inbound message: {}", err);
            Ok(None)
        }
    }
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

#[cfg(test)]
mod tests {
    use crate::control_plane::model::{ControlMode, ControlState, PublishedState};
    use tokio::sync::watch;

    #[test]
    fn borrow_and_update_clears_pending_change_after_snapshot() {
        let initial = PublishedState::new(ControlState::new(ControlMode::Server, None, None));
        let (tx, mut rx) = watch::channel(initial);

        let mut up_state = rx.borrow().state.clone();
        up_state.state = crate::control_plane::model::TunnelState::Up;
        let _ = tx.send_replace(PublishedState {
            seq: 1,
            state: up_state,
        });

        assert!(rx.has_changed().unwrap());

        let snapshot = rx.borrow_and_update().clone();
        assert_eq!(snapshot.seq, 1);
        assert!(!rx.has_changed().unwrap());
    }
}
