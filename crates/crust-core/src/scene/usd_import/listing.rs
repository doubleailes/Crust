//! Reading what a stage holds without importing it (`crust ls`): its cameras,
//! lights, materials or render passes.
//!
//! The listing reads the batch host's scene index — the chain a render
//! builds over the stage, through the delegate's renderer filters, at `-f` or
//! else the index's time code 0 — by Hydra prim type, in namespace order (a
//! depth-first walk of the index's children). So what is listed for a kind
//! is what the render would use of that kind: a prim USD removes (an inactive
//! or abstract one) is not in the index, and a prototype's prims are placed
//! through their instancer. Nothing but the listed prims' data is read: no
//! geometry is built, no material resolved, no texture or emission decoded —
//! so a light the import refuses for its evaluated values (a zero radius, a
//! transform that collapses it) is still listed; see [`ListKind::Light`].

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use moho::openusd::sdf;
use moho::scene_index::SceneIndex;
use moho::tokens::{prim_type as t, usd_prim_info as info};
use tracing::debug;

use super::{open_stage, release_stage, standard_chain};
use crate::scene::hydra::Prune;
use crate::scene::hydra::camera::camera_lens;
use crate::scene::hydra::commit::prune;
use crate::scene::hydra::delegate::HydraScene;
use crate::scene::hydra::first_camera;
use crate::scene::hydra::index::data::PrimData;
use crate::scene::hydra::index::instancers::instanced_by;
use crate::scene::hydra::index::network::{networks, resolve};
use crate::scene::hydra::lights::light_inputs;
use crate::scene::hydra::materials::{SURFACE_RENDER_CONTEXTS, index_bound_material};
use crate::scene::hydra::products::import_render_products;
use crate::scene::hydra::settings::{
    CameraPick, import_render_settings, pick_camera, render_source, wanted_camera,
};
use crate::scene::{CameraRecord, LightRecord, ListKind, ListRecord, MaterialRecord, PassRecord};

/// Every `kind` prim on the stage at `path`, in namespace order, as absolute
/// prim paths.
pub(crate) fn list_prims(path: &Path, kind: ListKind) -> Result<Vec<String>, crate::Error> {
    let started = Instant::now();
    let prims = with_index(path, None, |index| {
        let mut out = Vec::new();
        walk(index, |p, _, ty| {
            if is_kind(ty, kind) && listed(index, p, kind) {
                out.push(p.to_string());
            }
        });
        out
    })?;
    debug!(
        "Found {} {kind:?} prim(s) on {} in {:?}",
        prims.len(),
        path.display(),
        started.elapsed()
    );
    Ok(prims)
}

