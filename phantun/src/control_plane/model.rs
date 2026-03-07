use serde::{Deserialize, Serialize};
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
    Stopping,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    ProcessExit,
    MainLoopError,
    Signal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ControlState {
    pub state: TunnelState,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedState {
    pub seq: u64,
    pub state: ControlState,
}

impl PublishedState {
    pub fn new(state: ControlState) -> Self {
        Self { seq: 0, state }
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
    pub fn from_published(message_type: MessageType, published: &PublishedState) -> Self {
        Self {
            v: 1,
            message_type,
            ts: unix_timestamp_millis(),
            id: seq_to_message_id(published.seq),
            data: published.state.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboundMessageType {
    Ack,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InboundMessage {
    #[serde(rename = "type")]
    pub message_type: InboundMessageType,
    pub id: String,
}

impl InboundMessage {
    pub fn acked_seq(&self) -> Option<u64> {
        if self.message_type != InboundMessageType::Ack {
            return None;
        }

        message_id_to_seq(&self.id)
    }
}

pub fn seq_to_message_id(seq: u64) -> String {
    format!("m{seq}")
}

pub fn message_id_to_seq(message_id: &str) -> Option<u64> {
    let suffix = message_id.strip_prefix('m')?;
    suffix.parse().ok()
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::{
        ControlMessage, ControlMode, ControlState, InboundMessage, MessageType, PublishedState,
        StopReason, TunnelState, message_id_to_seq, seq_to_message_id,
    };

    #[test]
    fn serialize_stopping_event() {
        let mut state = ControlState::new(
            ControlMode::Client,
            Some("127.0.0.1:1234".to_string()),
            Some("1.2.3.4:4567".to_string()),
        );
        state.state = TunnelState::Stopping;
        state.reason = Some(StopReason::Signal);
        state.dev = Some("tun0".to_string());

        let msg = ControlMessage::from_published(
            MessageType::Event,
            &PublishedState { seq: 42, state },
        );
        let json = serde_json::to_string(&msg).unwrap();

        assert!(json.contains("\"type\":\"event\""));
        assert!(json.contains("\"id\":\"m42\""));
        assert!(json.contains("\"state\":\"stopping\""));
        assert!(json.contains("\"reason\":\"signal\""));
    }

    #[test]
    fn parse_ack_message_id() {
        let inbound: InboundMessage = serde_json::from_str(r#"{"type":"ack","id":"m15"}"#).unwrap();
        assert_eq!(inbound.acked_seq(), Some(15));
        assert_eq!(seq_to_message_id(27), "m27");
        assert_eq!(message_id_to_seq("m27"), Some(27));
        assert_eq!(message_id_to_seq("bad"), None);
    }
}
