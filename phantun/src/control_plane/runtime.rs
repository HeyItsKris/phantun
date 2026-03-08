use super::model::{ControlState, PublishedState};
use super::reporter::ControlReporter;
use super::sink_unix::{parse_unix_target, run_unix_sink};
use super::sync_state::ControlSyncState;
use log::{info, warn};
use std::collections::HashSet;
use std::io;
use tokio::sync::watch;

pub fn start_control_plane(
    targets: &[String],
    initial_state: ControlState,
) -> io::Result<ControlReporter> {
    if targets.is_empty() {
        return Ok(ControlReporter::disabled());
    }

    let parsed_targets = parse_targets(targets)?;
    let target_labels: Vec<String> = parsed_targets.iter().map(|target| target.label.clone()).collect();
    let sync = ControlSyncState::new(&target_labels);
    let (tx, rx) = watch::channel(PublishedState::new(initial_state));

    for target in parsed_targets {
        info!("control-plane target enabled: {}", target.label);
        tokio::spawn(run_unix_sink(target, rx.clone(), sync.clone()));
    }

    Ok(ControlReporter::new(tx, sync))
}

fn parse_targets(targets: &[String]) -> io::Result<Vec<super::sink_unix::UnixTarget>> {
    let mut dedup = HashSet::new();
    let mut parsed = Vec::new();

    for raw_target in targets {
        let target = parse_unix_target(raw_target)?;
        if dedup.insert(target.label.clone()) {
            parsed.push(target);
        } else {
            warn!(
                "duplicate control target ignored: {}",
                raw_target.as_str()
            );
        }
    }

    Ok(parsed)
}
