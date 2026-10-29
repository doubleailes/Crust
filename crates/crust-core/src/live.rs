//! Where an imported [`Scene`]'s parts came from ([`Provenance`]): what the
//! delegate's in-place updates rebuild against (`scene/hydra/update.rs`),
//! and what a pick names a hit by.
//!
//! **An in-place update gives exactly the scene a fresh commit would.** Every
//! rebuilt part is built by the commit's own code — the same material
//! resolution, the same light emitters, the same settings and camera readers
//! — at the index's time, and anything a rebuild cannot vouch for is refused
//! with [`Structural`], for the delegate to commit afresh:
//!
//! - a material whose swap would move a flag the world or the geometry was
//!   built on (cutout, straight transmission, medium boundary, emission, the
//!   UV or per-face tables, a displacement);
//! - a light whose rebuild would add or drop a light, change how many
//!   geometries it owns, or move it to or from infinity, and a light the
//!   scene's light links took out of the list (it is no longer one);
//! - a settings change that would move the working colour space, the
//!   subdivision level, the dicing camera or what tessellation read from it
//!   (adaptive subdivision, implicit shapes sized on screen), the shutter or
//!   the dome's camera visibility, or that would render through a camera the
//!   commit did not meet or that carries a committed prim;
//! - any rebuild that raises a warning about the rebuilt prim, or a scene
//!   whose warnings already name it: the warnings list must stay the one a
//!   fresh commit reports.

use std::collections::HashMap;
use std::ops::Range;

// Refusals come from the world and the light list too, so the type sits
// below them.
pub use crate::error::Structural;

/// Where an imported scene's parts came from. Built by the USD import in its
/// traversal order; memory in proportion to the material, light and emitting
/// prim counts, nothing per triangle.
#[derive(Clone, Debug, Default)]
pub struct Provenance {
    /// Material prim path → the `World` material slots holding it.
    pub(crate) materials: HashMap<String, MaterialOrigin>,
    /// `(prim, material)`: a prim outside a material that the material's
    /// network reads, through a composition arc or a connection — every
    /// material the import resolved, prototype-scoped ones included.
    pub(crate) material_reads: Vec<(String, String)>,
    /// Light prim path → its place in the light list and its geometry.
    pub(crate) lights: HashMap<String, LightOrigin>,
    /// `(geom_ids, prim path)` per prim that emitted geometry, in traversal
    /// order, so sorted by first id. [`Provenance::prim_of_geometry`] reads a
    /// run as owning every id up to the next run's, as light linking does;
    /// [`Provenance::geometry_of`] reads the ids the prim emitted.
    pub(crate) geometry: Vec<(Range<u32>, String)>,
    /// Every direct mesh placement (a `Mesh` prim or an implicit shape
    /// outside a prototype), sorted by `geom_id`: whether it was baked into
    /// the top level or instances its distinct mesh.
    pub(crate) meshes: Vec<MeshOrigin>,
    /// Every distinct mesh the import interned, by slot (what
    /// [`MeshOrigin::slot`] indexes).
    pub(crate) mesh_slots: Vec<MeshSlotFacts>,
    /// The static analytic spheres, by `geom_id`, sorted: placed in world
    /// space by their prim's transform.
    pub(crate) spheres: Vec<u32>,
    /// The prims that emitted a volume region, in traversal order. Volumes
    /// are not geometry runs, but they are placed by their prim's transform.
    pub(crate) volumes: Vec<String>,
    /// Every light the walk met, in path order, the lights the links took
    /// out of the list included: what the light layer is emitted from again
    /// (`interactive-updates` design D2, D3).
    pub(crate) light_paths: Vec<LightPath>,
    /// Each prototype placed at top level, by its Hydra prototype path (the
    /// key its parts are cached under): what reshaping one of its meshes
    /// rebuilds and places again.
    pub(crate) prototypes: HashMap<String, ProtoFacts>,
    /// The receivers' link record: what the links are judged against again
    /// when a light or a receiver's categories change.
    pub(crate) links: LinkRecord,
    /// Each volume region's mask before the links rewrote it, by region.
    pub(crate) volume_masks: Vec<crate::ray::RayMask>,
    /// What the import decided once for the whole stage.
    pub(crate) stage: StageFacts,
}

