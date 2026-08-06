//! Shared path fixtures for the videre-host integration tests.

use std::path::{Path, PathBuf};

/// Path under the workspace root (the topmost ancestor with a `Cargo.toml`).
pub fn workspace_path(relative: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .filter(|d| d.join("Cargo.toml").is_file())
        .last()
        .unwrap_or(manifest)
        .join(relative)
}

/// Opt-in escape hatch for a run with no guest wasms built.
const SKIP_VAR: &str = "VIDERE_SKIP_MISSING_WASMS";

/// Path to a module's `.wasm` artefact under the workspace target dir.
/// A missing artefact fails the test: skipping is what let a run with zero
/// wasms built report all green, and a runner captures the skip message of a
/// passing test. `VIDERE_SKIP_MISSING_WASMS` opts back into the skip, except
/// under `CI` where the gate may not excuse itself.
pub fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    wasm_or_skip(
        module_name,
        std::env::var_os(SKIP_VAR).is_some(),
        std::env::var_os("CI").is_some(),
    )
}

fn wasm_or_skip(module_name: &str, skip_requested: bool, ci: bool) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.is_file() {
        return Some(p);
    }
    assert!(
        skip_requested && !ci,
        "{} is missing - run `just build-modules`",
        p.display()
    );
    eprintln!("SKIP: {} not found - run `just build-modules`", p.display());
    None
}

#[cfg(test)]
mod tests {
    use super::wasm_or_skip;

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
}
