use crate::ContextEpoch;
use crate::MAX_VISIBLE_CONTEXT_ITEMS;
use crate::RawBoundary;
use crate::SourceDigestVersion;
use crate::SourceLedger;
use crate::SpineChar;
use crate::ThreadNamespace;
use sha2::Digest;

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

#[test]
fn v1_empty_digest_is_sha256_of_empty_json_array() {
    let source = ledger();
    let expected = format!("{:x}", sha2::Sha256::digest(b"[]"));
    assert_eq!(source.digest().as_str(), expected);
}

#[test]
fn v2_empty_digest_is_not_v1() {
    let v2 = SourceLedger::new_with_version(
        ThreadNamespace::parse("thread").expect("valid namespace"),
        ContextEpoch::ZERO,
        SourceDigestVersion::V2,
    )
    .expect("v2 ledger");
    assert_ne!(v2.digest(), ledger().digest());
}

#[test]
fn v2_incremental_append_matches_full_recompute() {
    let characters = [
        SpineChar::Opaque {
            boundary: RawBoundary(1),
        },
        SpineChar::Opaque {
            boundary: RawBoundary(2),
        },
        SpineChar::Opaque {
            boundary: RawBoundary(3),
        },
    ];
    let mut incremental = SourceLedger::new_with_version(
        ThreadNamespace::parse("thread").expect("valid namespace"),
        ContextEpoch::ZERO,
        SourceDigestVersion::V2,
    )
    .expect("v2 ledger");
    incremental
        .append([characters[0].clone()])
        .expect("append first");
    incremental
        .append([characters[1].clone(), characters[2].clone()])
        .expect("append rest");
    let mut full = SourceLedger::new_with_version(
        ThreadNamespace::parse("thread").expect("valid namespace"),
        ContextEpoch::ZERO,
        SourceDigestVersion::V2,
    )
    .expect("v2 ledger");
    full.append(characters).expect("append");
    assert_eq!(incremental.digest(), full.digest());
}
