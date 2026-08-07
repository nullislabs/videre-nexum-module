//! Freeze pin for `videre:value-flow@1.0.0`. The strict policy in
//! `docs/design/value-flow-versioning-policy.md` allows additive record
//! growth within 1.x, keeps every variant closed, and reserves variant
//! growth for 2.0; a version regression, a 1.x variant case, or a dropped
//! frozen field fails here instead of reaching a consumer.

use std::path::Path;

fn wit_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit/videre-value-flow/types.wit");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Item names inside the `header { .. }` block, doc lines skipped.
fn block_items(source: &str, header: &str) -> Vec<String> {
    let start = source
        .find(header)
        .unwrap_or_else(|| panic!("`{header}` not found in types.wit"));
    let body = &source[start + header.len()..];
    let end = body.find('}').expect("unterminated block in types.wit");
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("///"))
        .map(|line| {
            line.split([':', '(', ','])
                .next()
                .expect("split yields at least one part")
                .trim()
                .to_owned()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn package_version_is_frozen_at_1_0_0() {
    assert!(
        wit_source().contains("package videre:value-flow@1.0.0;"),
        "videre:value-flow must stay at 1.0.0 through the 1.x window"
    );
}

#[test]
fn asset_variant_cases_are_closed() {
    let cases = block_items(&wit_source(), "variant asset {");
    assert_eq!(
        cases,
        ["native", "erc20", "service"],
        "the asset case set is closed at 1.0; any variant growth is 2.0 \
         (docs/design/value-flow-versioning-policy.md)"
    );
}

#[test]
fn frozen_record_fields_stay_present() {
    let source = wit_source();
    for (header, frozen) in [
        ("record erc20 {", &["token"][..]),
        ("record service-desc {", &["description"]),
        ("record asset-amount {", &["asset", "amount"]),
    ] {
        let fields = block_items(&source, header);
        for name in frozen {
            assert!(
                fields.iter().any(|field| field == name),
                "`{header}` lost frozen field `{name}`; 1.x record growth is additive only"
            );
        }
    }
}
