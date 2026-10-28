//! Reading what a stage holds without importing it (`crust ls`): its cameras,
//! lights, materials or render passes.
//!
//! The walk is the render's own — the same stage, the same
//! [`prune_reason`], the same prims it does not enter — so what is listed for
//! a kind is what the import would use of that kind, and nothing it would
//! never meet. Nothing but the listed schema is read: no geometry is built,
//! no material resolved, no texture or light emission decoded — so a light
//! the import refuses for its evaluated values (a zero radius, a transform
//! that collapses it) is still listed; see [`ListKind::Light`].

use moho::{openusd, openusd_schemas};
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use openusd::sdf;
use openusd::usd::{Attribute, CollectionAPI, Prim, Stage};
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, Camera as UsdCamera, Mesh as UsdMesh,
    NurbsCurves as UsdNurbsCurves, PointInstancer, PointInstancerSchema, Sphere as UsdSphere,
};
use openusd_schemas::geom::{Capsule, Capsule_1, Cone, Cube, Cylinder, Cylinder_1, Plane};
use openusd_schemas::lux::{
    CylinderLight, DiskLight, DistantLight, DomeLight, DomeLight_1, RectLight, SphereLight,
};
use openusd_schemas::render::Pass as UsdRenderPass;
use openusd_schemas::shade::{
    BindingStrength, CollectionBinding, Material as UsdMaterial, MaterialBindingAPI, Shader,
    TerminalSource,
};
use tracing::debug;

use super::attrs::decode_text;
use super::camera::camera_lens;
use super::index::data::PrimData;
use super::lights::light_inputs;
use super::materials::{RENDER_BINDING_PURPOSE, SURFACE_RENDER_CONTEXTS};
use super::products::{Framing, import_render_products};
use super::settings::{CameraPick, import_render_settings, pick_camera, wanted_camera};
use super::{Prune, open_stage, prim_at, release_stage};
use crate::scene::{CameraRecord, LightRecord, ListKind, ListRecord, MaterialRecord, PassRecord};
use crate::tracer::RenderSettings;

