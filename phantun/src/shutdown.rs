use crate::control_plane::{ControlPlane, StopReason};
use crate::task_group::TaskGroup;
use log::info;
use std::io;
use std::time::Duration;
use tokio::task::JoinHandle;

pub async fn run_controlled_shutdown(
    main_loop: &mut JoinHandle<io::Result<()>>,
    abort_main_loop: bool,
    task_group: &TaskGroup,
    control_plane: &ControlPlane,
    stop_reason: StopReason,
    task_drain_timeout: Duration,
) {
    if !control_plane.prepare_stop(stop_reason).await {
        info!("control-plane pre_stop barrier failed");
    }

    task_group.cancel();
    if abort_main_loop {
        main_loop.abort();
        let _ = (&mut *main_loop).await;
    }

    if !task_group.wait_timeout(task_drain_timeout).await {
        info!(
            "Task drain did not finish within {:?}, continuing shutdown",
            task_drain_timeout
        );
    }

    if !control_plane.complete_stop().await {
        info!("control-plane post_stop barrier failed");
    }
}
