//! Focused public-RPC checks independent of legacy live-rollback test modules.
//! The canonical modules remain registered in `all`; this target does not replace it.

#[path = "suite/v2/initialize.rs"]
mod initialize;
#[path = "suite/v2/request_user_input.rs"]
mod request_user_input;
