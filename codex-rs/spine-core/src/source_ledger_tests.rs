use crate::ContextEpoch;
use crate::MAX_VISIBLE_CONTEXT_ITEMS;
use crate::RawBoundary;
use crate::SourceLedger;
use crate::SpineChar;
use crate::ThreadNamespace;

fn ledger() -> SourceLedger {
    SourceLedger::new(
        ThreadNamespace::parse("thread").expect("valid namespace"),
        ContextEpoch::ZERO,
    )
    .expect("source ledger")
}

#[test]
fn source_ledger_accepts_more_cells_than_one_turn_visible_item_cap() {
    let mut source = ledger();
    let count = MAX_VISIBLE_CONTEXT_ITEMS.saturating_add(1);
    let characters = (1..=count as u64).map(|ordinal| SpineChar::Opaque {
        boundary: RawBoundary(ordinal),
    });
    let inserted = source
        .append(characters)
        .expect("append-only source history is not capped at visible context items");
    assert_eq!(inserted.len(), count);
    assert_eq!(source.snapshot().cells().len(), count);
}
