use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use tokio::time::{Duration, Instant, timeout};

#[derive(Debug, Default)]
struct TrackerState {
    seen: HashMap<u64, HashSet<usize>>,
    retired_seq: u64,
}

#[derive(Debug)]
pub struct SequenceTracker {
    target_ids: HashMap<String, usize>,
    required_count: usize,
    state: Mutex<TrackerState>,
    notify: Notify,
}

impl SequenceTracker {
    pub fn new(target_labels: &[String]) -> Self {
        let mut target_ids = HashMap::with_capacity(target_labels.len());
        for (idx, label) in target_labels.iter().enumerate() {
            target_ids.insert(label.clone(), idx);
        }

        Self {
            required_count: target_ids.len(),
            target_ids,
            state: Mutex::new(TrackerState::default()),
            notify: Notify::new(),
        }
    }

    pub async fn mark(&self, seq: u64, target_label: &str) {
        let Some(target_id) = self.target_ids.get(target_label).copied() else {
            return;
        };

        let mut state = self.state.lock().await;
        if seq <= state.retired_seq {
            return;
        }

        state.seen.entry(seq).or_default().insert(target_id);
        drop(state);
        self.notify.notify_waiters();
    }

    pub async fn wait_for(&self, seq: u64, wait_timeout: Duration) -> bool {
        if self.required_count == 0 {
            return true;
        }

        let deadline = Instant::now() + wait_timeout;
        loop {
            if self.take_if_completed(seq).await {
                return true;
            }

            let now = Instant::now();
            if now >= deadline {
                self.retire(seq).await;
                return false;
            }

            let remaining = deadline.saturating_duration_since(now);
            let notified = self.notify.notified();
            if self.take_if_completed(seq).await {
                return true;
            }

            if timeout(remaining, notified).await.is_err() {
                self.retire(seq).await;
                return false;
            }
        }
    }

    async fn take_if_completed(&self, seq: u64) -> bool {
        let mut state = self.state.lock().await;
        let is_completed = state
            .seen
            .get(&seq)
            .map(|seen| seen.len() >= self.required_count)
            .unwrap_or(false);
        if !is_completed {
            return false;
        }

        retire_seq_locked(&mut state, seq);
        true
    }

    async fn retire(&self, seq: u64) {
        let mut state = self.state.lock().await;
        retire_seq_locked(&mut state, seq);
    }
}

fn retire_seq_locked(state: &mut TrackerState, seq: u64) {
    if seq > state.retired_seq {
        state.retired_seq = seq;
    }
    state.seen.retain(|candidate, _| *candidate > state.retired_seq);
}

#[derive(Clone, Debug)]
pub struct ControlSyncState {
    pub acked: Arc<SequenceTracker>,
    pub delivered: Arc<SequenceTracker>,
}

impl ControlSyncState {
    pub fn new(target_labels: &[String]) -> Self {
        Self {
            acked: Arc::new(SequenceTracker::new(target_labels)),
            delivered: Arc::new(SequenceTracker::new(target_labels)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SequenceTracker;
    use std::time::Duration;

    #[tokio::test]
    async fn waits_for_all_targets() {
        let tracker = SequenceTracker::new(&["unix:/tmp/a.sock".into(), "unix:/tmp/b.sock".into()]);
        tracker.mark(8, "unix:/tmp/a.sock").await;
        assert!(!tracker.wait_for(8, Duration::from_millis(10)).await);
    }

    #[tokio::test]
    async fn completes_when_every_target_marks_sequence() {
        let tracker = SequenceTracker::new(&["unix:/tmp/a.sock".into(), "unix:/tmp/b.sock".into()]);
        tracker.mark(9, "unix:/tmp/a.sock").await;
        tracker.mark(9, "unix:/tmp/b.sock").await;
        assert!(tracker.wait_for(9, Duration::from_millis(10)).await);
    }
}
