//! Freeze pin for `videre:value-flow@1.0.0`, per the strict policy in
//! `docs/design/value-flow-versioning-policy.md`. Pins only what a compile
//! cannot see: a dropped or renamed item already fails the bindgen struct
//! literals in `videre-sdk` and `videre-host`, and a stale version already
//! fails the versioned `import` lines, but case order, an added case, and a
//! retyped field all lower into ABI the compiler accepts.

use std::path::Path;

fn wit_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit/videre-value-flow/types.wit");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// Declaration lines inside the `header { .. }` block, verbatim less the
/// trailing comma, doc lines skipped.
fn block_lines(source: &str, header: &str) -> Vec<String> {
    let start = source
        .find(header)
        .unwrap_or_else(|| panic!("`{header}` not found in types.wit"));
    let body = &source[start + header.len()..];
    let end = body.find('}').expect("unterminated block in types.wit");
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("///"))
        .map(|line| line.trim_end_matches(',').trim_end().to_owned())
        .collect()
}

/// A tripwire, not a guard: the versioned imports already fail the build on
/// a stale reference, so this forces a legal 1.x minor bump to walk the
/// ripple list rather than move the pin by reflex.
#[test]
fn package_version_is_frozen_at_1_0_0() {
    assert!(
        wit_source().contains("package videre:value-flow@1.0.0;"),
        "value-flow left 1.0.0; walk the ripple list in \
         docs/design/value-flow-versioning-policy.md before moving this pin"
    );
}

#[test]
fn asset_variant_cases_are_closed() {
    let cases = block_lines(&wit_source(), "variant asset {");
    assert_eq!(
        cases,
        ["native", "erc20(erc20)", "service(service-desc)"],
        "the asset case set is closed and ordered at 1.0; a new case, a \
         reorder, or a retyped payload is 2.0 \
         (docs/design/value-flow-versioning-policy.md)"
    );
}

#[test]
fn frozen_record_fields_keep_their_declarations() {
    let source = wit_source();
    for (header, frozen) in [
        ("record erc20 {", &["token: address"][..]),
        ("record service-desc {", &["description: string"]),
        ("record asset-amount {", &["asset: asset", "amount: uint"]),
    ] {
        let lines = block_lines(&source, header);
        for decl in frozen {
            assert!(
                lines.iter().any(|line| line == decl),
                "`{header}` lost frozen declaration `{decl}`; a 1.x record \
                 grows additively and never retypes an existing field"
            );
        }
    }
}