/// [`list_prims`]' prims with the values a render reads for each, at time
/// code `frame` — see [`crate::Scene::list_usd_records`].
pub(crate) fn list_records(
    path: &Path,
    kind: ListKind,
    frame: Option<f64>,
) -> Result<Vec<ListRecord>, crate::Error> {
    if let Some(t) = frame
        && !t.is_finite()
    {
        return Err(crate::Error::InvalidFrame(t));
    }
    let started = Instant::now();
    let records = with_index(path, frame, |index| {
        // What the import reads before it meets a camera: the settings (the
        // image's aspect, which the aperture is conformed to) and the camera
        // it was told to use.
        let products = import_render_products(index, None, None);
        let mut settings = import_render_settings(index, None);
        if let Some((w, h)) = products.resolution {
            settings = settings.with_resolution(w, h);
        }
        let framing = products.framing;
        let wanted = wanted_camera(None, products.camera, index, None);

        let mut records: Vec<ListRecord> = Vec::new();
        let bound = if kind == ListKind::Material {
            bound_materials(index)
        } else {
            HashSet::new()
        };
        walk(index, |p, hidden, ty| {
            if !is_kind(ty, kind) || !listed(index, p, kind) {
                return;
            }
            let path = p.to_string();
            let record = match kind {
                ListKind::Camera => camera_lens(index, p, &settings, framing).map(|lens| {
                    ListRecord::Camera(CameraRecord {
                        path,
                        focal_length_mm: lens.focal_length,
                        aperture_mm: lens.aperture,
                        conformed_aperture_mm: lens.conformed_aperture,
                        aperture_offset_mm: lens.aperture_offset,
                        f_stop: lens.f_stop,
                        focus_distance: lens.focus_distance,
                        is_render_camera: false,
                        hidden,
                    })
                }),
                ListKind::Light => light_kind(ty).map(|kind| {
                    let inputs = light_inputs(&PrimData::at(index, p));
                    ListRecord::Light(LightRecord {
                        path,
                        kind,
                        intensity: inputs.intensity,
                        exposure: inputs.exposure,
                        color: inputs.color.to_array(),
                        normalize: inputs.normalize,
                    })
                }),
                ListKind::Material => Some(ListRecord::Material(MaterialRecord {
                    surface: surface_shader_id(&PrimData::at(index, p)),
                    path,
                    bound: false,
                })),
                ListKind::Pass => Some(ListRecord::Pass(PassRecord {
                    render_source: render_source(index, p).map(|t| t.to_string()),
                    path,
                })),
            };
            records.extend(record);
        });
        match kind {
            ListKind::Camera => {
                let wanted_path = wanted.as_ref().map(|c| c.path().to_string());
                let wanted_met = wanted_path
                    .as_ref()
                    .is_some_and(|w| records.iter().any(|r| r.path() == w));
                let render_camera =
                    match pick_camera(wanted.as_ref(), wanted_met, !records.is_empty()) {
                        CameraPick::Wanted => wanted_path,
                        CameraPick::First => first_camera(index).map(|p| p.to_string()),
                        CameraPick::Procedural | CameraPick::Missing(_) => None,
                    };
                for r in &mut records {
                    if let ListRecord::Camera(c) = r {
                        c.is_render_camera = render_camera.as_deref() == Some(c.path.as_str());
                    }
                }
            }
            ListKind::Material => {
                for r in &mut records {
                    if let ListRecord::Material(m) = r {
                        m.bound = bound.contains(&m.path);
                    }
                }
            }
            ListKind::Light | ListKind::Pass => {}
        }
        records
    })?;
    debug!(
        "Read {} {kind:?} record(s) on {} in {:?}",
        records.len(),
        path.display(),
        started.elapsed()
    );
    Ok(records)
}

/// `read` over the batch host's index of the stage at `path`, opened with
/// its payloads (a shot camera or a set's lights under a payload is the
/// production case), at `frame`.
fn with_index<T>(
    path: &Path,
    frame: Option<f64>,
    read: impl FnOnce(&dyn SceneIndex) -> T,
) -> Result<T, crate::Error> {
    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;
    let stage = open_stage(path, path_str)?;
    let delegate = HydraScene::new(standard_chain(&stage, frame));
    delegate.update();
    let out = read(delegate.index().as_ref());
    drop(delegate);
    release_stage(stage, false);
    Ok(out)
}

/// `visit(path, hidden, type)` for each prim of a depth-first walk of
/// `index`'s children (namespace order), `hidden` under an invisible
/// ancestor. The walk does not enter a native instance (its prototype is
/// placed through it), nor a prototype's prims (`instancedBy`), nor the
/// children of a point instancer.
fn walk(index: &dyn SceneIndex, mut visit: impl FnMut(&sdf::Path, bool, &str)) {
    let mut stack = vec![(sdf::Path::abs_root(), false)];
    while let Some((path, parent_hidden)) = stack.pop() {
        if !path.is_abs_root() && !instanced_by(index, &path).is_empty() {
            continue;
        }
        let prim = index.prim(&path);
        let ty = prim.ty.as_str().to_owned();
        let hidden = parent_hidden || prune(index, &path) == Some(Prune::Invisible);
        visit(&path, hidden, &ty);
        let data = PrimData::at(index, &path);
        let instance = data
            .locate([info::USD_PRIM_INFO, info::NI_PROTOTYPE_PATH])
            .is_some();
        if instance || ty == t::INSTANCER {
            continue;
        }
        let children = index.children(&path);
        stack.extend(children.into_iter().rev().map(|c| (c, hidden)));
    }
}