/// One prototype as its top-level placements attach it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProtoFacts {
    /// The USD prototype it comes from (`/__Prototype_N`, or the point
    /// instancer's target), which names it in warnings.
    pub(crate) usd: String,
    /// Each placed part: how many slots (`geom_id`s) it takes, its mask, and
    /// whether it holds curves.
    pub(crate) parts: Vec<(u32, crate::ray::RayMask, bool)>,
    /// The distinct meshes (mesh slots) its parts were built from, in order.
    pub(crate) reads: Vec<u32>,
    /// Whether a part comes from a nested instancer or a nested native
    /// instance's prototype.
    pub(crate) nested: bool,
    /// Whether a placement is not the shared parts attached whole and
    /// static: built for its one placement, a part skipped, or moving.
    pub(crate) irregular: bool,
    /// The first `geom_id` of each placement.
    pub(crate) placements: Vec<u32>,
}

/// One light the walk met, in or out of the light list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LightPath {
    pub(crate) path: String,
    /// The `geom_id`s of the geometry it emitted, as the layer numbered it.
    pub(crate) geoms: Range<u32>,
    /// Its link category ids, `lightLink` and `shadowLink`: `None` for a
    /// trivial collection.
    pub(crate) links: (Option<String>, Option<String>),
}

/// What light linking judges receivers by: every prim that emitted geometry
/// or a volume region, interned, with its link categories and the ids it
/// emitted (`scene/hydra/light_links.rs`).
#[derive(Clone, Debug, Default)]
pub(crate) struct LinkRecord {
    /// Every prim that emitted geometry or a volume, interned.
    pub(crate) paths: HashMap<moho::openusd::sdf::Path, u32>,
    /// Each interned prim's link categories.
    pub(crate) categories: Vec<Vec<String>>,
    /// `(first geom_id, path)` per emitting prim, in traversal order: the
    /// prim owns every id up to the next entry's.
    pub(crate) runs: Vec<(u32, u32)>,
    /// One past the last geom_id of `runs[k]`, as the prim emitted it.
    pub(crate) run_ends: Vec<u32>,
    /// `(volume index, path)` per volume-region prim.
    pub(crate) volumes: Vec<(u32, u32)>,
}

/// The slots one material fills.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterialOrigin {
    /// Every `World` slot whose material is this prim's: by `Arc` identity,
    /// every streamed chunk's `Arc` of it included.
    pub slots: Vec<u32>,
    /// Whether a mesh displaced by it: its geometry depends on its inputs.
    pub displaced: bool,
}

/// One direct mesh placement: its geometry, its distinct mesh, and how it
/// was placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeshOrigin {
    pub(crate) geom_id: u32,
    /// Its distinct mesh: an index into [`Provenance::mesh_slots`].
    pub(crate) slot: u32,
    /// Baked into world-space triangles in the top level (the distinct mesh
    /// is placed once), rather than an instance of it.
    pub(crate) baked: bool,
    /// Its transform moves over the shutter.
    pub(crate) moving: bool,
}

/// One distinct mesh: what the import deduplicated placements by, and what
/// decided between baking and instancing them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MeshSlotFacts {
    /// Its content key; `None` for a slot the arena no longer keys.
    pub(crate) key: Option<MeshKeyFacts>,
    /// Its direct placements.
    pub(crate) placements: u32,
    /// Whether a prototype or an instancer placed it too.
    pub(crate) shared: bool,
    /// Whether its triangles were displaced.
    pub(crate) displaced: bool,
}

/// A distinct mesh's content key, its material as a group number: two
/// placements dedupe exactly when their keys are equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MeshKeyFacts {
    pub(crate) geo_hash: u64,
    pub(crate) n_points: usize,
    pub(crate) n_indices: usize,
    pub(crate) n_uvs: Option<usize>,
    pub(crate) smooth: bool,
    pub(crate) material: u32,
}

/// One light's place in the scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightOrigin {
    /// Its index in the light list.
    pub index: usize,
    /// The `geom_id`s of the geometry it emitted (an area light's emitter,
    /// which shares its `Emissive`); empty for a light at infinity.
    pub geoms: Range<u32>,
}

