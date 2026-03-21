use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlMode {
    Client,
    Server,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStatePhase {
    PreStart,
    Starting,
    PostStart,
    Running,
    PreStop,
    Stopping,
    PostStop,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    ProcessExit,
    MainLoopError,
    Signal,
    AgentDisconnect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ControlState {
    pub state: ControlStatePhase,
    pub reason: Option<StopReason>,
    pub mode: ControlMode,
    pub local: Option<String>,
    pub remote: Option<String>,
    pub dev: Option<String>,
    pub mtu: Option<u32>,
    pub addr4: Option<String>,
    pub addr6: Option<String>,
}

impl ControlState {
    pub fn new(mode: ControlMode, local: Option<String>, remote: Option<String>) -> Self {
        Self {
            state: ControlStatePhase::PreStart,
            reason: None,
            mode,
            local,
            remote,
            dev: None,
            mtu: None,
            addr4: None,
            addr6: None,
        }
    }

    pub fn mark_starting(&mut self) {
        self.state = ControlStatePhase::Starting;
        self.reason = None;
    }

    pub fn mark_post_start(
        &mut self,
        dev: Option<String>,
        mtu: Option<u32>,
        addr4: Option<String>,
        addr6: Option<String>,
    ) {
        self.state = ControlStatePhase::PostStart;
        self.reason = None;
        self.dev = dev;
        self.mtu = mtu;
        self.addr4 = addr4;
        self.addr6 = addr6;
    }

    pub fn mark_running(&mut self) {
        self.state = ControlStatePhase::Running;
        self.reason = None;
    }

    pub fn mark_pre_stop(&mut self, reason: StopReason) {
        self.state = ControlStatePhase::PreStop;
        self.reason = Some(reason);
    }

    pub fn mark_stopping(&mut self, reason: StopReason) {
        self.state = ControlStatePhase::Stopping;
        self.reason = Some(reason);
    }

    pub fn mark_post_stop(&mut self, reason: StopReason) {
        self.state = ControlStatePhase::PostStop;
        self.reason = Some(reason);
    }

    pub fn mark_stopped(&mut self) {
        self.state = ControlStatePhase::Stopped;
    }
}
