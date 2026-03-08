use std::time::Duration;

pub mod utils;
pub mod control_plane;
pub mod task_group;

pub const UDP_TTL: Duration = Duration::from_secs(180);