/// Every `kind` prim on the stage at `path`, in namespace order (a parent's
/// children in their authored order), as absolute prim paths.
pub(crate) fn list_prims(path: &Path, kind: ListKind) -> Result<Vec<String>, crate::Error> {
    let started = Instant::now();
    let path_str = utf8(path)?;
    let mut found = Found::default();
    with_stage_at(path, path_str, |stage| {
        collect(stage, kind, Order::Authored, None, |prim, _| {
            found.push(prim.path().to_string(), ());
        });
    })?;
    let prims = found.items.into_iter().map(|(p, ())| p).collect::<Vec<_>>();
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
    let path_str = utf8(path)?;
    let index = open_stage(path, path_str)?;
    // What the import reads before it meets a camera: the settings (the
    // image's aspect, which the aperture is conformed to) and the camera it
    // was told to use.
    // The settings the import would read, through the scene index as the
    // import reads them.
    let scene = super::index::input_chain(index.clone(), frame);
    let products = import_render_products(scene.chain.as_ref(), None, None);
    let mut settings = import_render_settings(scene.chain.as_ref(), None);
    if let Some((w, h)) = products.resolution {
        settings = settings.with_resolution(w, h);
    }
    let framing = products.framing;
    let wanted = wanted_camera(None, products.camera, scene.chain.as_ref(), None);
    drop(scene);

    let mut found: Found<ListRecord> = Found::default();
    // The first camera met in the import's own order, and the materials the
    // import's binding resolution reaches.
    let mut first_camera: Option<String> = None;
    let mut bound: HashSet<String> = HashSet::new();
    {
        let stage = &index;
        // A camera's lens is read as the import reads it, through the scene
        // index of the stage it is on.
        let scene = matches!(kind, ListKind::Camera | ListKind::Light)
            .then(|| super::index::input_chain(stage.clone(), frame));
        collect(stage, kind, Order::Authored, frame, |prim, hidden| {
            let scene = scene.as_ref().map(|s| s.chain.as_ref());
            if let Some(r) = record(stage, scene, prim, kind, hidden, &settings, framing, frame) {
                found.push(prim.path().to_string(), r);
            }
        });
        match kind {
            ListKind::Camera if first_camera.is_none() => {
                collect(stage, kind, Order::Import, frame, |prim, _| {
                    if first_camera.is_none() {
                        first_camera = Some(prim.path().to_string());
                    }
                });
            }
            ListKind::Material => bound_materials(stage, &mut bound, frame),
            _ => {}
        }
    }
    release_stage(index, false);
    let mut records: Vec<ListRecord> = found.items.into_iter().map(|(_, r)| r).collect();
    match kind {
        ListKind::Camera => {
            let wanted_path = wanted.as_ref().map(|c| c.path().to_string());
            let wanted_met = wanted_path
                .as_ref()
                .is_some_and(|w| records.iter().any(|r| r.path() == w));
            let render_camera = match pick_camera(wanted.as_ref(), wanted_met, !records.is_empty())
            {
                CameraPick::Wanted => wanted_path,
                CameraPick::First => first_camera,
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
    debug!(
        "Read {} {kind:?} record(s) on {} in {:?}",
        records.len(),
        path.display(),
        started.elapsed()
    );
    Ok(records)
}

fn utf8(path: &Path) -> Result<&str, crate::Error> {
    path.to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))
}

/// `visit` on the stage at `path`, opened with its payloads — a shot camera
/// or a set's lights under a payload is the production case.
fn with_stage_at(
    path: &Path,
    path_str: &str,
    visit: impl FnOnce(&Stage),
) -> Result<(), crate::Error> {
    let stage = open_stage(path, path_str)?;
    visit(&stage);
    release_stage(stage, false);
    Ok(())
}

/// One listed prim's record, its derived fields (`is_render_camera`,
/// `bound`) left for the caller.
#[allow(clippy::too_many_arguments)]
fn record(
    stage: &Stage,
    scene: Option<&dyn moho::scene_index::SceneIndex>,
    prim: &Prim,
    kind: ListKind,
    hidden: bool,
    settings: &RenderSettings,
    framing: Framing,
    frame: Option<f64>,
) -> Option<ListRecord> {
    let path = prim.path().to_string();
    Some(match kind {
        ListKind::Camera => {
            let lens = camera_lens(scene?, prim.path(), settings, framing)?;
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
        }
        ListKind::Light => {
            // Read as the import reads it, through the scene index.
            let data = PrimData::at(scene?, prim.path());
            let kind = [
                ("SphereLight", "sphere"),
                ("RectLight", "rect"),
                ("DiskLight", "disk"),
                ("CylinderLight", "cylinder"),
                ("DistantLight", "distant"),
                ("DomeLight", "dome"),
                ("DomeLight_1", "dome"),
            ]
            .into_iter()
            .find(|(schema, _)| data.is_a(schema))
            .map(|(_, kind)| kind)?;
            let inputs = light_inputs(&data);
            ListRecord::Light(LightRecord {
                path,
                kind,
                intensity: inputs.intensity,
                exposure: inputs.exposure,
                color: inputs.color.to_array(),
                normalize: inputs.normalize,
            })
        }
        ListKind::Material => ListRecord::Material(MaterialRecord {
            surface: surface_shader_id(stage, prim.path(), frame),
            path,
            bound: false,
        }),
        ListKind::Pass => ListRecord::Pass(PassRecord {
            render_source: pass_render_source(stage, prim.path()).map(|t| t.to_string()),
            path,
        }),
    })
}

/// The materials the import's binding resolution ([`bound_material`])
/// resolves the geometry prims it renders to — meshes, spheres, curves and
/// implicit shapes,
/// walked as the import walks them: pruned subtrees left out, an instance's
/// prototype walked once in its place, a `PointInstancer`'s `prototypes`
/// targets walked once each in prototype scope (they render through it, a
/// `class` prototype included).
fn bound_materials(stage: &Stage, out: &mut HashSet<String>, frame: Option<f64>) {
    let mut prototypes: HashSet<String> = HashSet::new();
    let mut stack: Vec<(Prim, WalkScope)> =
        vec![(prim_at(stage, sdf::Path::abs_root()), WalkScope::Stage)];
    while let Some((prim, scope)) = stack.pop() {
        if prune_reason(&prim, scope, frame).is_some() {
            continue;
        }
        if prim.is_instance().unwrap_or(false)
            && let Ok(Some(proto)) = prim.prototype()
        {
            if prototypes.insert(proto.to_string()) {
                stack.push((prim_at(stage, proto), WalkScope::Prototype));
            }
            continue;
        }
        let p = || prim.path().clone();
        // An instancer draws its `prototypes` targets wherever they are
        // authored — often a `class` outside it — so they are walked as the
        // import walks them, in prototype scope, where abstractness does not
        // prune; each once.
        if let Ok(Some(instancer)) = PointInstancer::get(stage, p()) {
            for target in instancer.prototypes_rel().targets().unwrap_or_default() {
                if prototypes.insert(target.to_string()) {
                    stack.push((prim_at(stage, target), WalkScope::Prototype));
                }
            }
        }
        // An implicit shape is a surface unless it is a volume, which the
        // import checks first and which takes no material.
        let geometry = matches!(UsdMesh::get(stage, p()), Ok(Some(_)))
            || matches!(UsdSphere::get(stage, p()), Ok(Some(_)))
            || matches!(UsdBasisCurves::get(stage, p()), Ok(Some(_)))
            || matches!(UsdNurbsCurves::get(stage, p()), Ok(Some(_)))
            || (custom_token(&prim, "primvars:crust:volume:type", frame).is_none()
                && is_shape(&prim));
        if geometry && let Some(m) = bound_material(stage, &prim) {
            out.insert(m.to_string());
        }
        if let Ok(children) = prim.children() {
            stack.extend(children.into_iter().map(|c| (c, scope)));
        }
    }
}

/// In which order a walk visits a prim's children: as authored (the
/// listing's order), or as the import's traversal pops them, last first —
/// the order whose first camera is the import's fallback.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Authored,
    Import,
}

