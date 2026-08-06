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

/// Path to a module's `.wasm` artefact under the workspace target dir.
/// A missing artefact is a hard failure under CI (the gate may not skip
/// itself) and a soft skip locally.
pub fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    wasm_or_skip(module_name, std::env::var_os("CI").is_some())
}

fn wasm_or_skip(module_name: &str, ci: bool) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.exists() {
        return Some(p);
    }
    assert!(
        !ci,
        "{} must be prebuilt in CI - run `just build-modules`",
        p.display()
    );
    eprintln!("SKIP: {} not found - run `just build-modules`", p.display());
    None
}

#[cfg(test)]
mod tests {
    use super::wasm_or_skip;

    #[test]
    #[should_panic(expected = "must be prebuilt in CI")]
    fn missing_wasm_hard_fails_under_ci() {
        wasm_or_skip("no-such-module", true);
    }

    #[test]
    fn missing_wasm_skips_locally() {
        assert!(wasm_or_skip("no-such-module", false).is_none());
    }
}
