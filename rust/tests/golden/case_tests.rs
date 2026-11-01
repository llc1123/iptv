//! Case-level invariant tests: every fixture case must stay internally
//! consistent with the wire layout, padding, mapping, and token rules it
//! claims to freeze for the future Rust implementations.

use super::{family, load};
use crate::checks;
use crate::loader::Case;

fn assert_all_ok(results: Vec<Result<(), crate::loader::GoldenError>>) {
    for result in results {
        assert!(result.is_ok(), "fixture invariant failed: {result:?}");
    }
}

#[test]
fn option60_encode_cases_follow_the_wire_layout() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "option60.json")
            .cases
            .iter()
            .map(checks::option60_encode_layout)
            .collect(),
    );
}

#[test]
fn option60_reject_vectors_violate_their_claimed_rules() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "option60.json")
            .cases
            .iter()
            .map(checks::option60_reject_violation)
            .collect(),
    );
}

#[test]
fn padding_cases_follow_their_padding_rule() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "padding.json")
            .cases
            .iter()
            .map(checks::padding_rule)
            .collect(),
    );
}

#[test]
fn epg_auth_cases_pin_uppercase_pkcs7_lengths() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "epg_auth.json")
            .cases
            .iter()
            .map(checks::epg_auth_shape)
            .collect(),
    );
}

#[test]
fn catchup_numeric_cases_follow_the_threshold_rule() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "catchup.json")
            .cases
            .iter()
            .map(checks::catchup_numeric_rule)
            .collect(),
    );
}

#[test]
fn status_cases_match_the_operation_mapping() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "status_shapes.json")
            .cases
            .iter()
            .map(checks::status_mapping)
            .collect(),
    );
}

#[test]
fn hls_rewrite_cases_only_reference_registered_tokens() {
    let corpus = load();

    assert_all_ok(
        family(&corpus, "hls_rewrite.json")
            .cases
            .iter()
            .map(checks::hls_rewrite_tokens)
            .collect(),
    );
}

#[test]
fn every_case_id_is_unique_within_its_family() {
    let corpus = load();

    for (path, family_file) in &corpus.families {
        let mut seen: Vec<&str> = family_file
            .cases
            .iter()
            .map(|Case { id, .. }| id.as_str())
            .collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "duplicate case id in {path}");
    }
}