/// What a walk does with one prim.
enum Visit {
    /// Neither the prim nor anything below it.
    Prune,
    /// The prim and its subtree; `hidden` is true under an invisible ancestor.
    Enter { hidden: bool },
}

/// The prims listed so far, in the order first met, each path once.
struct Found<T> {
    items: Vec<(String, T)>,
    seen: HashSet<String>,
}

impl<T> Default for Found<T> {
    fn default() -> Self {
        Found {
            items: Vec::new(),
            seen: HashSet::new(),
        }
    }
}

impl<T> Found<T> {
    fn push(&mut self, path: String, item: T) {
        if self.seen.insert(path.clone()) {
            self.items.push((path, item));
        }
    }
}

/// Calls `found` with each `kind` prim a walk of `stage` meets, in `order`,
/// and whether it is under an invisible ancestor.
fn collect(
    stage: &Stage,
    kind: ListKind,
    order: Order,
    frame: Option<f64>,
    mut found: impl FnMut(&Prim, bool),
) {
    let mut stack: Vec<(Prim, bool)> = vec![(prim_at(stage, sdf::Path::abs_root()), false)];
    while let Some((prim, parent_hidden)) = stack.pop() {
        let Visit::Enter { hidden } = visit(stage, &prim, kind, parent_hidden, frame) else {
            continue;
        };
        if is_kind(stage, &prim, kind) {
            found(&prim, hidden);
        }
        if let Ok(children) = prim.children() {
            match order {
                // Reversed onto the stack, so they pop in authored order.
                Order::Authored => stack.extend(children.into_iter().rev().map(|c| (c, hidden))),
                Order::Import => stack.extend(children.into_iter().map(|c| (c, hidden))),
            }
        }
    }
}

