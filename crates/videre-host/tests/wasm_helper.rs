//! Unit coverage for the hardened wasm lookup in `common`, held to one
//! binary so the cases do not multiply across every suite that includes
//! the shared module.

mod common;

use common::wasm_or_skip;

#[test]
#[should_panic(expected = "run `just build-modules`")]
fn missing_wasm_hard_fails_by_default() {
    wasm_or_skip("no-such-module", false, false);
}

#[test]
fn missing_wasm_skips_when_opted_in() {
    assert!(wasm_or_skip("no-such-module", true, false).is_none());
}

#[test]
#[should_panic(expected = "run `just build-modules`")]
fn ci_overrides_the_skip_opt_in() {
    wasm_or_skip("no-such-module", true, true);
}
