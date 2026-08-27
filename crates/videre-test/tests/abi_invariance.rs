//! Oracle for the component-model invariance rule.
//!
//! # What this pins
//!
//! The WebAssembly Component Model relaxes subtyping only for `module`,
//! `instance` and `component` types. The data types are invariant: a
//! `record`, a `variant`, an `enum` and a `flags` must match structurally,
//! field for field and case for case. There is no width subtyping on a
//! record, so a consumer cannot "just ignore" a new field.
//!
//! This test builds that rule into the gate. It extracts the embedded WIT
//! from a real component, applies one mutation at a time to a copy of that
//! WIT, and asks `wasm-tools component targets` whether the unchanged
//! component still targets the mutated world.
//!
//! | Mutation                              | Expected |
//! |---------------------------------------|----------|
//! | baseline, unmodified                  | targets  |
//! | add a field to a record               | rejected |
//! | add a case to a variant               | rejected |
//! | rename a record field                 | rejected |
//! | add a function to an imported interface | targets  |
//! | add a function the world must export  | rejected |
//!
//! This test re-extracts its baseline from the component on every run, so
//! it is self-referential by design. It pins the rule, not the current
//! shape of the packages under `wit/`. A field added to `wit/` moves the
//! component and its embedded WIT together, and this test stays green. The
//! rule is what must not be forgotten.
//!
//! # Why it matters
//!
//! A written versioning policy once claimed that "a new record field lowers
//! into the canonical ABI without disturbing the existing fields, so record
//! growth is safe for a consumer that ignores it". That claim is false, and
//! the table above is the counter-evidence.
//!
//! The rule this test defends: every data-shape change to a shared WIT
//! package is a major version bump. Adding a field, adding a variant case,
//! adding an enum case, adding a flag and renaming anything all break every
//! component already built against the old shape. The table exercises the
//! record, the variant and the rename; the enum and the flags follow the
//! same invariance rule.
//!
//! The only safe additive move is to add a function, and only host-first.
//! A component tolerates an import it does not use, so the host may offer a
//! new function before any guest calls it. The reverse never holds. The
//! last row proves it: a world that demands one more export rejects the
//! component at once, so a guest can never lead the host.
//!
//! # Honest direction
//!
//! A conformance test that passes because every case fails proves nothing.
//! The imported-function row is the control: it must report `targets`. If
//! the harness breaks, that row turns red, so a silently broken harness
//! cannot masquerade as a green gate.
//!
//! Each rejected row also asserts on the reason `wasm-tools` prints. A
//! nonzero exit alone is weak evidence, because a mutation that produced
//! unparseable WIT would also exit nonzero and would prove nothing about
//! the ABI. Each mutation anchor must further match the embedded WIT
//! exactly once, so a stale anchor fails loudly rather than degrading into
//! a no-op that trivially passes.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path under this workspace root.
///
/// The root is the nearest ancestor whose `Cargo.toml` declares a
/// `[workspace]`. Nearest, not topmost: a git worktree of this repo nests
/// under the main checkout, and the topmost match would resolve to the
/// main checkout's `target/` rather than the worktree's own.
fn workspace_path(relative: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|d| {
            std::fs::read_to_string(d.join("Cargo.toml"))
                .is_ok_and(|t| t.lines().any(|l| l.trim_start().starts_with("[workspace]")))
        })
        .unwrap_or(manifest)
        .join(relative)
}

/// Path to a module's `.wasm` artefact under the workspace target dir.
///
/// A missing artefact is a hard failure under CI (the gate may not skip
/// itself) and a soft skip locally.
fn module_wasm_or_skip(module_name: &str) -> Option<PathBuf> {
    let artifact = module_name.replace('-', "_");
    let p = workspace_path(&format!("target/wasm32-wasip2/release/{artifact}.wasm"));
    if p.exists() {
        return Some(p);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{} must be prebuilt in CI",
        p.display()
    );
    eprintln!(
        "SKIP: {} not found - build with `cargo build -p {module_name} --target wasm32-wasip2 --release`",
        p.display()
    );
    None
}

/// True when `wasm-tools` is on `PATH` and runnable.
///
/// A missing tool is a hard failure under CI and a soft skip locally, the
/// same way a missing module wasm is. The CI workflow installs the tool for
/// this reason; a gate that skips itself is not a gate.
fn wasm_tools_present() -> bool {
    match Command::new("wasm-tools").arg("--version").output() {
        Ok(out) if out.status.success() => true,
        _ => {
            assert!(
                std::env::var_os("CI").is_none(),
                "wasm-tools must be installed in CI"
            );
            eprintln!(
                "SKIP: wasm-tools not found on PATH - enter the dev shell with `nix develop`"
            );
            false
        }
    }
}

/// The component's own embedded WIT, as printed by `wasm-tools component wit`.
fn embedded_wit(component: &Path) -> String {
    let out = Command::new("wasm-tools")
        .arg("component")
        .arg("wit")
        .arg(component)
        .output()
        .expect("run `wasm-tools component wit`");
    assert!(
        out.status.success(),
        "`wasm-tools component wit` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("embedded WIT is utf-8")
}

/// Outcome of a `wasm-tools component targets` check.
enum Outcome {
    /// The component targets the world.
    Targets,
    /// The component does not target the world, with the tool's reason.
    Rejected(String),
}

impl Outcome {
    /// `"targets"` or `"rejected"`, for the report table.
    fn label(&self) -> &'static str {
        match self {
            Self::Targets => "targets",
            Self::Rejected(_) => "rejected",
        }
    }
}