/// Whether the walk for `kind` enters `prim`, following the import:
///
/// - **cameras** — the walk's rules ([`super::visit_prim`]): every [`prune_reason`]
///   but invisibility prunes; an invisible subtree is still walked, as the
///   render walks it for cameras; an instance with a prototype (and every
///   instance under an invisible ancestor) and a visible `PointInstancer`,
///   whose children are prototypes, are not entered.
/// - **lights** — the same, except that invisibility prunes too: an
///   invisible light lights nothing. A prototype's lights are never imported.
/// - **materials** — every Material prim a binding can reach: only an
///   inactive subtree, which USD removes from the stage, prunes, and an
///   instance's prototype is not entered (its materials are the prototype's,
///   bound only from inside it).
/// - **passes** — every `RenderPass` `--pass` accepts: only an inactive
///   subtree prunes.
fn visit(
    stage: &Stage,
    prim: &Prim,
    kind: ListKind,
    parent_hidden: bool,
    frame: Option<f64>,
) -> Visit {
    let pruned = prune_reason(prim, WalkScope::Stage, frame);
    let is_instance = prim.is_instance().unwrap_or(false);
    let has_prototype = || matches!(prim.prototype(), Ok(Some(_)));
    let is_instancer = || {
        PointInstancer::get(stage, prim.path().clone())
            .ok()
            .flatten()
            .is_some()
    };
    match kind {
        ListKind::Camera => {
            if pruned.is_some_and(|r| r != Prune::Invisible) {
                return Visit::Prune;
            }
            let hidden = parent_hidden || pruned == Some(Prune::Invisible);
            if is_instance && (hidden || has_prototype()) || !hidden && is_instancer() {
                return Visit::Prune;
            }
            Visit::Enter { hidden }
        }
        ListKind::Light => {
            if pruned.is_some() || is_instance && has_prototype() || is_instancer() {
                return Visit::Prune;
            }
            Visit::Enter { hidden: false }
        }
        ListKind::Material => {
            if pruned == Some(Prune::Inactive) || is_instance && has_prototype() {
                return Visit::Prune;
            }
            Visit::Enter { hidden: false }
        }
        ListKind::Pass => {
            if pruned == Some(Prune::Inactive) {
                return Visit::Prune;
            }
            Visit::Enter { hidden: false }
        }
    }
}

/// Whether `prim` is of `kind`: a `UsdGeomCamera`; one of the six UsdLux
/// lights the import reads; a `UsdShadeMaterial`; a `RenderPass`.
fn is_kind(stage: &Stage, prim: &Prim, kind: ListKind) -> bool {
    let path = || prim.path().clone();
    match kind {
        ListKind::Camera => matches!(UsdCamera::get(stage, path()), Ok(Some(_))),
        ListKind::Light => {
            matches!(SphereLight::get(stage, path()), Ok(Some(_)))
                || matches!(RectLight::get(stage, path()), Ok(Some(_)))
                || matches!(DiskLight::get(stage, path()), Ok(Some(_)))
                || matches!(CylinderLight::get(stage, path()), Ok(Some(_)))
                || matches!(DistantLight::get(stage, path()), Ok(Some(_)))
                || matches!(DomeLight::get(stage, path()), Ok(Some(_)))
                || matches!(DomeLight_1::get(stage, path()), Ok(Some(_)))
        }
        ListKind::Material => matches!(UsdMaterial::get(stage, path()), Ok(Some(_))),
        ListKind::Pass => matches!(UsdRenderPass::get(stage, path()), Ok(Some(_))),
    }
}

/// The first target of the pass at `path`'s `renderSource`, read off the
/// listed stage: `crust ls` reads the stage it lists, not a scene index.
fn pass_render_source(stage: &Stage, path: &sdf::Path) -> Option<sdf::Path> {
    use openusd_schemas::render::{Pass, PassSchema};
    Pass::get(stage, path.clone())
        .ok()
        .flatten()?
        .render_source_rel()
        .targets()
        .ok()?
        .into_iter()
        .next()
}

// -----------------------------------------------------------------------
// The stage readers only the listing uses
// -----------------------------------------------------------------------
//
// `crust ls` walks the stage, not the scene index, so these read openusd
// directly: the import's pruning rule, binding resolution and shader ids as
// the direct importer read them, and attribute values with its
// authored-versus-fallback rule (the scene index's generic data carry the
// same rule for the import, `index::data`).