/// The stage-wide decisions an import made, which a settings change must
/// not move for an in-place update to stand.
#[derive(Clone, Debug, Default)]
pub(crate) struct StageFacts {
    /// The host's import options.
    pub(crate) options: crate::UsdImportOptions,
    /// The `RenderSettings` prim the import read, when one.
    pub(crate) render_settings: Option<String>,
    pub(crate) subdiv_level: u32,
    /// What adaptive subdivision read from the dicing camera (its projection,
    /// target and ceiling), as bits; `None` for uniform subdivision.
    pub(crate) adaptive: Option<ScreenKey>,
    pub(crate) domes_seen_by_camera: bool,
    /// Whether the settings authored a precomp: its compiled filters and
    /// kernels are the import's, so an edit while one is authored (or one
    /// that authors one) re-imports.
    pub(crate) precomp: bool,
    pub(crate) working: Option<crate::color::Space>,
    /// The camera rendered through, `None` for the procedural one.
    pub(crate) camera: Option<String>,
    /// Every camera the traversal met, in the order it met them: the cameras
    /// a settings change may retarget the render to, since a fresh import
    /// would build any of them when named, and first the one it falls back
    /// to when none is.
    pub(crate) cameras: Vec<String>,
    /// The camera `crust:dicingCamera` named, when it is one: tessellation
    /// read it rather than the render camera. `None` when the render camera
    /// dices.
    pub(crate) dicing: Option<String>,
    /// The dicing camera's projection an implicit shape took its radial count
    /// from, as bits; `None` when no count came from it.
    pub(crate) on_screen_shapes: Option<ScreenKey>,
    /// Whether an unshared curved implicit shape took the fixed radial count
    /// because no camera was named: naming one then moves geometry.
    pub(crate) unsized_shapes: bool,
    /// The primvars the geometry kept for the render's primvar vars, as
    /// `(name, components)`: a product that asks for others needs geometry
    /// read again.
    pub(crate) primvars: Vec<(String, u8)>,
    /// The custom MaterialX node library the import read from
    /// `PXR_MTLX_PLUGIN_SEARCH_PATHS`: an in-place rebuild resolves against
    /// it rather than reading every file again, and raises none of the
    /// import's `mtlx.library_unreadable` warnings a second time.
    pub(crate) mtlx_library: std::sync::Arc<crust_mtlx::Library>,
    /// The shutter motion was measured over: geometry was placed at its
    /// ends, so another shutter is another scene.
    pub(crate) shutter: ShutterKey,
}

/// The shutter an import measured motion over, as bits: the camera it was
/// read from, its offsets as authored, the interval in time codes (`None`
/// when instantaneous), and whether prims were asked whether their transform
/// varies. The last two decide warnings as well as geometry
/// (`time.motion_needs_frame`, `camera.shutter_instantaneous`), so a change to
/// any part re-imports. `frame_pose` is whether moving placements were posed
/// at the frame (`disableMotionBlur` over a shutter that does not open at
/// the frame), which moves geometry, so turning that flag on or off
/// re-imports too where it matters, and stays an in-place settings edit
/// elsewhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShutterKey {
    pub(crate) camera: Option<String>,
    pub(crate) offsets: [u64; 2],
    pub(crate) interval: Option<[u64; 2]>,
    pub(crate) query: bool,
    pub(crate) frame_pose: bool,
}

/// A value the import's tessellation read from the render camera, as the
/// bits of every number in it: two keys are equal exactly when the
/// tessellation would come out the same, `-0.0` and `NaN` included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScreenKey(pub(crate) Vec<u32>);

impl Provenance {
    /// The material prims the scene uses, and the slots each fills.
    pub fn materials(&self) -> impl Iterator<Item = (&str, &MaterialOrigin)> {
        self.materials.iter().map(|(p, o)| (p.as_str(), o))
    }

    /// The material at `path`, when the scene uses it.
    pub fn material(&self, path: &str) -> Option<&MaterialOrigin> {
        self.materials.get(path)
    }