/// The materials the geometry a render draws is bound to: every geometry
/// prim of the index, a prototype's included (it renders through its
/// instances), not pruned and not a volume region (which takes no
/// material), by its resolved `materialBindings`.
fn bound_materials(index: &dyn SceneIndex) -> HashSet<String> {
    let mut bound = HashSet::new();
    let mut stack = vec![sdf::Path::abs_root()];
    while let Some(path) = stack.pop() {
        if prune(index, &path) == Some(Prune::Invisible) {
            continue;
        }
        let ty = index.prim(&path).ty;
        if is_geometry(ty.as_str()) && prune(index, &path).is_none() {
            let data = PrimData::at(index, &path);
            if crate::scene::hydra::volume::volume_type(&data).is_none()
                && let Some(m) = index_bound_material(&data)
            {
                bound.insert(m.to_string());
            }
        }
        stack.extend(index.children(&path));
    }
    bound
}

/// Whether a prim of Hydra type `ty` is of `kind`.
fn is_kind(ty: &str, kind: ListKind) -> bool {
    match kind {
        ListKind::Camera => ty == t::CAMERA,
        ListKind::Light => light_kind(ty).is_some(),
        ListKind::Material => ty == t::MATERIAL,
        ListKind::Pass => ty == t::RENDER_PASS,
    }
}

/// Whether the walk lists the `kind` prim at `path`:
///
/// - **cameras** — every one a render can go through: none under a purpose a
///   render does not draw, an invisible one included.
/// - **lights** — pruned as geometry is, invisibility included: an invisible
///   light lights nothing.
/// - **materials**, **passes** — every one.
fn listed(index: &dyn SceneIndex, path: &sdf::Path, kind: ListKind) -> bool {
    let pruned = prune(index, path);
    match kind {
        ListKind::Camera => pruned.is_none_or(|r| r == Prune::Invisible),
        ListKind::Light => pruned.is_none(),
        ListKind::Material | ListKind::Pass => true,
    }
}

/// The kind a light of Hydra type `ty` is listed as.
fn light_kind(ty: &str) -> Option<&'static str> {
    Some(match ty {
        t::SPHERE_LIGHT => "sphere",
        t::RECT_LIGHT => "rect",
        t::DISK_LIGHT => "disk",
        t::CYLINDER_LIGHT => "cylinder",
        t::DISTANT_LIGHT => "distant",
        t::DOME_LIGHT => "dome",
        _ => return None,
    })
}

/// Whether a prim of Hydra type `ty` is geometry a material binds to.
fn is_geometry(ty: &str) -> bool {
    matches!(
        ty,
        t::MESH
            | t::SPHERE
            | t::BASIS_CURVES
            | t::NURBS_CURVES
            | t::CUBE
            | t::CONE
            | t::CYLINDER
            | t::CAPSULE
            | t::PLANE
    )
}

/// The identifier of the surface shader the import decodes the material
/// from, in the import's order: a `PxrDisneyBsdf` node first, else the node
/// the surface terminal resolves to. `None` when there is none — a MaterialX
/// reference composes no shader. What `crust ls material --json` reports as
/// `surface`.
fn surface_shader_id(material: &PrimData) -> Option<String> {
    let networks = networks(material);
    if networks
        .get(moho::tokens::material::ALL)
        .is_some_and(|all| all.nodes.values().any(|n| n.id == "PxrDisneyBsdf"))
    {
        return Some("PxrDisneyBsdf".to_owned());
    }
    let (_, node) = resolve(&networks, "surface", SURFACE_RENDER_CONTEXTS)?;
    node.map(|(_, n)| n.id.clone()).filter(|id| !id.is_empty())
}
