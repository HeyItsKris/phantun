use super::agent::{AgentHandle, UnixTarget, parse_unix_target, spawn_agent};
use super::model::{ControlState, ControlStatePhase, StopReason};
use super::protocol::{ControlRequest, RequestPhase};
use log::{info, warn};
use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PRE_START_TIMEOUT: Duration = Duration::from_secs(3);
const POST_START_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_GRACE_TIMEOUT: Duration = Duration::from_secs(3);
const PRE_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const POST_STOP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct ControlPlane {
    inner: Option<Arc<ControlRuntime>>,
}

impl ControlPlane {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.inner
            .as_ref()
            .map(|inner| inner.shutdown_token.clone())
            .unwrap_or_default()
    }

    pub async fn post_start(
        &self,
        dev: Option<String>,
        mtu: Option<u32>,
        addr4: Option<String>,
        addr6: Option<String>,
        peer4: Option<String>,
        peer6: Option<String>,
    ) -> io::Result<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };

        {
            let mut state = inner.state.lock().await;
            state.mark_post_start(dev, mtu, addr4, addr6, peer4, peer6);
        }

        inner
            .broadcast_phase(RequestPhase::PostStart, POST_START_TIMEOUT)
            .await?;
        {
            let mut state = inner.state.lock().await;
            state.mark_running();
        }
        info!("control-plane entered running state");
        Ok(())
    }

    pub async fn prepare_stop(&self, reason: StopReason) -> bool {
        let Some(inner) = &self.inner else {
            return true;
        };

        inner.shutting_down.store(true, Ordering::Release);
        {
            let mut state = inner.state.lock().await;
            state.mark_pre_stop(reason.clone());
        }

        let barrier_ok = inner
            .broadcast_phase(RequestPhase::PreStop, PRE_STOP_TIMEOUT)
            .await
            .is_ok();

        {
            let mut state = inner.state.lock().await;
            state.mark_stopping(reason);
        }

        barrier_ok
    }

    pub async fn complete_stop(&self) -> bool {
        let Some(inner) = &self.inner else {
            return true;
        };

        let stop_reason = {
            let state = inner.state.lock().await;
            state.reason.clone().unwrap_or(StopReason::ProcessExit)
        };

        {
            let mut state = inner.state.lock().await;
            state.mark_post_stop(stop_reason);
        }

        let barrier_ok = inner
            .broadcast_phase(RequestPhase::PostStop, POST_STOP_TIMEOUT)
            .await
            .is_ok();

        {
            let mut state = inner.state.lock().await;
            state.mark_stopped();
        }

        barrier_ok
    }
}

pub async fn start_control_plane(
    targets: &[String],
    initial_state: ControlState,
) -> io::Result<ControlPlane> {
    if targets.is_empty() {
        return Ok(ControlPlane::disabled());
    }

    let parsed_targets = parse_targets(targets)?;
    let connectivity_notify = Arc::new(Notify::new());
    let agents: Vec<AgentHandle> = parsed_targets
        .into_iter()
        .map(|target| {
            info!("control-plane target enabled: {}", target.label);
            spawn_agent(target, connectivity_notify.clone())
        })
        .collect();

    let runtime = Arc::new(ControlRuntime {
        agents,
        session_id: new_session_id(),
        next_request_id: AtomicU64::new(1),
        state: Mutex::new(initial_state),
        connectivity_notify,
        shutdown_token: CancellationToken::new(),
        recovery_in_progress: AtomicBool::new(false),
        shutting_down: AtomicBool::new(false),
    });

    if !runtime.wait_for_all_connected(CONNECT_TIMEOUT).await {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "failed to connect all control agents",
        ));
    }

    runtime
        .broadcast_phase(RequestPhase::PreStart, PRE_START_TIMEOUT)
        .await?;
    {
        let mut state = runtime.state.lock().await;
        state.mark_starting();
    }

    tokio::spawn(runtime.clone().monitor_quorum());

    Ok(ControlPlane {
        inner: Some(runtime),
    })
}

