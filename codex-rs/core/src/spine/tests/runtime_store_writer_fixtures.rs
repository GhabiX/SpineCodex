use super::*;
use std::time::Duration;
use std::time::Instant;

pub(crate) fn eventually_load_or_create_writer(
    rollout: &std::path::Path,
    raw_len: u64,
) -> SpineRuntime {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_err = None;
    loop {
        match SpineRuntime::load_or_create(rollout, raw_len) {
            Ok(runtime) => return runtime,
            Err(err) => {
                last_err = Some(err);
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    panic!(
        "writer lock should release after drop: {}",
        last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    );
}

pub(crate) fn eventually_set_replayed_writer(
    state: &mut SpineSessionState,
    rollout: &std::path::Path,
    raw_len: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_err = None;
    loop {
        let replayed = SpineRuntime::load_for_rollout(rollout, raw_len)
            .expect("reload read-only replay after first live runtime drops")
            .expect("sidecar exists");
        match state.set_replayed(raw_len, Some(replayed)) {
            Ok(()) => return,
            Err(err) => {
                last_err = Some(err);
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    panic!(
        "replayed runtime can become live writer after lock release: {}",
        last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    );
}
