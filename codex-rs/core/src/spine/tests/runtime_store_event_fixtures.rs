use super::*;

pub(crate) fn logged_events(runtime: &SpineRuntime) -> Vec<LoggedSpineLedgerEvent> {
    runtime.store.events().expect("events")
}

pub(crate) fn event_log(runtime: &SpineRuntime) -> Vec<SpineLedgerEvent> {
    logged_events(runtime)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

pub(crate) fn event_log_debug(runtime: &SpineRuntime) -> Vec<String> {
    event_log(runtime)
        .into_iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

pub(crate) fn assert_parse_stack_tree_and_events_unchanged(
    runtime: &SpineRuntime,
    parse_stack_before: &ParseStack,
    tree_before: &str,
    events_before: &[String],
) {
    assert_eq!(runtime.parse_stack(), parse_stack_before);
    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before
    );
    assert_eq!(event_log_debug(runtime), events_before);
}

pub(crate) fn ledger_event_debug(runtime: &SpineRuntime) -> Vec<String> {
    runtime
        .ledger
        .events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

pub(crate) fn assert_pending_close_retry_state(runtime: &SpineRuntime, ledger_before: &[String]) {
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Close(_)))),
        "failed close-like reduce should retain the zero-width Close token for retry"
    );
    assert_eq!(ledger_event_debug(runtime), ledger_before);
}

pub(crate) fn assert_pending_compact_retry_state(runtime: &SpineRuntime, ledger_before: &[String]) {
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..)))),
        "failed root compact reduce should retain the zero-width Compact token for retry"
    );
    assert_eq!(ledger_event_debug(runtime), ledger_before);
}
