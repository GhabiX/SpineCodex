use super::InputPressureState;
use crate::SpineOperationFact;

fn open() -> SpineOperationFact {
    SpineOperationFact::Open {
        summary: "task".to_string(),
    }
}

fn close() -> SpineOperationFact {
    SpineOperationFact::Close {
        memory: "done".to_string(),
    }
}

fn next() -> SpineOperationFact {
    SpineOperationFact::Next {
        closed_memory: "done".to_string(),
        next_summary: "next".to_string(),
    }
}

#[test]
fn close_restores_the_open_sampling_pressure() {
    let mut pressure = InputPressureState::default();

    let operation = open();
    pressure.apply_sampling(Some(100), [&operation]);
    let operation = close();
    pressure.apply_sampling(Some(900), [&operation]);

    assert_eq!(pressure.current_input_tokens(), Some(100));
}

#[test]
fn next_rebases_the_sibling_checkpoint() {
    let mut pressure = InputPressureState::default();

    let operation = open();
    pressure.apply_sampling(Some(100), [&operation]);
    let operation = next();
    pressure.apply_sampling(Some(900), [&operation]);
    let operation = close();
    pressure.apply_sampling(Some(300), [&operation]);

    assert_eq!(pressure.current_input_tokens(), Some(100));
}

#[test]
fn nested_close_restores_each_open_checkpoint() {
    let mut pressure = InputPressureState::default();

    let operation = open();
    pressure.apply_sampling(Some(100), [&operation]);
    pressure.apply_sampling(Some(250), [&open()]);
    pressure.apply_sampling(Some(700), [&close()]);
    assert_eq!(pressure.current_input_tokens(), Some(250));

    pressure.apply_sampling(Some(400), [&close()]);
    assert_eq!(pressure.current_input_tokens(), Some(100));
}

#[test]
fn missing_usage_stays_unknown_across_open_and_close() {
    let mut pressure = InputPressureState::default();

    let operation = open();
    pressure.apply_sampling(None, [&operation]);
    let operation = close();
    pressure.apply_sampling(Some(900), [&operation]);

    assert_eq!(pressure.current_input_tokens(), None);
}

#[test]
fn compact_starts_a_new_pressure_epoch() {
    let mut pressure = InputPressureState::default();

    pressure.apply_sampling(Some(100), [&open()]);
    pressure.compact();
    pressure.apply_sampling(Some(900), [&close()]);

    assert_eq!(pressure.current_input_tokens(), Some(900));
}

#[test]
fn root_close_does_not_discard_the_current_sampling_pressure() {
    let mut pressure = InputPressureState::default();

    pressure.apply_sampling(Some(900), [&close()]);

    assert_eq!(pressure.current_input_tokens(), Some(900));
}

#[test]
fn root_next_rebases_the_new_sibling_to_the_current_pressure() {
    let mut pressure = InputPressureState::default();

    pressure.apply_sampling(Some(900), [&next()]);

    assert_eq!(pressure.current_input_tokens(), Some(900));
}
