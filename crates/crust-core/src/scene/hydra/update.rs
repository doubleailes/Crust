//! In-place updates (design D9): what an update's batch changed, rebuilt
//! from the delegate's index by the functions the commit built it with, at
//! the index's time, and put into the committed scene. Never a second
//! implementation: the bit-identity of an in-place update with a fresh
//! commit rests on there being one. Anything a rebuild cannot vouch for is
//! refused ([`Structural`]), and the delegate commits afresh.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crust_rt::Geometry;
use glam::Affine3A;
use moho::openusd::sdf;
use moho::scene_index::SceneIndexHandle;

use crate::camera::Camera;
use crate::light::{LightKind, LightList};
use crate::live::{LightPath, LinkRecord, MeshKeyFacts, Provenance, ShutterKey, Structural};
use crate::material::Material;
use crate::rt_world::{GeometryEdit, LinkTarget, World, WorldBuilder};
use crate::scene::Scene;
use crate::tracer::RenderSettings;
use crate::{AssetLoader, Warning};

use super::camera::build_camera;
use super::commit::{Walked, emit_light_layer};
use super::index::data::PrimData;
use super::index::host::HostFacts;
use super::light_links::{LightLinks, LinkTables, install, judge, link_ids};
use super::materials::{resolve_displacement, resolve_fresh};
use super::mesh::{
    MeshKey, MeshNeeds, MeshPlace, SubdivPolicy, bake_indices, bake_normals, bake_verts,
    mesh_source, placement_scale,
};
use super::shapes::emit_sphere;
use super::xform::Span;
use super::{
    ImportCaches, ImportCtx, ValidOptions, apply_exposure, asks_about_time, emit_light, frame_pose,
    hide_lights_at_infinity, read_index,
};

/// What a rebuild reads: the delegate's index, the host's facts, the stage's
/// file and the asset loader, as the commit read them.
pub(in crate::scene) struct Rebuild<'a> {
    pub(in crate::scene) index: SceneIndexHandle,
    pub(in crate::scene) facts: &'a dyn HostFacts,
    pub(in crate::scene) path: &'a Path,
    pub(in crate::scene) assets: &'a dyn AssetLoader,
    /// The delegate's environment maps ([`super::lights::Environments`]).
    pub(in crate::scene) environments: Option<&'a super::lights::Environments>,
}

/// What an update changes, as far as an in-place update can follow it:
/// from the batch's dirtied locators ([`super::commit::classify`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::scene) struct Change {
    /// The `RenderSettings` prim the render uses, or the render camera.
    pub(in crate::scene) settings: bool,
    /// Material prims whose inputs changed.
    pub(in crate::scene) materials: Vec<String>,
    /// Light prims whose inputs changed.
    pub(in crate::scene) lights: Vec<String>,
    /// The light layer is built again (`interactive-updates` design D1, D3):
    /// a light added or removed, an area light moved or reshaped, a
    /// receiver's categories changed.
    pub(in crate::scene) layer: bool,
    /// Light prims added or removed: whether each is a light the layer emits
    /// is read from the index again.
    pub(in crate::scene) layer_paths: Vec<String>,
    /// Receivers and instancers whose link categories changed.
    pub(in crate::scene) categories: Vec<String>,
    /// Prims whose transform changed, each moving the meshes it and its
    /// descendants placed ([`Provenance::movable`]).
    pub(in crate::scene) moved: Vec<String>,
    /// Mesh prims whose points, topology or primvars changed
    /// ([`Provenance::reshapable`]).
    pub(in crate::scene) reshaped: Vec<String>,
    /// Prototypes, by key, one of whose meshes changed
    /// ([`Provenance::prototype_of`]).
    pub(in crate::scene) prototypes: Vec<String>,
}

/// The settings a settings change re-reads.
pub(in crate::scene) struct SettingsUpdate {
    /// The stage's render settings, before any host override.
    pub(in crate::scene) settings: RenderSettings,
    pub(in crate::scene) camera: Camera,
    /// The products the stage asks for.
    pub(in crate::scene) aovs: crate::AovRequest,
    /// The camera prim `camera` was built from: the render camera, which a
    /// retarget moves to another prim.
    pub(in crate::scene) camera_path: String,
    /// The shutter as re-read, from that camera.
    pub(in crate::scene) shutter: ShutterKey,
    /// What the stamp records of the re-read camera and stage
    /// ([`Scene::source`]); the layer stack is the session's, unchanged.
    pub(in crate::scene) lens: Option<crate::scene::Lens>,
    pub(in crate::scene) motion_blur: bool,
}

/// One rebuilt light: its new light-list entry and the materials of the
/// geometry it shares its `Emissive` with.
pub(in crate::scene) struct LightUpdate {
    pub(in crate::scene) index: usize,
    pub(in crate::scene) light: LightKind,
    /// Its per-lobe multipliers, which the light list keeps beside it.
    pub(in crate::scene) gains: crate::LobeGains,
    pub(in crate::scene) emitters: Vec<(u32, Arc<dyn Material>)>,
}

/// The light layer built again, its links judged: ready to replace the
/// world's layer, the light list and the provenance's lights.
pub(in crate::scene) struct LayerUpdate {
    /// The emitters, numbered from the world's light base.
    world: WorldBuilder,
    /// The light list the layer built, before the links take any light out.
    lights: LightList,
    tables: LinkTables,
    /// Each light the list holds, by path, at its index before the links.
    origins: Vec<(String, crate::live::LightOrigin)>,
    paths: Vec<LightPath>,
    /// The layer's geometry runs, for [`Provenance::prim_of_geometry`].
    runs: Vec<(std::ops::Range<u32>, String)>,
    /// The receivers' record with the changed categories, when any changed.
    receivers: Option<LinkRecord>,
}