#[derive(Debug)]
struct ControlRuntime {
    agents: Vec<AgentHandle>,
    session_id: String,
    next_request_id: AtomicU64,
    state: Mutex<ControlState>,
    connectivity_notify: Arc<Notify>,
    shutdown_token: CancellationToken,
    recovery_in_progress: AtomicBool,
    shutting_down: AtomicBool,
}

impl ControlRuntime {
    async fn broadcast_phase(
        &self,
        phase: RequestPhase,
        phase_timeout: Duration,
    ) -> io::Result<()> {
        let request = {
            let state = self.state.lock().await.clone();
            let request_id = self.next_request_id.fetch_add(1, Ordering::AcqRel);
            ControlRequest::new(
                self.session_id.clone(),
                request_id,
                phase,
                phase_timeout,
                state,
            )
        };

        let mut join_set = JoinSet::new();
        for agent in &self.agents {
            let request = request.clone();
            let agent = agent.clone();
            join_set.spawn(async move {
                let label = agent.label().to_string();
                let response = agent.request(request, phase_timeout).await;
                (label, response)
            });
        }

        while let Some(joined) = join_set.join_next().await {
            let (label, response) = joined.map_err(|err| io::Error::other(err.to_string()))?;
            let response = response?;
            if !response.success {
                return Err(io::Error::other(format!(
                    "control agent {label} rejected request {}: {}",
                    response.id,
                    response.message.unwrap_or_else(|| "unknown error".into())
                )));
            }
        }

        Ok(())
    }

    async fn monitor_quorum(self: Arc<Self>) {
        loop {
            self.connectivity_notify.notified().await;
            if self.shutting_down.load(Ordering::Acquire) {
                continue;
            }

            let is_running = {
                let state = self.state.lock().await;
                state.state == ControlStatePhase::Running
            };

            if !is_running || self.all_connected() {
                continue;
            }

            if self
                .recovery_in_progress
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }

            if self.shutting_down.load(Ordering::Acquire) || self.all_connected() {
                self.recovery_in_progress.store(false, Ordering::Release);
                continue;
            }

            warn!("control-plane quorum lost, entering bounded recovery");
            if self.wait_for_all_connected(RECONNECT_GRACE_TIMEOUT).await {
                let recovery_result = match self
                    .broadcast_phase(RequestPhase::SyncState, POST_START_TIMEOUT)
                    .await
                {
                    Ok(()) => {
                        self.broadcast_phase(RequestPhase::PostStart, POST_START_TIMEOUT)
                            .await
                    }
                    Err(err) => Err(err),
                };

                if recovery_result.is_ok() {
                    info!("control-plane quorum restored");
                    self.recovery_in_progress.store(false, Ordering::Release);
                    continue;
                }
            }

            warn!("control-plane recovery failed, requesting shutdown");
            self.recovery_in_progress.store(false, Ordering::Release);
            self.shutdown_token.cancel();
            return;
        }
    }

    fn all_connected(&self) -> bool {
        self.agents.iter().all(AgentHandle::is_connected)
    }

    async fn wait_for_all_connected(&self, wait_timeout: Duration) -> bool {
        let deadline = Instant::now() + wait_timeout;
        loop {
            if self.all_connected() {
                return true;
            }

            let now = Instant::now();
            if now >= deadline {
                return false;
            }

            let notified = self.connectivity_notify.notified();
            if self.all_connected() {
                return true;
            }

            if timeout(deadline.saturating_duration_since(now), notified)
                .await
                .is_err()
            {
                return false;
            }
        }
    }
}

fn parse_targets(targets: &[String]) -> io::Result<Vec<UnixTarget>> {
    let mut dedup = HashSet::new();
    let mut parsed = Vec::new();

    for raw_target in targets {
        let target = parse_unix_target(raw_target)?;
        if dedup.insert(target.label.clone()) {
            parsed.push(target);
        } else {
            warn!("duplicate control target ignored: {}", raw_target);
        }
    }

    Ok(parsed)
}

fn new_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    format!("boot-{millis:x}-{pid:x}")
}