/// Where a prim is met, which decides whether [`Prune::Abstract`] applies.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkScope {
    /// The stage's own namespace: the top-level traversal and the placement
    /// count that must agree with it.
    Stage,
    /// Inside a prototype, reached through an instance. A prototype is
    /// commonly authored under a `class` prim, so abstractness does not prune
    /// here.
    Prototype,
}

/// [`Prune`]'s rule read on the stage, for a listing that walks the stage:
/// abstractness (in the stage's own namespace only) and activity, then
/// purpose and visibility at `time`.
fn prune_reason(prim: &Prim, scope: WalkScope, time: Option<f64>) -> Option<Prune> {
    if scope == WalkScope::Stage && prim.is_abstract().unwrap_or(false) {
        return Some(Prune::Abstract);
    }
    if !prim.is_active().unwrap_or(true) {
        return Some(Prune::Inactive);
    }
    let token = |name: &str| prim_value(prim, name, time);
    if let Some(v) = token("purpose") {
        match v.as_str() {
            Some("proxy") => return Some(Prune::Purpose("proxy")),
            Some("guide") => return Some(Prune::Purpose("guide")),
            _ => {}
        }
    }
    token("visibility")
        .is_some_and(|v| v.as_str() == Some("invisible"))
        .then_some(Prune::Invisible)
}

/// The material bound to `prim` for a final render, resolved the way
/// `UsdShadeMaterialBindingAPI::ComputeBoundMaterial` does.
///
/// Two things this has to get right that a direct lookup on the prim does not:
///
/// - **Inheritance.** A binding on an ancestor applies to every prim beneath
///   it, and production assets bind on the group: ALab binds each asset's
///   `GEO` scope, never its meshes. openusd-schemas' `bound_material`
///   walks the ancestors (binding strength and collection bindings included),
///   but only from a prim that *carries* `MaterialBindingAPI`, and
///   `MaterialBindingAPI::get` returns `None` for any other — which is every
///   mesh under a bound group. So the walk starts at the nearest ancestor
///   that has the API. Skipping the API-less prims in between loses nothing:
///   USD disregards a binding on a prim without the API applied.
/// - **Purpose.** ALab authors `material:binding:full` and
///   `material:binding:preview` and almost never the all-purpose relationship,
///   so asking for `""` alone found nothing on 7 256 prims.
/// - **Targets outside the stage.** openusd-schemas' `bound_material`
///   answers with the `Material` prim, and so `None` wherever the winning
///   target is not one *on this stage* — which a streamed import's masked
///   stage makes of every material outside the chunk (the Cornell box binds
///   `/scene/Looks/…` from five sibling chunks). That answer would drop the
///   binding: no material provenance for `crust mcp`, no
///   `material.fallback_default` warning. So when it answers nothing, the
///   binding is read as the *winning* binding's target instead
///   ([`winning_binding_target`]), which the resolution then composes on the
///   stage it keeps for materials outside the chunk (`materials::material_stage`),
///   reporting it unresolvable only when it is not there either. It has to
///   be the winner, strength and
///   collections included: a weaker binding that does resolve would render
///   its material where the winner's default grey belongs.
pub(super) fn bound_material(stage: &Stage, prim: &Prim) -> Option<sdf::Path> {
    let api = nearest_binding_api(stage, prim.path().clone())?;
    match api.bound_material(RENDER_BINDING_PURPOSE) {
        Ok(Some(material)) => Some(material.path().clone()),
        _ => winning_binding_target(stage, api.path()),
    }
}

/// The `MaterialBindingAPI` of `path` or its nearest ancestor carrying it.
fn nearest_binding_api(stage: &Stage, path: sdf::Path) -> Option<MaterialBindingAPI> {
    let mut path = Some(path);
    while let Some(p) = path {
        if p.is_abs_root() {
            return None;
        }
        if let Ok(Some(api)) = MaterialBindingAPI::get(stage, p.clone()) {
            return Some(api);
        }
        path = p.parent();
    }
    None
}