/// What a row expects of `wasm-tools component targets`.
enum Expect {
    /// The component still targets the mutated world.
    Targets,
    /// The tool rejects the component, and says so for this reason. The
    /// substring keeps a rejected row honest: a parse error would also exit
    /// nonzero, and it would prove nothing about the ABI.
    RejectedBecause(&'static str),
}

/// Write `wit` into a fresh directory and ask whether `component` targets
/// the `root` world it declares.
fn targets(dir: &Path, wit: &str, component: &Path) -> Outcome {
    std::fs::create_dir_all(dir).expect("create the mutated WIT directory");
    std::fs::write(dir.join("root.wit"), wit).expect("write the mutated WIT");
    let out = Command::new("wasm-tools")
        .arg("component")
        .arg("targets")
        .arg("-w")
        .arg("root")
        .arg(dir)
        .arg(component)
        .output()
        .expect("run `wasm-tools component targets`");
    if out.status.success() {
        Outcome::Targets
    } else {
        let mut why = String::from_utf8_lossy(&out.stderr).into_owned();
        why.push_str(&String::from_utf8_lossy(&out.stdout));
        Outcome::Rejected(why.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

/// Replace the single occurrence of `anchor` in `wit` with `replacement`.
///
/// The uniqueness assertion is load-bearing. A mutation that silently
/// matched nothing would leave the WIT unchanged, and its row would then
/// pass for the wrong reason.
fn mutate(wit: &str, anchor: &str, replacement: &str) -> String {
    assert_eq!(
        wit.matches(anchor).count(),
        1,
        "mutation anchor {anchor:?} must appear exactly once in the embedded WIT"
    );
    wit.replace(anchor, replacement)
}

/// Records, variants and field names are invariant in the component ABI;
/// only a new import-side function is additive.
#[test]
fn shared_wit_data_shapes_are_invariant_and_only_imported_functions_are_additive() {
    let Some(component) = module_wasm_or_skip("echo-client") else {
        return;
    };
    if !wasm_tools_present() {
        return;
    }

    let base = embedded_wit(&component);
    let scratch = tempfile::tempdir().expect("create the scratch directory");

    // The baseline must target its own world. If it does not, every other
    // row below is meaningless, so fail loudly here.
    if let Outcome::Rejected(why) = targets(&scratch.path().join("baseline"), &base, &component) {
        panic!("the unmodified component must target its own embedded WIT: {why}");
    }

    // Each row: a name, the mutated WIT, and what the tool must say.
    let cases: Vec<(&str, String, Expect)> = vec![
        (
            "add a field to a record",
            mutate(
                &base,
                "record quotation {\n",
                "record quotation {\n      firm: option<u64>,\n",
            ),
            Expect::RejectedBecause("type mismatch"),
        ),
        (
            "add a case to a variant",
            mutate(
                &base,
                "variant venue-error {\n",
                "variant venue-error {\n      settlement-stalled,\n",
            ),
            Expect::RejectedBecause("type mismatch"),
        ),
        (
            "rename a record field",
            mutate(&base, "valid-until-ms: u64,", "valid-until-millis: u64,"),
            Expect::RejectedBecause("type mismatch"),
        ),
        (
            "add a function to an imported interface",
            mutate(
                &base,
                "record quotation {",
                "abi-probe: func() -> u32;\n\n    record quotation {",
            ),
            Expect::Targets,
        ),
        (
            "add a function the world must export",
            mutate(
                &base,
                "world root {\n",
                "world root {\n  export abi-probe-export: func() -> u32;\n",
            ),
            Expect::RejectedBecause("missing export named `abi-probe-export`"),
        ),
    ];

    let mut report = String::from("\nABI invariance matrix (echo-client, wasm-tools):\n");
    report.push_str("  baseline, unmodified                     targets\n");
    let mut failures = Vec::new();

    for (name, wit, expect) in &cases {
        let dir = scratch.path().join(name.replace(' ', "-"));
        let got = targets(&dir, wit, &component);
        report.push_str(&format!("  {name:<40} {}\n", got.label()));
        match (expect, &got) {
            (Expect::Targets, Outcome::Targets) => {}
            (Expect::RejectedBecause(reason), Outcome::Rejected(why)) if why.contains(reason) => {}
            (Expect::Targets, Outcome::Rejected(why)) => {
                failures.push(format!("  {name}: expected targets, got rejected: {why}"));
            }
            (Expect::RejectedBecause(reason), Outcome::Targets) => {
                failures.push(format!(
                    "  {name}: expected rejected ({reason}), but the mutated world was accepted"
                ));
            }
            (Expect::RejectedBecause(reason), Outcome::Rejected(why)) => {
                failures.push(format!(
                    "  {name}: rejected, but not for the pinned reason ({reason}): {why}"
                ));
            }
        }
    }

    eprintln!("{report}");
    assert!(
        failures.is_empty(),
        "the component ABI did not behave as pinned:\n{}\n{report}",
        failures.join("\n")
    );
}
