use super::runtime_check;
use pretty_assertions::assert_eq;

#[test]
fn doctor_provenance_reports_product_release_not_protocol_version() {
    let check = runtime_check();
    assert_eq!(
        check
            .details
            .iter()
            .filter(|detail| detail.starts_with("version: "))
            .collect::<Vec<_>>(),
        vec![&"version: 0.4.0".to_string()]
    );
}
