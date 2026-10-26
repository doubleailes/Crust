//! The walk over the scene index (design D5): from the absolute root, over the
//! index's children, with the direct traversal's stack discipline (push in
//! order, pop last-first), because geometry ids, light-link runs, pass flags
//! and provenance depend on visit order, and bitwise image parity on geometry
//! order.
//!
//! The children come from one input's chain — the single stage's, or the
//! chunk's own masked stage while streaming — never from the merging index
//! above a chunk root, whose children are not the stage's namespace order
//! (Moho's merged order: inputs strongest first). Pruning reads the index: an
//! abstract or inactive prim is absent from it, and `purpose` and `visibility`
//! are each prim's own resolved values from the generic attribute data, pruning
//! the subtree where a non-render purpose or `invisible` is authored, as the
//! direct importer did. (Hydra's flattened purpose lets a child's authored
//! purpose override an ancestor's `proxy`; Crust's rule prunes the subtree, so
//! the flattened value would change what renders.)

use glam::Mat4 as GMat4;
use moho::data_source::locate;
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::openusd::usd::Stage;
use moho::scene_index::SceneIndex;
use moho::tokens::usd as generic;

use super::super::xform::Timing;
use super::super::{ImportCtx, Prune, visit_prim};
use super::{Input, bridge};

/// Walks `input`'s chain into `ctx`, reading each prim's families through the
/// bridge on `stage`, the stage `input` was built over. `top` is the top of
/// the import's chain, over `input` (and, streamed, the other inputs): what
/// the material readers read.
pub(in crate::scene::usd_import) fn walk(
    stage: &Stage,
    input: &Input,
    top: &moho::scene_index::SceneIndexHandle,
    ctx: &mut ImportCtx,
) {
    let index: &dyn SceneIndex = input.chain.as_ref();
    if let Some(pass) = &mut ctx.pass {
        pass.begin_chunk(input);
    }
    ctx.caches.scene = Some((input.chain.clone(), stage.clone()));
    ctx.caches.top = Some(top.clone());
    let mut stack: Vec<(sdf::Path, GMat4, bool, Timing)> = vec![(
        sdf::Path::abs_root(),
        GMat4::IDENTITY,
        false,
        Timing::Static,
    )];
    while let Some((path, parent_world, parent_hidden, parent_timing)) = stack.pop() {
        let prim = bridge::prim(stage, &path);
        let pruned = prune(index, &path);
        let Some((world, hidden, timing)) = visit_prim(
            stage,
            index,
            &prim,
            pruned,
            parent_world,
            parent_hidden,
            &parent_timing,
            ctx,
        ) else {
            continue;
        };
        for child in children(stage, index, &path) {
            stack.push((child, world, hidden, timing.clone()));
        }
    }
    ctx.caches.scene = None;
    ctx.caches.top = None;
}

/// The children the walk descends into: the index's, less the prototypes
/// the stage index lists under the absolute root (as UsdImaging does), which
/// the direct traversal never reached from the root — an instance takes its
/// prototype's geometry through the instancing reader.
pub(in crate::scene::usd_import) fn children(
    stage: &Stage,
    index: &dyn SceneIndex,
    path: &sdf::Path,
) -> Vec<sdf::Path> {
    let mut children = index.children(path);
    if path.is_abs_root() {
        children.retain(|c| !bridge::prim(stage, c).is_prototype().unwrap_or(false));
    }
    children
}

/// Why the walk leaves `path` and its subtree out, from the index: its own
/// `purpose` (`proxy`, `guide`) and `visibility` (`invisible`).
pub(in crate::scene::usd_import) fn prune(
    index: &dyn SceneIndex,
    path: &sdf::Path,
) -> Option<Prune> {
    if path.is_abs_root() {
        return None;
    }
    let data = index.prim(path).data?;
    let token = |name: &str| {
        let value = locate(
            &data,
            &Locator::new([generic::ATTRIBUTES, name, generic::VALUE]),
        )?
        .value()?;
        match value {
            moho::openusd::sdf::Value::Token(t) => Some(t.as_str().to_owned()),
            moho::openusd::sdf::Value::String(s) => Some(s),
            _ => None,
        }
    };
    match token("purpose").as_deref() {
        Some("proxy") => return Some(Prune::Purpose("proxy")),
        Some("guide") => return Some(Prune::Purpose("guide")),
        _ => {}
    }
    (token("visibility").as_deref() == Some("invisible")).then_some(Prune::Invisible)
}