    /// The materials other than `owner` whose network reads `prim` or a prim
    /// above or below it, through a composition arc or a connection: an edit
    /// of `prim` changes them too, so it cannot be applied to `owner` alone.
    pub fn other_readers<'a>(
        &'a self,
        prim: &'a str,
        owner: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        let related = |a: &str, b: &str| {
            a.strip_prefix(b).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with('/') || rest.starts_with('.')
            })
        };
        self.material_reads
            .iter()
            .filter(move |(read, reader)| {
                reader != owner && (related(prim, read) || related(read, prim))
            })
            .map(|(_, reader)| reader.as_str())
    }

    /// The light at `path`, when the scene has it.
    pub fn light(&self, path: &str) -> Option<&LightOrigin> {
        self.lights.get(path)
    }

    /// The lights the scene has.
    pub fn lights(&self) -> impl Iterator<Item = (&str, &LightOrigin)> {
        self.lights.iter().map(|(p, o)| (p.as_str(), o))
    }

    /// The prim that emitted the geometry `geom_id`.
    pub fn prim_of_geometry(&self, geom_id: u32) -> Option<&str> {
        let i = self
            .geometry
            .partition_point(|(ids, _)| ids.start <= geom_id);
        i.checked_sub(1).map(|i| self.geometry[i].1.as_str())
    }

    /// The geom_ids the prim at `path` and its descendants emitted, in id
    /// order: a native instance's and a PointInstancer's under the instance
    /// or instancer prim, nothing under a prototype's own path. A scan of the
    /// runs, one per emitting prim; `path` matches by whole path components
    /// (`/World/a` is not an ancestor of `/World/ab`).
    pub fn geometry_of(&self, path: &str) -> Vec<Range<u32>> {
        let path = path.trim_end_matches('/');
        self.geometry
            .iter()
            .filter(|(_, prim)| {
                prim.strip_prefix(path)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .map(|(ids, _)| ids.clone())
            .collect()
    }

    /// Whether a prim the scene imported — a material, a light, or a prim
    /// that emitted geometry or a volume region — lies strictly below `path`,
    /// matched by whole path components: a transform edit at `path` then
    /// moves what the scene holds. A scan of every material, light, geometry
    /// run and volume.
    pub fn any_below(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        let below = |prim: &str| {
            prim.strip_prefix(path)
                .is_some_and(|rest| rest.len() > 1 && rest.starts_with('/'))
        };
        self.materials.keys().any(|p| below(p))
            || self.lights.keys().any(|p| below(p))
            || self.geometry.iter().any(|(_, p)| below(p))
            || self.volumes.iter().any(|p| below(p))
    }

    /// The direct mesh placement of `geom_id`, when it is one.
    pub(crate) fn mesh(&self, geom_id: u32) -> Option<&MeshOrigin> {
        self.meshes
            .binary_search_by_key(&geom_id, |m| m.geom_id)
            .ok()
            .map(|i| &self.meshes[i])
    }

    /// Why a transform edit at `path` cannot move what it places in place,
    /// `None` when it can: every geometry at or below `path` is a static
    /// direct mesh placement (a baked one undisplaced), no volume or camera
    /// the render reads lies there, and the scene was imported with no
    /// motion over the shutter. Lights at or below `path` are rebuilt as
    /// light edits ([`Provenance::lights_at`]). The rebuild checks again.
    pub fn movable(&self, path: &str) -> Option<String> {
        let path = path.trim_end_matches('/');
        let at = |prim: &str| {
            prim.strip_prefix(path)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        };
        if self.stage.shutter.interval.is_some() {
            return Some("the scene was imported with motion over the shutter".into());
        }
        if self.geometry_of(path).is_empty() && self.lights_at(path).next().is_none() {
            // A prim the traversal placed nothing from, as far as the runs
            // say: an instancer's prototype, an arc's source, an empty
            // Xform. Not provably inert.
            return Some(format!("nothing the scene placed lies at {path}"));
        }
        if let Some(v) = self.volumes.iter().find(|v| at(v)) {
            return Some(format!("{v} is a volume region"));
        }
        for camera in [&self.stage.camera, &self.stage.dicing]
            .into_iter()
            .flatten()
        {
            if at(camera) {
                return Some(format!("{camera} is a camera the render reads"));
            }
        }
        for ids in self.geometry_of(path) {
            for id in ids {
                let Some(m) = self.mesh(id) else {
                    if self.lights.values().any(|l| l.geoms.contains(&id))
                        || self.spheres.binary_search(&id).is_ok()
                    {
                        continue;
                    }
                    return Some(format!("geometry {id} under {path} is not a placed mesh"));
                };
                if m.moving {
                    return Some(format!("geometry {id} under {path} moves"));
                }
                let slot = self.mesh_slots.get(m.slot as usize);
                if m.baked && slot.is_none_or(|s| s.displaced) {
                    return Some(format!("geometry {id} under {path} is displaced"));
                }
            }
        }
        None
    }

    /// The lights at or below `path`.
    pub fn lights_at<'a>(&'a self, path: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        let path = path.trim_end_matches('/');
        self.lights
            .keys()
            .filter(move |prim| {
                prim.strip_prefix(path)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .map(String::as_str)
    }

    /// Why a points, topology or primvar edit of the mesh prim `path` cannot
    /// be applied in place, `None` when it can: the prim placed exactly one
    /// geometry, baked (its distinct mesh placed once, by nothing else), not
    /// displaced and static. Whether the new mesh would dedupe with another
    /// is the rebuild's to check.
    pub fn reshapable(&self, path: &str) -> Option<String> {
        if self.stage.adaptive.is_some() {
            return Some("subdivision is adaptive".into());
        }
        let runs: Vec<_> = self
            .geometry
            .iter()
            .filter(|(_, prim)| prim == path)
            .map(|(ids, _)| ids.clone())
            .collect();
        let [ids] = runs.as_slice() else {
            return Some(format!("{path} placed no geometry of its own"));
        };
        if ids.len() != 1 {
            return Some(format!("{path} placed {} geometries", ids.len()));
        }
        let Some(m) = self.mesh(ids.start) else {
            return Some(format!("{path} is not a placed mesh"));
        };
        let Some(slot) = self.mesh_slots.get(m.slot as usize) else {
            return Some(format!("{path}'s mesh is unknown"));
        };
        if slot.shared {
            return Some(format!(
                "{path}'s mesh is a prototype's or an instancer's too"
            ));
        }
        // Placed once: baked, as `flush_meshes` decides. Placed more than
        // once by direct placements only: the reshaped placement takes a
        // slot of its own, decided again (`interactive-updates` design D5).
        if slot.placements == 1 && !m.baked {
            return Some(format!("{path}'s mesh is instanced though placed once"));
        }
        if slot.placements > 1 && !crate::config().mesh_bake {
            return Some(format!("{path}'s mesh is shared, and CRUST_MESH_BAKE=0"));
        }
        if slot.displaced || m.moving {
            return Some(format!("{path} is displaced or moving"));
        }
        None
    }

    /// The prototype whose parts hold the prim at `path` — a mesh inside a
    /// native instance's propagated prototype or a point instancer's
    /// prototype copy — by its key.
    pub(crate) fn prototype_of(&self, path: &str) -> Option<&str> {
        self.prototypes
            .keys()
            .find(|key| {
                path.strip_prefix(key.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .map(String::as_str)
    }

    /// The direct placements of the distinct mesh `slot`, by `geom_id`.
    pub(crate) fn placements_of(&self, slot: u32) -> impl Iterator<Item = &MeshOrigin> {
        self.meshes.iter().filter(move |m| m.slot == slot)
    }

    /// Whether `path` is a light whose emitter geometry the world could
    /// replace: one with emitter geometry needs a world that kept its inputs.
    pub fn light_geometry(&self, path: &str) -> bool {
        self.lights.contains_key(path)
    }

    /// The `RenderSettings` prim the import read, when one.
    pub fn render_settings(&self) -> Option<&str> {
        self.stage.render_settings.as_deref()
    }

    /// The camera prim rendered through, `None` for the procedural camera.
    pub fn camera(&self) -> Option<&str> {
        self.stage.camera.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mesh under the camera is below it; a sibling, a prim whose name
    /// only extends the camera's, and the camera itself are not.
    #[test]
    fn any_below_matches_whole_components_strictly_below() {
        let prov = Provenance {
            geometry: vec![
                (0..1, "/World/cam/rig/mesh".into()),
                (1..2, "/World/box".into()),
            ],
            ..Default::default()
        };
        assert!(prov.any_below("/World/cam"));
        assert!(prov.any_below("/World/cam/"));
        assert!(
            !prov.any_below("/World/box"),
            "the prim itself is not below it"
        );
        assert!(
            !prov.any_below("/World/ca"),
            "a name prefix is not an ancestor"
        );
        assert!(!prov.any_below("/World/sibling"));

        let mut lit = Provenance::default();
        lit.lights.insert(
            "/World/cam/key".into(),
            LightOrigin {
                index: 0,
                geoms: 0..0,
            },
        );
        lit.materials
            .insert("/World/other/mtl".into(), MaterialOrigin::default());
        assert!(lit.any_below("/World/cam"));
        assert!(lit.any_below("/World/other"));
        assert!(!lit.any_below("/World/cam/key"));

        let fog = Provenance {
            volumes: vec!["/World/cam/fog".into()],
            ..Default::default()
        };
        assert!(
            fog.any_below("/World/cam"),
            "a volume region is imported too"
        );
        assert!(!fog.any_below("/World/cam/fog"));
    }
}
