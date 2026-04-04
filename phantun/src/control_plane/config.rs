use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct ControlPlaneTimeouts {
    pub connect_timeout: Duration,
    pub pre_start_timeout: Duration,
    pub post_start_timeout: Duration,
    pub sync_state_timeout: Duration,
    pub reconnect_grace_timeout: Duration,
    pub pre_stop_timeout: Duration,
    pub post_stop_timeout: Duration,
}

impl ControlPlaneTimeouts {
    pub const CONNECT_TIMEOUT_DEFAULT: &str = "3s";
    pub const PRE_START_TIMEOUT_DEFAULT: &str = "3s";
    pub const POST_START_TIMEOUT_DEFAULT: &str = "3s";
    pub const SYNC_STATE_TIMEOUT_DEFAULT: &str = "3s";
    pub const RECONNECT_GRACE_TIMEOUT_DEFAULT: &str = "3s";
    pub const PRE_STOP_TIMEOUT_DEFAULT: &str = "2s";
    pub const POST_STOP_TIMEOUT_DEFAULT: &str = "2s";
}

impl Default for ControlPlaneTimeouts {
    fn default() -> Self {
        Self {
            connect_timeout: humantime::parse_duration(Self::CONNECT_TIMEOUT_DEFAULT).unwrap(),
            pre_start_timeout: humantime::parse_duration(Self::PRE_START_TIMEOUT_DEFAULT).unwrap(),
            post_start_timeout: humantime::parse_duration(Self::POST_START_TIMEOUT_DEFAULT)
                .unwrap(),
            sync_state_timeout: humantime::parse_duration(Self::SYNC_STATE_TIMEOUT_DEFAULT)
                .unwrap(),
            reconnect_grace_timeout: humantime::parse_duration(
                Self::RECONNECT_GRACE_TIMEOUT_DEFAULT,
            )
            .unwrap(),
            pre_stop_timeout: humantime::parse_duration(Self::PRE_STOP_TIMEOUT_DEFAULT).unwrap(),
            post_stop_timeout: humantime::parse_duration(Self::POST_STOP_TIMEOUT_DEFAULT).unwrap(),
        }
    }
}

pub fn parse_duration_arg(raw: &str) -> Result<Duration, String> {
    humantime::parse_duration(raw).map_err(|err| format!("invalid duration {raw:?}: {err}"))
}