/// Rebuilt parts, ready to replace the scene's ([`Update::apply`]).
#[derive(Default)]
pub(in crate::scene) struct Update {
    /// The light layer, when it is built again: then [`Update::lights`] is
    /// empty, the layer holding every light.
    pub(in crate::scene) layer: Option<LayerUpdate>,
    pub(in crate::scene) settings: Option<SettingsUpdate>,
    /// Each rebuilt material and the slots it goes into.
    pub(in crate::scene) materials: Vec<(Vec<u32>, Arc<dyn Material>)>,
    pub(in crate::scene) lights: Vec<LightUpdate>,
    /// Geometry replaced in the world, which then commits its tree again.
    pub(in crate::scene) geometry: Vec<crate::rt_world::GeometryEdit>,
    /// The content keys reshaped meshes now have, by slot: written into the
    /// scene's provenance, so a later reshape compares against them.
    pub(in crate::scene) mesh_keys: Vec<(u32, MeshKeyFacts)>,
    /// A shared direct mesh one placement of which was reshaped into a slot
    /// of its own (design D5, case 2).
    pub(in crate::scene) split: Option<SlotSplit>,
}

/// One placement of a shared direct mesh reshaped: it takes a distinct mesh
/// of its own, placed once and baked; the mesh it left keeps the other
/// placements, the last of them baked when it is left alone.
pub(in crate::scene) struct SlotSplit {
    geom_id: u32,
    old_slot: u32,
    key: MeshKeyFacts,
    /// The other placement, baked now that it is the mesh's only one.
    rebaked: Option<u32>,
}

impl Update {
    /// Refuses an update the scene's world and lights cannot take without a
    /// re-import (see [`World::check_materials`], [`LightList::check_replace`]).
    pub(in crate::scene) fn check(
        &self,
        world: &World,
        lights: &LightList,
    ) -> Result<(), Structural> {
        world.check_geometry(&self.geometry)?;
        world.check_materials(&self.swaps())?;
        if let Some(layer) = &self.layer {
            world.check_light_layer(&layer.world)?;
        }
        for l in &self.lights {
            lights.check_replace(l.index, &l.light)?;
        }
        Ok(())
    }

    fn swaps(&self) -> Vec<(&[u32], &Arc<dyn Material>)> {
        let mut swaps: Vec<(&[u32], &Arc<dyn Material>)> = self
            .materials
            .iter()
            .map(|(slots, m)| (slots.as_slice(), m))
            .collect();
        for l in &self.lights {
            for (id, m) in &l.emitters {
                swaps.push((std::slice::from_ref(id), m));
            }
        }
        swaps
    }

    /// Puts the rebuilt parts into `scene`, after [`Update::check`]: the
    /// materials into their slots, each light into its place (its emitter's
    /// slot with it, so the light and its geometry keep one `Emissive`), the
    /// settings, camera and products over the old. The light selection is
    /// the renderer's to rebuild (`Renderer::new` / `reconfigure`).
    pub(in crate::scene) fn apply(self, scene: &mut Scene) -> Result<(), Structural> {
        self.check(&scene.world, &scene.lights)?;
        scene.world.replace_materials(&self.swaps());
        if let Some(prov) = scene.provenance.as_mut() {
            for (slot, key) in self.mesh_keys {
                if let Some(s) = prov.mesh_slots.get_mut(slot as usize) {
                    s.key = Some(key);
                }
            }
            if let Some(split) = self.split {
                let slot = prov.mesh_slots.len() as u32;
                prov.mesh_slots.push(crate::live::MeshSlotFacts {
                    key: Some(split.key),
                    placements: 1,
                    shared: false,
                    displaced: false,
                });
                if let Some(s) = prov.mesh_slots.get_mut(split.old_slot as usize) {
                    s.placements -= 1;
                }
                for m in prov.meshes.iter_mut() {
                    if m.geom_id == split.geom_id {
                        m.slot = slot;
                        m.baked = true;
                    } else if Some(m.geom_id) == split.rebaked {
                        m.baked = true;
                    }
                }
            }
        }
        if !self.geometry.is_empty() {
            scene.world.replace_geometry(self.geometry)?;
            // The scene counters stay the commit's: the unique primitives and
            // the footprint walk every instance (18 s on the Moana island,
            // more than the edit itself), and nothing renders from them.
        }
        for l in self.lights {
            scene.lights.replace(l.index, l.light);
            scene.lights.set_lobe_gains(l.index, l.gains);
        }
        if let Some(layer) = self.layer {
            apply_layer(layer, scene)?;
        }
        if let Some(s) = self.settings {
            // The camera rendered through may be another prim now: the
            // scene and its provenance name it, as a fresh import's would.
            if let Some(prov) = scene.provenance.as_mut() {
                prov.stage.camera = Some(s.camera_path.clone());
                prov.stage.shutter = s.shutter;
            }
            scene.camera_path = Some(s.camera_path);
            scene.settings = s.settings;
            scene.camera = s.camera;
            scene.aovs = s.aovs;
            scene.source.lens = s.lens;
            scene.source.depth_of_field = scene.camera.has_depth_of_field();
            scene.source.motion_blur = s.motion_blur;
            // `invisible_overrides` stays the commit's: a walk of the
            // strongest layer, which a settings edit cannot move (a
            // visibility `over` dirties the prim's `visibility`, not the
            // settings), and which grew with the scene on every refresh.
            scene.stats.image = (&scene.settings).into();
        }
        Ok(())
    }
}

/// Puts a light layer built again into `scene`: the world's layer replaced,
/// the links installed on the world, the list replaced, the provenance's
/// lights, light paths and emitter runs with them.
fn apply_layer(layer: LayerUpdate, scene: &mut Scene) -> Result<(), Structural> {
    let LayerUpdate {
        world,
        mut lights,
        tables,
        mut origins,
        paths,
        runs,
        receivers,
    } = layer;
    let g = scene.world.light_base();
    scene.world.replace_light_layer(world)?;
    let remap = install(tables, &mut scene.world, &mut lights, &mut scene.volumes);
    origins.retain_mut(|(_, origin)| match remap[origin.index] {
        Some(index) => {
            origin.index = index;
            true
        }
        None => false,
    });
    let domes_seen = scene
        .provenance
        .as_ref()
        .is_none_or(|p| p.stage.domes_seen_by_camera);
    hide_lights_at_infinity(domes_seen, &mut lights);
    scene.stats.scene.lights = lights.count();
    scene.stats.scene.geometries = scene.world.count();
    scene.lights = lights;
    if let Some(prov) = scene.provenance.as_mut() {
        prov.lights = origins.into_iter().collect();
        prov.light_paths = paths;
        prov.geometry.retain(|(ids, _)| ids.start < g);
        prov.geometry.extend(runs);
        if let Some(receivers) = receivers {
            prov.links = receivers;
        }
    }
    Ok(())
}

