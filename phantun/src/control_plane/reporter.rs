use super::model::{ControlState, DownReason, TunnelState};
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct ControlReporter {
    tx: Option<watch::Sender<ControlState>>,
}

impl ControlReporter {
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    pub(crate) fn new(tx: watch::Sender<ControlState>) -> Self {
        Self { tx: Some(tx) }
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

    pub fn publish_down(&self, reason: DownReason) {
        self.update(|state| {
            state.state = TunnelState::Down;
            state.reason = Some(reason);
        });
    }

    fn update<F>(&self, mutator: F)
    where
        F: FnOnce(&mut ControlState),
    {
        let Some(tx) = &self.tx else {
            return;
        };

        let mut next = tx.borrow().clone();
        mutator(&mut next);
        let _ = tx.send_replace(next);
    }
}
