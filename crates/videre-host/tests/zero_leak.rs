//! Zero-leak oracle: the generic runtime this host links reaches no
//! venue-shaped crate.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root: the nearest ancestor whose `Cargo.toml` declares a
/// `[workspace]`. Nearest, not topmost, so a checkout nested under another
/// one (a git worktree, say) resolves to its own root.
fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|dir| {
            std::fs::read_to_string(dir.join("Cargo.toml"))
                .is_ok_and(|toml| toml.contains("[workspace]"))
        })
        .unwrap_or(manifest)
        .to_path_buf()
}

/// The graph oracle: `cargo tree` for the host crate (normal + build
/// edges) names no videre, intent, venue, or cow crate.
///
/// After the carve, `nexum-runtime` is a git dependency rather than a
/// local workspace member, so `--all-features` cannot be requested for it
/// (`cargo` rejects feature selection for packages outside the workspace).
/// The subtree is instead rendered with the feature set the workspace
/// already resolves for it (which includes the `test-utils` feature
/// `videre-host` activates), keeping the invariant meaningful: the generic
/// runtime, as this host actually links it, reaches no venue-shaped crate.
#[test]
fn host_crate_graph_reaches_no_venue_shaped_crate() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "nexum-runtime",
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--locked",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("cargo tree runs");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    let reached: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            let name = name.to_lowercase();
            ["videre", "intent", "venue", "cow"]
                .iter()
                .any(|word| name.contains(word))
        })
        .collect();
    assert!(
        reached.is_empty(),
        "venue-shaped crates reached: {reached:?}"
    );
}