/// Warning codes a settings change can move, by family: the settings, the
/// products and their AOVs, the camera, the time code and the colour space.
/// Not `primvar.`: those are raised reading geometry, for the primvars the
/// geometry kept, and a refresh that would change that set is refused.
const SETTINGS_FAMILIES: &[&str] = &[
    "settings.",
    "product.",
    "aov.",
    "lpe.",
    "camera.",
    "time.",
    "color.",
    "subdiv.",
];

/// Rebuilds what `change` names from the delegate's index, as the commit
/// that built the scene would build it now, without touching the scene.
/// `warnings` are the scene's own: a rebuild about a prim they name is
/// refused. `world` is the scene's: a geometry rebuild reads the materials
/// and committed meshes it holds.
pub(in crate::scene) fn rebuild(
    prov: &Provenance,
    at: &Rebuild<'_>,
    warnings: &[Warning],
    change: &Change,
    world: &World,
) -> Result<Update, Structural> {
    let facts = &prov.stage;
    let working = facts
        .working
        .ok_or_else(|| Structural("the scene was not imported from a stage".into()))?;
    let mut update = Update::default();
    if change.settings {
        update.settings = Some(refresh_settings(prov, at, warnings)?);
    }
    let rebuilding = Rebuilding {
        prov,
        at,
        working,
        warnings,
    };
    for path in &change.materials {
        update.materials.push(rebuilding.material(path)?);
    }
    // A light's parameters are rebuilt alone, unless its links may have
    // changed or the links took it out of the list: then, as for a light
    // added or removed, the whole layer is.
    let mut layer = change.layer;
    for path in &change.lights {
        if !layer && !rebuilding.alone(path) {
            layer = true;
        }
    }
    if layer {
        update.layer = Some(rebuilding.light_layer(change, world)?);
    } else {
        for path in &change.lights {
            update.lights.push(rebuilding.light(path)?);
        }
    }
    if !change.moved.is_empty() || !change.reshaped.is_empty() {
        let (edits, keys, split) = rebuilding.geometry(&change.moved, &change.reshaped, world)?;
        update.geometry.extend(edits);
        update.mesh_keys = keys;
        update.split = split;
    }
    for key in &change.prototypes {
        let (edits, keys) = rebuilding.prototype(key, world)?;
        update.geometry.extend(edits);
        update.mesh_keys.extend(keys);
    }
    Ok(update)
}

/// Whether `prim` is `root` or below it.
fn under(prim: &str, root: &str) -> bool {
    prim.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('.'))
}

/// Refuses a rebuild about `root` when the scene's warnings name it, or may
/// name it beyond the prims they keep: a fresh import could report them
/// differently now.
fn unwarned(warnings: &[Warning], root: &str) -> Result<(), Structural> {
    for w in warnings {
        if w.prims.len() >= crate::warnings::MAX_PRIMS || w.prims.iter().any(|p| under(p, root)) {
            return Err(Structural(format!(
                "the scene's {} warning may concern {root}",
                w.code.as_str()
            )));
        }
    }
    Ok(())
}

/// Refuses a rebuild that raised a warning the scene does not already carry
/// for the same prim, or any about `root` or the whole stage.
fn same_warnings(raised: &[Warning], scene: &[Warning], root: &str) -> Result<(), Structural> {
    for w in raised {
        let known = !w.prims.is_empty()
            && w.prims.iter().all(|p| {
                !under(p, root)
                    && scene
                        .iter()
                        .any(|s| s.code == w.code && s.prims.iter().any(|q| q == p))
            });
        if !known {
            return Err(Structural(format!(
                "rebuilding {root} raised {}",
                w.code.as_str()
            )));
        }
    }
    Ok(())
}

fn prim_path(path: &str) -> Result<sdf::Path, Structural> {
    sdf::Path::new(path)
        .ok()
        .filter(|p| p.is_prim())
        .ok_or_else(|| Structural(format!("{path} is not a prim path")))
}

/// A distinct mesh's slot and the content key it has now.
type SlotKey = (u32, MeshKeyFacts);

/// What [`Rebuilding::geometry`] rebuilt: the edits, the keys reshaped slots
/// have now, and a shared mesh's slot split.
type GeometryRebuild = (Vec<GeometryEdit>, Vec<SlotKey>, Option<SlotSplit>);

struct Rebuilding<'a> {
    prov: &'a Provenance,
    at: &'a Rebuild<'a>,
    working: crate::color::Space,
    warnings: &'a [Warning],
}

