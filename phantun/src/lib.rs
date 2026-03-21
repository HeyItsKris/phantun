use std::time::Duration;

pub mod control_plane;
pub mod shutdown;
pub mod task_group;
pub mod utils;

pub const UDP_TTL: Duration = Duration::from_secs(180);
