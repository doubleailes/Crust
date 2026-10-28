//! Hydra's instancers, as the instancing reader finds them in the scene
//! index: a native instance's prototype (`instance` → its instancer's
//! `instancerTopology`, `.../UsdNiInstancer/UsdNiPrototype`), and a point
//! instancer's prototype copies (`ForInstancer<hash>`, in the order of its
//! `prototypes` targets), as UsdImaging's prototype propagation names them.
//! A prim of a prototype carries `instancedBy`: it is placed only through its
//! instancer.

use moho::openusd::sdf;
use moho::scene_index::SceneIndex;
use moho::tokens::instancing as t;

use super::data::PrimData;

fn paths(value: Option<sdf::Value>) -> Vec<sdf::Path> {
    match value {
        Some(sdf::Value::PathVec(v)) => v,
        _ => Vec::new(),
    }
}

/// The prototype root the native instance at `path` places: its
/// instancer's `instancerTopology/prototypes` entry at its
/// `prototypeIndex` —
/// `<root>/UsdNiPropagatedPrototypes/<bindings>/__Prototype_N/UsdNiInstancer/UsdNiPrototype`.
pub(in crate::scene::usd_import) fn native_prototype(
    inst: &dyn SceneIndex,
    path: &sdf::Path,
) -> Option<sdf::Path> {
    let prim = PrimData::at(inst, path);
    let instancer = paths(
        prim.locate([t::INSTANCE, t::INSTANCER])
            .and_then(|d| d.value()),
    )
    .pop()?;
    let index = match prim
        .locate([t::INSTANCE, t::PROTOTYPE_INDEX])
        .and_then(|d| d.value())
    {
        Some(sdf::Value::Int(i)) => usize::try_from(i).ok()?,
        _ => 0,
    };
    instancer_prototypes(inst, &instancer)
        .into_iter()
        .nth(index)
}

/// The prototypes the instancer at `path` places, as its
/// `instancerTopology/prototypes` names them: a point instancer's
/// `ForInstancer` copies, in its `prototypes` targets' order.
pub(in crate::scene::usd_import) fn instancer_prototypes(
    inst: &dyn SceneIndex,
    path: &sdf::Path,
) -> Vec<sdf::Path> {
    paths(
        PrimData::at(inst, path)
            .locate([t::INSTANCER_TOPOLOGY, t::PROTOTYPES])
            .and_then(|d| d.value()),
    )
}

/// The instancers that place the prim at `path` (`instancedBy/paths`): empty
/// for a prim no instancer instances.
pub(in crate::scene::usd_import) fn instanced_by(
    index: &dyn SceneIndex,
    path: &sdf::Path,
) -> Vec<sdf::Path> {
    paths(
        PrimData::at(index, path)
            .locate([t::INSTANCED_BY, t::PATHS])
            .and_then(|d| d.value()),
    )
}