impl Rebuilding<'_> {
    fn caches(&self) -> ImportCaches<'_> {
        let mut caches = ImportCaches::new(
            self.at.assets,
            self.at.facts,
            self.at.path,
            SubdivPolicy::new(self.prov.stage.subdiv_level),
            self.working,
            self.prov.stage.mtlx_library.clone(),
        );
        caches.time = super::index::data::current_frame(self.at.index.as_ref());
        caches.environments = self.at.environments;
        caches.scene = Some(self.at.index.clone());
        caches.top = Some(self.at.index.clone());
        caches.inst = Some(self.at.index.clone());
        caches
    }

    /// The material at `path`, resolved as the commit's first binding to
    /// it resolved it, into the slots it fills.
    fn material(&self, path: &str) -> Result<(Vec<u32>, Arc<dyn Material>), Structural> {
        let origin = self
            .prov
            .material(path)
            .ok_or_else(|| Structural(format!("the scene does not use {path}")))?;
        if origin.displaced {
            return Err(Structural(format!("{path} displaces a mesh")));
        }
        unwarned(self.warnings, path)?;
        let mat_path = prim_path(path)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            let mut caches = self.caches();
            let (material, mtlx) = resolve_fresh(&mat_path, &mut caches);
            let displaces = crate::config().displace
                && resolve_displacement(&mat_path, mtlx, &mut caches).is_some();
            if displaces {
                return Err(Structural(format!("{path} now defines a displacement")));
            }
            Ok(material)
        })();
        let raised = scope.finish();
        let material = built?;
        same_warnings(&raised, self.warnings, path)?;
        Ok((origin.slots.clone(), material))
    }

    /// Whether the light at `path` can be rebuilt alone: the list holds it,
    /// and its link category ids are the ones the layer recorded, so the
    /// judged links stand.
    fn alone(&self, path: &str) -> bool {
        let Some(recorded) = self.prov.light_paths.iter().find(|l| l.path == path) else {
            return false;
        };
        let Ok(prim) = prim_path(path) else {
            return false;
        };
        self.prov.light(path).is_some() && link_ids(self.at.index.as_ref(), &prim) == recorded.links
    }

    /// The light at `path`, emitted as the layer emitted it, its geometry
    /// ids the ones it had.
    fn light(&self, path: &str) -> Result<LightUpdate, Structural> {
        let origin = self
            .prov
            .light(path)
            .ok_or_else(|| Structural(format!("the scene has no light {path}")))?;
        unwarned(self.warnings, path)?;
        let light_path = prim_path(path)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            // The light's transform and inputs from the index, as the commit
            // reads them.
            let index = &self.at.index;
            let world_xf =
                crate::scene::hydra::xform::world(&PrimData::at(index.as_ref(), &light_path));
            // A world whose ids start at the light's first: its geometry
            // takes the ids it had.
            let world = WorldBuilder::starting_at(origin.geoms.start);
            let mut ctx = scratch_ctx(self, world);
            let light = PrimData::in_index(index, &light_path);
            if !emit_light(&mut ctx, &light, world_xf) {
                return Err(Structural(format!("{path} is no longer a light")));
            }
            let geoms = origin.geoms.start..ctx.world.count() as u32;
            if ctx.lights.count() != 1 || geoms != origin.geoms {
                return Err(Structural(format!("{path} would emit other geometry")));
            }
            let emitters = geoms
                .map(|id| (id, ctx.world.material(id).clone()))
                .collect();
            let gains = ctx.lights.lobe_gains(0);
            let (light, _) = ctx.lights.remove(0);
            Ok(LightUpdate {
                index: origin.index,
                light,
                gains,
                emitters,
            })
        })();
        let raised = scope.finish();
        let light = built?;
        same_warnings(&raised, self.warnings, path)?;
        Ok(light)
    }
}

