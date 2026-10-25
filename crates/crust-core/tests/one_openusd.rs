//! One openusd in the build: the stage Moho's scene index reads and the stage
//! crust's direct import reads are the same type.
//!
//! Cargo identifies a git source by URL plus reference, so a patch to openusd's
//! `branch = "main"` and Moho's `rev = "…"` are two packages even at one commit.
//! The workspace patches openusd by the rev Moho pins (`[patch.crates-io]` in
//! the root `Cargo.toml`); if the two ever diverge, this file stops compiling,
//! because `moho::openusd` and `openusd` become different crates.
//!
//! Checked once when the guard was added: with the patch set back to
//! `branch = "main"`, the build carried two openusd packages and this file
//! failed with "mismatched types" (expected `openusd::usd::Stage`, found
//! `moho::openusd::usd::Stage`) — see the `moho-import` change, task 1.3.

use std::sync::Arc;

/// Takes the workspace's openusd stage.
fn workspace_stage(stage: openusd::usd::Stage) -> openusd::usd::Stage {
    stage
}

/// Takes the workspace's openusd-schemas registry.
fn workspace_registry(
    registry: Arc<openusd::usd::SchemaRegistry>,
) -> Arc<openusd::usd::SchemaRegistry> {
    registry
}

#[test]
fn moho_and_crust_share_one_openusd() {
    // A stage built through Moho's re-export, handed where crust's is expected.
    let registry: Arc<moho::openusd::usd::SchemaRegistry> =
        moho::openusd_schemas::schema_registry();
    let registry = workspace_registry(registry);
    let stage: moho::openusd::usd::Stage = moho::openusd::usd::Stage::builder()
        .schema_registry(registry)
        .open("../../samples/cornellbox.usda")
        .expect("the Cornell box opens");
    let stage = workspace_stage(stage);
    assert!(stage.prim("/").is_ok());
}

#[test]
fn moho_and_crust_share_one_openusd_schemas() {
    // A typed schema view from Moho's openusd-schemas, where crust's is expected.
    let as_crust: fn(
        Option<moho::openusd_schemas::geom::Mesh>,
    ) -> Option<openusd_schemas::geom::Mesh> = |mesh| mesh;
    assert!(as_crust(None).is_none());
}
