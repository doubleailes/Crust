//! One openusd in the build, by construction: no crate of the workspace
//! depends on `openusd` or `openusd-schemas` itself. Every crate reaches them
//! through Moho's re-exports (`moho::openusd`, `moho::openusd_schemas`), so
//! the stage Moho's scene index reads and the stage crust's code handles are
//! the same type, at the rev Moho pins.

use std::path::{Path, PathBuf};

/// The workspace's member manifests and the root one.
fn manifests() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/") {
        let manifest = entry.expect("an entry").path().join("Cargo.toml");
        if manifest.exists() {
            out.push(manifest);
        }
    }
    out
}

#[test]
fn no_crate_depends_on_openusd_directly() {
    let mut offenders = Vec::new();
    for manifest in manifests() {
        let text = std::fs::read_to_string(&manifest).expect("a readable manifest");
        for (n, line) in text.lines().enumerate() {
            let line = line.trim_start();
            let key = line.split(['=', '.', ' ']).next().unwrap_or("");
            if matches!(key, "openusd" | "openusd-schemas") {
                offenders.push(format!("{}:{}: {line}", manifest.display(), n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "depend on openusd through moho (`moho::openusd`), not directly:\n{}",
        offenders.join("\n")
    );
}

/// Moho's re-exports are what crust names: a stage built through them is
/// the stage the scene index takes.
#[test]
fn moho_reexports_the_stage_the_index_reads() {
    let registry = moho::openusd_schemas::schema_registry();
    let stage = moho::openusd::usd::Stage::builder()
        .schema_registry(registry)
        .open("../../samples/cornellbox.usda")
        .expect("the Cornell box opens");
    let index = moho::stage::StageSceneIndex::new();
    index.set_stage(stage);
    assert!(index.stage().is_some());
}