impl Rebuilding<'_> {
    /// The light layer built again (`interactive-updates` design D1–D3):
    /// every light of the layer, the added ones in and the removed ones out,
    /// emitted in path order by the commit's [`emit_light_layer`] into a
    /// world starting at the scene's light base, and the links judged again
    /// over the receivers' record (their categories read again where
    /// `change` names them). Refused under an active render pass, which
    /// flags the light sources as it meets them, when the scene carries a
    /// warning about a light or a link, and when the rebuild raises any.
    fn light_layer(&self, change: &Change, world: &World) -> Result<LayerUpdate, Structural> {
        let prov = self.prov;
        if let Some(pass) = &prov.stage.options.pass {
            return Err(Structural(format!(
                "render pass {pass} flags the light sources as it meets them"
            )));
        }
        let lit: std::collections::HashSet<&str> =
            prov.light_paths.iter().map(|l| l.path.as_str()).collect();
        if let Some(w) = self.warnings.iter().find(|w| {
            let code = w.code.as_str();
            code.starts_with("light.")
                || code.starts_with("light_link.")
                || code.starts_with("ies.")
                || w.prims.iter().any(|p| lit.contains(p.as_str()))
        }) {
            return Err(Structural(format!(
                "the scene's {} warning may concern a light",
                w.code.as_str()
            )));
        }
        let index = self.at.index.as_ref();
        // The lights now, in path order: the layer's, less those the batch
        // added or removed, which the index says whether the walk would
        // send to the layer.
        let touched: std::collections::BTreeSet<&str> =
            change.layer_paths.iter().map(String::as_str).collect();
        let mut paths: Vec<sdf::Path> = Vec::with_capacity(lit.len() + touched.len());
        for l in &prov.light_paths {
            if !touched.contains(l.path.as_str()) {
                paths.push(prim_path(&l.path)?);
            }
        }
        for t in touched {
            let path = prim_path(t)?;
            match super::commit::walked_light(index, &path) {
                Walked::Light => paths.push(path),
                Walked::Not => {}
                Walked::Unsure(why) => return Err(Structural(why)),
            }
        }
        paths.sort();
        paths.dedup();
        let lights: Vec<(sdf::Path, glam::Mat4)> = paths
            .into_iter()
            .map(|p| {
                let world_xf = crate::scene::hydra::xform::world(&PrimData::at(index, &p));
                (p, world_xf)
            })
            .collect();
        let base = world.light_base();
        // Every prim whose categories Moho dirtied; only the receivers the
        // record holds are read again (a light's or the root's are nothing).
        let categories: Vec<sdf::Path> = change
            .categories
            .iter()
            .filter_map(|c| sdf::Path::new(c).ok())
            .collect();
        let scope = crate::WarningScope::enter();
        let layer = {
            let mut ctx = scratch_ctx(self, WorldBuilder::starting_at(base));
            let layer = emit_light_layer(&mut ctx, &lights);
            let receivers = (!categories.is_empty()).then(|| {
                let mut record = prov.links.clone();
                record.reread(index, &categories);
                record
            });
            let scratch = &ctx.world;
            let unlinked = |id: u32| {
                if id < base {
                    world.unlinked_mask(id)
                } else {
                    scratch.mask(id)
                }
            };
            let tables = judge(
                receivers.as_ref().unwrap_or(&prov.links),
                &layer.links,
                &ctx.lights,
                scratch.count(),
                &unlinked,
                &prov.volume_masks,
            );
            LayerUpdate {
                runs: layer.links.record.geometry_runs(),
                world: ctx.world,
                lights: ctx.lights,
                tables,
                origins: ctx.light_origins,
                paths: layer.paths,
                receivers,
            }
        };
        let raised = scope.finish();
        if let Some(w) = raised.first() {
            return Err(Structural(format!(
                "building the light layer again raised {}",
                w.code.as_str()
            )));
        }
        Ok(layer)
    }

    /// The meshes the prims of `moved` and their descendants placed, placed
    /// again at their transforms now, and the meshes of `reshaped` read
    /// again: each by the import's own reading, interning and baking, so the
    /// geometry and side tables are the ones a fresh import gives.
    ///
    /// An instance keeps its committed mesh and takes the new transform. A
    /// baked mesh is read again and baked at the new transform; its key must
    /// be the one it had (a move does not change the mesh), or, reshaped,
    /// must equal no other distinct mesh's (a fresh import would then share
    /// one mesh between them and instance both).
    fn geometry(
        &self,
        moved: &[String],
        reshaped: &[String],
        world: &World,
    ) -> Result<GeometryRebuild, Structural> {
        let prov = self.prov;
        if !world.keeps_inputs() {
            return Err(Structural("the world kept no geometry to replace".into()));
        }
        if !prov.stage.primvars.is_empty() {
            return Err(Structural(
                "the products keep primvars the geometry would read again".into(),
            ));
        }
        // geom_id → (the root whose stage reads it, whether it is reshaped).
        let mut todo: BTreeMap<u32, (String, bool)> = BTreeMap::new();
        for root in moved {
            if let Some(why) = prov.movable(root) {
                return Err(Structural(why));
            }
            for ids in prov.geometry_of(root) {
                for id in ids.filter(|&id| {
                    prov.mesh(id).is_some() || prov.spheres.binary_search(&id).is_ok()
                }) {
                    todo.entry(id).or_insert_with(|| (root.clone(), false));
                }
            }
        }
        for prim in reshaped {
            if let Some(why) = prov.reshapable(prim) {
                return Err(Structural(why));
            }
            for ids in prov.geometry_of(prim) {
                for id in ids {
                    todo.entry(id).or_insert_with(|| (prim.clone(), true)).1 = true;
                }
            }
        }
        // A reshaped placement of a mesh other direct placements share: the
        // slot splits (design D5, case 2), alone in its batch.
        let shared: Vec<crate::live::MeshOrigin> = todo
            .iter()
            .filter(|(_, (_, reshape))| *reshape)
            .filter_map(|(&id, _)| prov.mesh(id).copied())
            .filter(|o| {
                prov.mesh_slots
                    .get(o.slot as usize)
                    .is_some_and(|s| s.placements > 1)
            })
            .collect();
        if let Some(&origin) = shared.first() {
            if todo.len() != 1 {
                return Err(Structural(
                    "a shared mesh is reshaped among other geometry edits".into(),
                ));
            }
            let (edits, split) = self.split_slot(origin, world)?;
            return Ok((edits, Vec::new(), Some(split)));
        }
        let mut edits = Vec::with_capacity(todo.len());
        // The keys this batch's reshapes give their slots, which later
        // reshapes in the batch compare against instead of the import's.
        let mut keys: Vec<SlotKey> = Vec::new();
        for (id, (_, reshape)) in todo {
            let prim_str = prov
                .prim_of_geometry(id)
                .ok_or_else(|| Structural(format!("geometry {id} has no prim")))?
                .to_owned();
            unwarned(self.warnings, &prim_str)?;
            let scope = crate::WarningScope::enter();
            let built = match prov.mesh(id) {
                Some(&origin) => self.placed_mesh(&prim_str, origin, reshape, world, &keys, false),
                None => self
                    .placed_sphere(&prim_str, id, world)
                    .map(|edit| (edit, None)),
            };
            let raised = scope.finish();
            let (edit, key) = built?;
            same_warnings(&raised, self.warnings, &prim_str)?;
            edits.push(edit);
            keys.extend(key);
        }
        Ok((edits, keys, None))
    }

    /// Design D5, case 2: the placement `origin` of a mesh other direct
    /// placements share, reshaped. A fresh commit gives it a distinct mesh
    /// of its own, placed once, so baked; the mesh it leaves keeps the other
    /// placements, and bakes the last of them when it is left alone. Both
    /// are read and baked by the commit's readers ([`Self::placed_mesh`]).
    fn split_slot(
        &self,
        origin: crate::live::MeshOrigin,
        world: &World,
    ) -> Result<(Vec<GeometryEdit>, SlotSplit), Structural> {
        let prov = self.prov;
        let prim_of = |id: u32| {
            prov.prim_of_geometry(id)
                .map(str::to_owned)
                .ok_or_else(|| Structural(format!("geometry {id} has no prim")))
        };
        let others: Vec<crate::live::MeshOrigin> = prov
            .placements_of(origin.slot)
            .filter(|m| m.geom_id != origin.geom_id)
            .copied()
            .collect();
        let prim = prim_of(origin.geom_id)?;
        unwarned(self.warnings, &prim)?;
        // The reshaped placement is its new mesh's only one, static.
        if !super::mesh::bakes(1, false, origin.moving) {
            return Err(Structural(format!("{prim} would not bake")));
        }
        let alone = match others.as_slice() {
            [q] => {
                if !super::mesh::bakes(1, false, q.moving) {
                    return Err(Structural(
                        "the mesh's last placement would not bake".into(),
                    ));
                }
                let q_prim = prim_of(q.geom_id)?;
                unwarned(self.warnings, &q_prim)?;
                Some((*q, q_prim))
            }
            _ => None,
        };
        let scope = crate::WarningScope::enter();
        let built = (|| {
            let baked = crate::live::MeshOrigin {
                baked: true,
                ..origin
            };
            let (edit, key) = self.placed_mesh(&prim, baked, true, world, &[], true)?;
            let key = key
                .map(|(_, k)| k)
                .ok_or_else(|| Structural(format!("{prim}'s mesh has no key")))?;
            let mut edits = vec![edit];
            let mut rebaked = None;
            if let Some((q, q_prim)) = &alone {
                let baked = crate::live::MeshOrigin { baked: true, ..*q };
                let (edit, _) = self.placed_mesh(q_prim, baked, false, world, &[], false)?;
                edits.push(edit);
                rebaked = Some(q.geom_id);
            }
            Ok((
                edits,
                SlotSplit {
                    geom_id: origin.geom_id,
                    old_slot: origin.slot,
                    key,
                    rebaked,
                },
            ))
        })();
        let raised = scope.finish();
        let built = built?;
        same_warnings(&raised, self.warnings, &prim)?;
        Ok(built)
    }

    /// Design D5, case 3: the prototype at `key`, one of whose meshes
    /// changed, built again from the index by the commit's prototype walk
    /// (fresh caches) and placed again at every placement, each part's new
    /// kernel scene at the transform it had. Refused when its parts would
    /// lay out otherwise, it holds nested prototypes or curves, a placement
    /// was not the shared parts attached whole and static, a mesh it reads
    /// is read by direct placements or another prototype too, or a new mesh
    /// equals a distinct mesh of the scene (a fresh commit would share it).
    fn prototype(
        &self,
        key: &str,
        world: &World,
    ) -> Result<(Vec<GeometryEdit>, Vec<SlotKey>), Structural> {
        let prov = self.prov;
        let facts = prov
            .prototypes
            .get(key)
            .ok_or_else(|| Structural(format!("no prototype {key}")))?;
        if prov.stage.adaptive.is_some() {
            return Err(Structural("subdivision is adaptive".into()));
        }
        if facts.nested || facts.irregular || facts.parts.iter().any(|p| p.2) {
            return Err(Structural(format!(
                "prototype {} is not a plain shared one",
                facts.usd
            )));
        }
        if !world.keeps_inputs() {
            return Err(Structural("the world kept no geometry to replace".into()));
        }
        if !prov.stage.primvars.is_empty() {
            return Err(Structural(
                "the products keep primvars the geometry would read again".into(),
            ));
        }
        for &slot in &facts.reads {
            if prov.meshes.iter().any(|m| m.slot == slot)
                || prov
                    .prototypes
                    .iter()
                    .any(|(k, f)| k != key && f.reads.contains(&slot))
            {
                return Err(Structural(format!(
                    "a mesh of prototype {} is read elsewhere too",
                    facts.usd
                )));
            }
        }
        unwarned(self.warnings, key)?;
        unwarned(self.warnings, &facts.usd)?;
        let scope = crate::WarningScope::enter();
        let built = (|| {
            let mut caches = self.caches();
            let parts = super::instancing::rebuilt_prototype(key, &facts.usd, &mut caches)
                .ok_or_else(|| Structural(format!("{key} is not a prim path")))?;
            let layout: Vec<(u32, crate::ray::RayMask, bool)> = parts
                .iter()
                .map(|p| (p.slots.len() as u32, p.mask, false))
                .collect();
            if layout != facts.parts || parts.iter().any(|p| p.nested) {
                return Err(Structural(format!(
                    "prototype {}'s parts would lay out otherwise",
                    facts.usd
                )));
            }
            let reads: Vec<u32> = parts.iter().flat_map(|p| p.reads.iter().copied()).collect();
            if reads.len() != facts.reads.len() {
                return Err(Structural(format!(
                    "prototype {} would read other meshes",
                    facts.usd
                )));
            }
            // The new meshes' keys, against every distinct mesh's now (the
            // material compared as the slot's own group).
            let fresh: HashMap<u32, &MeshKey> =
                caches.meshes.by_key.iter().map(|(k, &s)| (s, k)).collect();
            let mut keys: Vec<SlotKey> = Vec::new();
            for (&new, &old) in reads.iter().zip(&facts.reads) {
                let k = fresh
                    .get(&new)
                    .ok_or_else(|| Structural(format!("prototype {} lost a mesh", facts.usd)))?;
                let group = prov
                    .mesh_slots
                    .get(old as usize)
                    .and_then(|s| s.key.as_ref())
                    .map(|k| k.material)
                    .ok_or_else(|| {
                        Structural(format!("prototype {}'s mesh is unknown", facts.usd))
                    })?;
                let facts_key = MeshKeyFacts {
                    geo_hash: k.geo_hash,
                    n_points: k.n_points,
                    n_indices: k.n_indices,
                    n_uvs: k.n_uvs,
                    smooth: k.smooth,
                    material: group,
                };
                let geometry = |o: &MeshKeyFacts| {
                    (o.geo_hash, o.n_points, o.n_indices, o.n_uvs, o.smooth)
                        == (k.geo_hash, k.n_points, k.n_indices, k.n_uvs, k.smooth)
                };
                if prov
                    .mesh_slots
                    .iter()
                    .any(|s| s.key.as_ref().is_some_and(geometry))
                {
                    return Err(Structural(format!(
                        "a mesh of prototype {} would equal another distinct mesh",
                        facts.usd
                    )));
                }
                keys.push((old, facts_key));
            }
            let mut edits = Vec::new();
            for &first in &facts.placements {
                edits.extend(super::instancing::replaced_placement(&parts, world, first)?);
            }
            Ok((edits, keys))
        })();
        let raised = scope.finish();
        let built = built?;
        if let Some(w) = raised.first() {
            return Err(Structural(format!(
                "rebuilding prototype {} raised {}",
                facts.usd,
                w.code.as_str()
            )));
        }
        Ok(built)
    }

    /// A static analytic sphere at its prim's transform on `stage`, placed
    /// by the traversal's own `emit_sphere` into a scratch world.
    fn placed_sphere(
        &self,
        prim_str: &str,
        id: u32,
        world: &World,
    ) -> Result<GeometryEdit, Structural> {
        let path = prim_path(prim_str)?;
        let index = &self.at.index;
        if !PrimData::at(index.as_ref(), &path).is_a("Sphere") {
            return Err(Structural(format!("{prim_str} is not a Sphere")));
        }
        let world_xf =
            crate::scene::hydra::xform::world(&PrimData::at(self.at.index.as_ref(), &path));
        // Emitted alone, at id 0: a sphere's geometry and tables do not
        // depend on its id, so the edit is re-addressed rather than the
        // scratch world padded up to the id, which made a batch of spheres
        // quadratic.
        let mut scratch = WorldBuilder::new();
        let data = PrimData::in_index(index, &path);
        let placed = emit_sphere(
            &mut scratch,
            &data,
            Span::fixed(world_xf),
            world.material_of(id).clone(),
            self.working,
        );
        if placed != Some(0) {
            return Err(Structural(format!(
                "{prim_str} would not be a static sphere"
            )));
        }
        let mut edit = scratch.edit_of(0);
        edit.geom_id = id;
        Ok(edit)
    }

    /// One placement of a direct mesh at its prim's transform on `stage`.
    fn placed_mesh(
        &self,
        prim_str: &str,
        origin: crate::live::MeshOrigin,
        reshape: bool,
        world: &World,
        batch: &[SlotKey],
        own_too: bool,
    ) -> Result<(GeometryEdit, Option<SlotKey>), Structural> {
        let id = origin.geom_id;
        let path = prim_path(prim_str)?;
        let world_xf =
            crate::scene::hydra::xform::world(&PrimData::at(self.at.index.as_ref(), &path));
        // A singular placement is baked at attach time, with no slot: a
        // fresh import would build it another way.
        if world_xf.determinant().abs() < 1e-12 {
            return Err(Structural(format!(
                "{prim_str} would have a singular transform"
            )));
        }
        let l2w = Affine3A::from_mat4(world_xf);
        let scale = placement_scale(&l2w);
        // An instance placement keeps its committed, possibly shared, mesh:
        // it can only move. A shape change on one commits afresh
        // (`Provenance::reshapable` refuses it, and `classify` never sends a
        // dirtied shape down the move path); refuse here too rather than
        // render a stale shape.
        if !origin.baked && reshape {
            return Err(Structural(format!(
                "{prim_str}'s shape changed, and its placement shares its mesh"
            )));
        }
        if !origin.baked && !reshape {
            let Some(Geometry::Instance {
                scene,
                transform_end: None,
                ..
            }) = world.input(id)
            else {
                return Err(Structural(format!(
                    "geometry {id} is not a static instance"
                )));
            };
            let geometry = Geometry::Instance {
                scene: scene.clone(),
                transform: l2w,
                transform_end: None,
            };
            return Ok((world.edit_with(id, geometry, scale), None));
        }
        if !PrimData::at(self.at.index.as_ref(), &path).is_a("Mesh") {
            return Err(Structural(format!("{prim_str} is not a Mesh")));
        }
        let slot_facts = self
            .prov
            .mesh_slots
            .get(origin.slot as usize)
            .ok_or_else(|| Structural(format!("{prim_str}'s mesh is unknown")))?;
        let old_key = slot_facts
            .key
            .as_ref()
            .ok_or_else(|| Structural(format!("{prim_str}'s mesh is unknown")))?;
        let material = world.material_of(id).clone();
        let mut caches = self.caches();
        let meshes = &mut caches.meshes;
        let needs = MeshNeeds::of(&*material, None, &meshes.primvars, meshes.working);
        let data = PrimData::in_index(&self.at.index, &path);
        let src = mesh_source(
            &data,
            &data,
            needs,
            &mut meshes.subdiv,
            MeshPlace::World(&world_xf),
        )
        .ok_or_else(|| Structural(format!("{prim_str} no longer reads as a mesh")))?;
        let key = MeshKey::new(&src, &material);
        let key = MeshKeyFacts {
            geo_hash: key.geo_hash,
            n_points: key.n_points,
            n_indices: key.n_indices,
            n_uvs: key.n_uvs,
            smooth: key.smooth,
            material: old_key.material,
        };
        if !reshape && key != *old_key {
            return Err(Structural(format!("{prim_str}'s mesh changed too")));
        }
        // Every other slot's key as it is now: this batch's for a slot it
        // reshaped already, else the provenance's (which in-place reshapes
        // keep current, `Update::mesh_keys`).
        let current = |i: usize| {
            batch
                .iter()
                .find(|(slot, _)| *slot as usize == i)
                .map(|(_, k)| k)
                .or(self.prov.mesh_slots[i].key.as_ref())
        };
        // `own_too`: the placement leaves its slot, which other placements
        // keep: equal to it again, a fresh commit would share it still.
        if reshape
            && (0..self.prov.mesh_slots.len())
                .any(|i| (own_too || i != origin.slot as usize) && current(i) == Some(&key))
        {
            return Err(Structural(format!(
                "{prim_str}'s mesh would share another prim's"
            )));
        }
        let slot = meshes
            .intern(&data, src, &material, None)
            .ok_or_else(|| Structural(format!("{prim_str} produces no triangles")))?;
        let s = &mut meshes.slots[slot as usize];
        let geom = s
            .local
            .take()
            .ok_or_else(|| Structural(format!("{prim_str}'s mesh was committed")))?;
        // As `flush_meshes` bakes a placement, tables and all.
        let mirrored = l2w.matrix3.determinant() < 0.0;
        let geometry = Geometry::TriangleMesh {
            vertices: bake_verts(&geom.verts, &l2w),
            indices: bake_indices(geom.tris, &l2w),
            normals: geom.normals.map(|ns| bake_normals(&ns, &l2w)),
        };
        let primvars = s.primvars.clone();
        let new_key = reshape.then_some((origin.slot, key));
        let edit = GeometryEdit {
            geom_id: id,
            geometry,
            map: s.faces.clone(),
            uv: s.uvs.clone(),
            shading: primvars.clone().filter(|t| t.shades()),
            swapped: mirrored,
            aov: primvars
                .filter(|t| t.channels.iter().any(Option::is_some))
                .map(|t| (t, mirrored)),
            scale,
        };
        Ok((edit, new_key))
    }
}