/// The target of the binding that wins for `queried`, as openusd-schemas'
/// `bound_material` chooses it, but answering the target path whether or not
/// it is a `Material` on this stage. [`RENDER_BINDING_PURPOSE`] is resolved
/// across the whole ancestor chain before the all-purpose binding; within a
/// purpose the closest binding wins unless an ancestor's is
/// `strongerThanDescendants` (the topmost such then wins); at one prim a
/// collection binding including `queried` beats the direct one, in native
/// property order. The walk visits only prims carrying `MaterialBindingAPI`.
fn winning_binding_target(stage: &Stage, queried: &sdf::Path) -> Option<sdf::Path> {
    [RENDER_BINDING_PURPOSE, ""]
        .into_iter()
        .find_map(|purpose| {
            let mut winner = None;
            let mut at = Some(queried.clone());
            while let Some(api) = at.and_then(|p| nearest_binding_api(stage, p)) {
                if let Some((target, strength)) = winning_binding_at(stage, &api, queried, purpose)
                    && (winner.is_none() || strength == BindingStrength::StrongerThanDescendants)
                {
                    winner = Some(target);
                }
                at = api.path().parent();
            }
            winner
        })
}

/// The binding that wins at `api`'s prim for `queried` and one `purpose`, as
/// `(target, strength)`: a collection binding whose collection includes
/// `queried`, in native property order, else the direct binding.
fn winning_binding_at(
    stage: &Stage,
    api: &MaterialBindingAPI,
    queried: &sdf::Path,
    purpose: &str,
) -> Option<(sdf::Path, BindingStrength)> {
    let prefix = "material:binding:collection:";
    for name in api.authored_property_names().unwrap_or_default() {
        let Some(rest) = name.as_str().strip_prefix(prefix) else {
            continue;
        };
        // `<purpose>:<name>` is purpose-restricted; anything else is
        // all-purpose, as C++ `_GetMaterialPurpose` counts tokens.
        let (binding_purpose, binding_name) = match rest.split_once(':') {
            Some((p, n)) if !n.contains(':') => (p, n),
            _ => ("", rest),
        };
        if binding_purpose != purpose {
            continue;
        }
        let Ok(Some(binding)) =
            CollectionBinding::new(api.collection_binding_rel(binding_name, purpose))
        else {
            continue;
        };
        let (collection, target) = (binding.collection_path(), binding.material_path().clone());
        if collection_includes(stage, collection, queried) {
            let strength = api
                .path()
                .append_property(name.as_str())
                .ok()
                .and_then(|rel| stage.field::<sdf::Value>(rel, "bindMaterialAs").ok())
                .and_then(|v| match v {
                    Some(sdf::Value::Token(t)) => BindingStrength::from_token(t),
                    _ => None,
                })
                .unwrap_or_default();
            return Some((target, strength));
        }
    }
    let binding = api.direct_binding(purpose).ok()??;
    let strength = binding.strength().unwrap_or_default();
    Some((binding.material_path().clone(), strength))
}

/// Whether the collection at `collection` (a `<prim>.collection:<name>`
/// path) includes `queried`; a path that names no collection includes nothing.
fn collection_includes(stage: &Stage, collection: &sdf::Path, queried: &sdf::Path) -> bool {
    let Some((prim, name)) = CollectionAPI::instance_at_path(collection) else {
        return false;
    };
    stage
        .prim(prim)
        .ok()
        .and_then(|prim| {
            CollectionAPI::from_prim_unchecked(prim, name)
                .membership_query()
                .ok()
        })
        .is_some_and(|q| q.is_included(queried))
}

/// The `info:id` of the surface shader the import decodes the material at
/// `mat_path` from, in the import's order: a `PxrDisneyBsdf` child first
/// (see `materials::resolve_material_uncached`), else the shader the surface terminal
/// resolves to. `None` when there is none — a MaterialX reference composes
/// no shader prim. What `crust ls material --json` reports as `surface`.
fn surface_shader_id(stage: &Stage, mat_path: &sdf::Path, time: Option<f64>) -> Option<String> {
    if has_shader_id(stage, mat_path, "PxrDisneyBsdf", time) {
        return Some("PxrDisneyBsdf".to_owned());
    }
    let mat = UsdMaterial::get(stage, mat_path.clone()).ok().flatten()?;
    terminal_shader(mat.surface_source(SURFACE_RENDER_CONTEXTS))
        .and_then(|s| shader_info_id(&s, time))
}

