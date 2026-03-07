use super::model::{ControlState, PublishedState, StopReason, TunnelState};
use super::sync_state::ControlSyncState;
use std::time::Duration;
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct ControlReporter {
    tx: Option<watch::Sender<PublishedState>>,
    sync: Option<ControlSyncState>,
}

impl ControlReporter {
    pub fn disabled() -> Self {
        Self {
            tx: None,
            sync: None,
        }
    }

    pub(crate) fn new(tx: watch::Sender<PublishedState>, sync: ControlSyncState) -> Self {
        Self {
            tx: Some(tx),
            sync: Some(sync),
        }
    }

    pub fn publish_starting(&self) {
        self.update(|state| {
            state.state = TunnelState::Starting;
            state.reason = None;
        });
    }

    pub fn publish_up(
        &self,
        dev: Option<String>,
        mtu: Option<u32>,
        addr4: Option<String>,
        addr6: Option<String>,
    ) {
        self.update(|state| {
            state.state = TunnelState::Up;
            state.reason = None;
            state.dev = dev;
            state.mtu = mtu;
            state.addr4 = addr4;
            state.addr6 = addr6;
        });
    }

    pub fn publish_stopping(&self, reason: StopReason) -> Option<u64> {
        self.update(|state| {
            state.state = TunnelState::Stopping;
            state.reason = Some(reason);
        })
    }

    pub fn publish_down(&self) -> Option<u64> {
        self.update(|state| {
            state.state = TunnelState::Down;
            state.reason = None;
        })
    }

    pub async fn publish_stopping_and_wait_ack(
        &self,
        reason: StopReason,
        timeout: Duration,
    ) -> bool {
        let Some(seq) = self.publish_stopping(reason) else {
            return true;
        };

        let Some(sync) = &self.sync else {
            return true;
        };

        sync.acked.wait_for(seq, timeout).await
    }

    pub async fn publish_down_and_wait_delivery(&self, timeout: Duration) -> bool {
        let Some(seq) = self.publish_down() else {
            return true;
        };

        let Some(sync) = &self.sync else {
            return true;
        };

        sync.delivered.wait_for(seq, timeout).await
    }

    fn update<F>(&self, mutator: F) -> Option<u64>
    where
        F: FnOnce(&mut ControlState),
    {
        let Some(tx) = &self.tx else {
            return None;
        };

        let current = tx.borrow().clone();
        let mut next_state = current.state.clone();
        mutator(&mut next_state);

        // Emit events only when state actually changes.
        if next_state == current.state {
            return None;
        }

        let next_seq = current.seq.wrapping_add(1);
        let _ = tx.send_replace(PublishedState {
            seq: next_seq,
            state: next_state,
        });

        Some(next_seq)
    }
}
