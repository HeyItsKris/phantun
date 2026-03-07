use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use tokio::time::{Duration, Instant, timeout};

#[derive(Debug, Default)]
pub struct SequenceTracker {
    seen: Mutex<HashSet<u64>>,
    notify: Notify,
}

impl SequenceTracker {
    pub async fn mark(&self, seq: u64) {
        let mut seen = self.seen.lock().await;
        seen.insert(seq);
        drop(seen);
        self.notify.notify_waiters();
    }

    pub async fn wait_for(&self, seq: u64, wait_timeout: Duration) -> bool {
        let deadline = Instant::now() + wait_timeout;
        loop {
            if self.take_if_seen(seq).await {
                return true;
            }

            let now = Instant::now();
            if now >= deadline {
                return false;
            }

            let remaining = deadline.saturating_duration_since(now);
            let notified = self.notify.notified();
            if self.take_if_seen(seq).await {
                return true;
            }

            if timeout(remaining, notified).await.is_err() {
                return false;
            }
        }
    }

    async fn take_if_seen(&self, seq: u64) -> bool {
        let mut seen = self.seen.lock().await;
        seen.remove(&seq)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ControlSyncState {
    pub acked: Arc<SequenceTracker>,
    pub delivered: Arc<SequenceTracker>,
}