/// The first `Shader`-typed source driving a resolved terminal.
fn terminal_shader(
    resolved: Result<
        Option<openusd_schemas::shade::ResolvedTerminal>,
        openusd_schemas::SchemaError,
    >,
) -> Option<Shader> {
    resolved
        .ok()
        .flatten()
        .and_then(|t| t.sources().iter().find_map(TerminalSource::shader).cloned())
}

fn shader_info_id(shader: &Shader, time: Option<f64>) -> Option<String> {
    // `UsdShadeNodeDefAPI::GetShaderId`: the composed `info:id` when the
    // shader's implementation source is `id`.
    if let Ok(Some(id)) = shader.node_def().shader_id() {
        return Some(id.as_str().to_owned());
    }
    // Otherwise the raw `info:id`, as the import has always read it whatever
    // the implementation source says.
    value_at(&shader.attribute("info:id"), time).and_then(decode_text)
}

/// Whether the material has a child `Shader` prim with this `info:id`.
///
/// Cheaper and more reliable than resolving a render-context output for the
/// question actually being asked — "does this material shade with X" — since a
/// material may declare several context outputs and USD gives no ordering
/// between them without a configured render context.
fn has_shader_id(stage: &Stage, mat_path: &sdf::Path, id: &str, time: Option<f64>) -> bool {
    let Ok(children) = prim_at(stage, mat_path.clone()).children() else {
        return false;
    };
    children
        .iter()
        .any(|c| custom_token(c, "info:id", time).as_deref() == Some(id))
}

/// `attr`'s value at time code `time` (`None`: time code 0, the index's time
/// without `-f`), or `None` when it is unauthored, blocked or unreadable.
///
/// openusd resolves an unauthored schema attribute to its schema fallback,
/// and the importer's readers take `None` as "unauthored": a product inherits
/// the settings' resolution and a camera's focus distance is crust's 10, not
/// the schema's 0. So a
/// value equal to the fallback is asked whether a layer authored it — only
/// then, since a value that differs from the fallback was authored, and an
/// attribute no schema declares (`crust:*`, primvars, shader inputs) has none.
fn value_at(attr: &Attribute, time: Option<f64>) -> Option<sdf::Value> {
    let value = attr
        .get_at::<sdf::Value>(openusd::usd::TimeCode::new(time.unwrap_or(0.0)))
        .ok()
        .flatten()?;
    match attr.fallback_value() {
        Ok(Some(fallback)) if fallback == value => attr.has_authored_value().ok()?.then_some(value),
        _ => Some(value),
    }
}

/// The value of `prim`'s attribute `name` at `time` — [`value_at`] by
/// name.
fn prim_value(prim: &Prim, name: &str, time: Option<f64>) -> Option<sdf::Value> {
    value_at(&prim.attribute(name), time)
}

fn custom_token(prim: &Prim, name: &str, time: Option<f64>) -> Option<String> {
    prim_value(prim, name, time).and_then(decode_text)
}

/// Whether `prim` is one of the seven shapes `implicit::Shape::read` reads, from
/// its USD type alone: what `crust ls` asks of the stage it lists.
fn is_shape(prim: &Prim) -> bool {
    let prim = || prim.clone();
    matches!(Cube::from_prim(prim()), Ok(Some(_)))
        || matches!(Plane::from_prim(prim()), Ok(Some(_)))
        || matches!(Cylinder::from_prim(prim()), Ok(Some(_)))
        || matches!(Cylinder_1::from_prim(prim()), Ok(Some(_)))
        || matches!(Cone::from_prim(prim()), Ok(Some(_)))
        || matches!(Capsule::from_prim(prim()), Ok(Some(_)))
        || matches!(Capsule_1::from_prim(prim()), Ok(Some(_)))
}
