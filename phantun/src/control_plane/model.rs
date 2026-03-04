use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlMode {
    Client,
    Server,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelState {
    Starting,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownReason {
    ProcessExit,
    MainLoopError,
    Signal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ControlState {
    pub state: TunnelState,
    pub reason: Option<DownReason>,
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
            state: TunnelState::Starting,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    Snapshot,
    Event,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlMessage {
    pub v: u8,
    #[serde(rename = "type")]
    pub message_type: MessageType,
    pub ts: u64,
    pub id: String,
    pub data: ControlState,
}

impl ControlMessage {
    pub fn new(message_type: MessageType, id: String, data: ControlState) -> Self {
        Self {
            v: 1,
            message_type,
            ts: unix_timestamp_millis(),
            id,
            data,
        }
    }
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{ControlMessage, ControlMode, ControlState, DownReason, MessageType, TunnelState};

    #[test]
    fn serialize_down_event() {
        let mut state = ControlState::new(
            ControlMode::Client,
            Some("127.0.0.1:1234".to_string()),
            Some("1.2.3.4:4567".to_string()),
        );
        state.state = TunnelState::Down;
        state.reason = Some(DownReason::ProcessExit);
        state.dev = Some("tun0".to_string());

        let msg = ControlMessage::new(MessageType::Event, "m1".to_string(), state);
        let json = serde_json::to_string(&msg).unwrap();

        assert!(json.contains("\"v\":1"));
        assert!(json.contains("\"type\":\"event\""));
        assert!(json.contains("\"state\":\"down\""));
        assert!(json.contains("\"reason\":\"process_exit\""));
    }
}
