//! The transitional bridge (design D6): the `usd::Prim` at a walked path, on
//! the stage of the input that holds it, for a schema family whose reader has
//! not moved to the scene index yet. It reads at the import's time through
//! `EvalTimeScope`, as the direct readers do. It goes, with the direct readers,
//! once every family reads the index.

use moho::openusd::sdf;
use moho::openusd::usd::{Prim, Stage};

/// The prim at `path` on `stage`, for the direct reader that still reads it.
pub(in crate::scene::usd_import) fn prim(stage: &Stage, path: &sdf::Path) -> Prim {
    super::super::prim_at(stage, path.clone())
}