/// An import context around `world` with nothing in it but the caches a
/// fresh import starts with, for one light's emitter to run in.
fn scratch_ctx<'a>(r: &'a Rebuilding<'a>, world: WorldBuilder) -> ImportCtx<'a> {
    let mut lights = LightList::new();
    lights.set_luma(crate::color::luma(r.working));
    ImportCtx {
        world,
        lights,
        volumes: Vec::new(),
        camera: None,
        wanted_camera: None,
        first_camera: None,
        first_camera_path: None,
        cameras_seen: Vec::new(),
        caches: r.caches(),
        pending_meshes: Vec::new(),
        spheres: Vec::new(),
        links: LightLinks::default(),
        pass: None,
        light_origins: Vec::new(),
        light_prims: Vec::new(),
        layer: None,
        settings: crate::tracer::RenderSettings::default(),
        framing: Default::default(),
        stage_path: r.at.path,
        assets: r.at.assets,
    }
}

/// The settings, products and camera re-read as the import reads them.
fn refresh_settings(
    prov: &Provenance,
    at: &Rebuild<'_>,
    warnings: &[Warning],
) -> Result<SettingsUpdate, Structural> {
    let facts = &prov.stage;
    if let Some(w) = warnings.iter().find(|w| {
        SETTINGS_FAMILIES
            .iter()
            .any(|f| w.code.as_str().starts_with(f))
    }) {
        return Err(Structural(format!(
            "the scene's {} warning may change with the settings",
            w.code.as_str()
        )));
    }
    let Some(rendered) = facts.camera.as_deref() else {
        return Err(Structural("the scene renders the procedural camera".into()));
    };
    let rendered = prim_path(rendered)?;
    let mut options = facts.options.clone();
    options.frame = super::index::data::current_frame(at.index.as_ref());
    let valid = ValidOptions::check(&options)
        .map_err(|e| Structural(format!("the import options: {e}")))?;
    let scope = crate::WarningScope::enter();
    let built = (|| {
        let read = read_index(at.facts, at.index.as_ref(), at.path, &options, &valid)
            .map_err(|e| Structural(format!("the settings: {e}")))?;
        if Some(read.working) != facts.working {
            return Err(Structural("the working colour space changed".into()));
        }
        if read.subdiv.level != facts.subdiv_level {
            return Err(Structural("the subdivision level changed".into()));
        }
        // Tessellation that reads the dicing camera stands only when it reads
        // the same camera, and what it read is what it would read now, bit
        // for bit: an exposure keeps it, a move, a lens or a resolution does
        // not. Moving a render camera that does not dice keeps it too.
        if read.dicing.as_ref().map(ToString::to_string) != facts.dicing {
            return Err(Structural("the dicing camera changed".into()));
        }
        if read.subdiv.adaptive.map(|r| r.key()) != facts.adaptive {
            return Err(Structural(
                "adaptive subdivision would read another camera projection".into(),
            ));
        }
        if facts.on_screen_shapes.is_some()
            && read.projection.map(|p| p.key()) != facts.on_screen_shapes
        {
            return Err(Structural(
                "implicit shapes would be sized for another camera projection".into(),
            ));
        }
        if facts.unsized_shapes && read.projection.is_some() {
            return Err(Structural(
                "implicit shapes would be sized for the newly named camera".into(),
            ));
        }
        // The camera a fresh import would render through: the one the
        // settings or the first product name, else the first camera the
        // traversal met. Another camera than the scene's is a retarget,
        // which stands only for a camera that import met (it would build it
        // when named), carrying no prim the scene imported (the rebuild
        // moves none). Which camera dices, and so whether a retarget moves
        // tessellation, is checked above through the keys.
        let target =
            match &read.wanted_camera {
                Some(choice) => choice.path().clone(),
                None => prim_path(facts.cameras.first().ok_or_else(|| {
                    Structural("the import met no camera to fall back to".into())
                })?)?,
            };
        let retarget = target != rendered;
        if retarget {
            let name = target.to_string();
            if !facts.cameras.contains(&name) {
                return Err(Structural(format!("{name} is not a camera the import met")));
            }
            if prov.any_below(&name) {
                return Err(Structural(format!(
                    "a prim the scene imported lies below {name}"
                )));
            }
        }
        // Geometry was placed at the shutter's ends, so another shutter
        // re-imports, as a transform edit does. A retarget reads it from the
        // new camera: it must open and close as the old one did.
        let shutter = read.shutter.key(
            asks_about_time(&read.shutter, &read.products),
            frame_pose(&read.shutter, &read.products),
        );
        let expected = if retarget {
            crate::live::ShutterKey {
                camera: Some(target.to_string()),
                ..facts.shutter.clone()
            }
        } else {
            facts.shutter.clone()
        };
        if shutter != expected {
            return Err(Structural("the camera shutter changed".into()));
        }
        if read.domes_seen_by_camera != facts.domes_seen_by_camera {
            return Err(Structural("the dome's camera visibility changed".into()));
        }
        // Display filters edited in place are a later change: a precomp,
        // before or after, re-imports.
        if facts.precomp || read.products.request.precomp.is_some() {
            return Err(Structural("the render settings author a precomp".into()));
        }
        // The geometry kept the primvars the products asked for when it was
        // read, and nothing else: asking for another needs it read again.
        if read.products.primvars.key() != facts.primvars {
            return Err(Structural(
                "the products ask for other primvars than the geometry kept".into(),
            ));
        }
        let motion_blur = read.settings.motion_blur() && read.shutter.interval.is_some();
        let camera = target.to_string();
        let camera = camera.as_str();
        let rc = build_camera(
            at.index.as_ref(),
            at.facts,
            &target,
            &read.settings,
            read.products.framing,
        )
        .ok_or_else(|| Structural(format!("{camera} is no longer a camera")))?;
        rc.warn_lens_units(Some(camera));
        rc.warn_clipping(Some(camera));
        let settings = apply_exposure(
            read.settings,
            read.exposure_compensation,
            rc.exposure_scale,
            Some(camera),
        );
        Ok(SettingsUpdate {
            settings,
            camera: rc.camera,
            camera_path: camera.to_owned(),
            shutter,
            aovs: read.products.request,
            lens: rc.lens,
            motion_blur,
        })
    })();
    let raised = scope.finish();
    let update = built?;
    if let Some(w) = raised.first() {
        return Err(Structural(format!(
            "re-reading the settings raised {}",
            w.code.as_str()
        )));
    }
    Ok(update)
}
