use super::model::ControlState;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const PROTOCOL_VERSION: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Request,
    Response,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestPhase {
    PreStart,
    PostStart,
    SyncState,
    PreStop,
    PostStop,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlRequest {
    pub v: u8,
    pub kind: MessageKind,
    pub session_id: String,
    pub id: u64,
    pub phase: RequestPhase,
    pub deadline_ms: u64,
    pub payload: ControlState,
}

impl ControlRequest {
    pub fn new(
        session_id: String,
        id: u64,
        phase: RequestPhase,
        deadline: Duration,
        payload: ControlState,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: MessageKind::Request,
            session_id,
            id,
            phase,
            deadline_ms: deadline.as_millis() as u64,
            payload,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlResponse {
    pub v: u8,
    pub kind: MessageKind,
    pub session_id: String,
    pub id: u64,
    pub success: bool,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ControlRequest, ControlResponse, MessageKind, RequestPhase};
    use crate::control_plane::{ControlMode, ControlState};
    use std::time::Duration;

    #[test]
    fn serialize_request() {
        let req = ControlRequest::new(
            "boot-a".into(),
            3,
            RequestPhase::PreStart,
            Duration::from_secs(3),
            ControlState::new(ControlMode::Client, Some("127.0.0.1:1".into()), None),
        );

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"kind\":\"request\""));
        assert!(json.contains("\"phase\":\"pre_start\""));
        assert!(json.contains("\"id\":3"));
    }

    #[test]
    fn deserialize_response() {
        let response: ControlResponse = serde_json::from_str(
            r#"{"v":2,"kind":"response","session_id":"boot-a","id":7,"success":true,"message":null}"#,
        )
        .unwrap();

        assert_eq!(response.kind, MessageKind::Response);
        assert!(response.success);
        assert_eq!(response.id, 7);
    }
}
