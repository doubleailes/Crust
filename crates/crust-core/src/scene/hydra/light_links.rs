//! `collection:lightLink` and `collection:shadowLink`: which receivers a
//! light illuminates, and which occluders shadow it.
//!
//! Membership is Hydra's (moho-hydra-parity, design D8): Moho's light
//! linking renderer filter (`HdsiLightLinkingSceneIndex`) judges each
//! light's collections with Hydra's path expression predicates, gives each
//! distinct non-trivial expression a category id (`group_<n>`) at
//! `light/lightLink` and `light/shadowLink`, and gives every receiver the ids
//! of the collections that include it: `categories` on geometry no instancer
//! instances, `instanceCategories` per instance on a native instancer, and
//! `categories` on a point instancer for all its instances. A trivial
//! collection (everything) has the empty id. Crust judges no membership
//! itself; what it adds is *when* the answers are installed. A light can be
//! traversed after the geometry it links, so nothing is decided at emission:
//!
//! - As the traversal dispatches each prim that emits geometry, it records the
//!   first `geom_id` that prim assigned, its path and its categories
//!   ([`LightLinks::saw`]). Ids are handed out in traversal order, so one entry
//!   covers a mesh, a native instance, a whole `PointInstancer` and an area
//!   light's own emitter. Volume regions are recorded the same way.
//! - Each light with a non-trivial link has its category ids read when it is
//!   met ([`LightLinks::light_added`]).
//! - After the walk, [`LightLinks::resolve`] answers each distinct receiver
//!   against every linked light, deduplicates the answers into *classes*, and
//!   installs them: a per-`geom_id` light-class table in the world, a
//!   per-light illuminated-class set, and the shadow-class encoding in the
//!   geometry masks (design D3 of `add-light-and-shadow-linking`).
//!
//! An area light's own emitter is no Hydra geometry, so it carries no
//! category and no non-trivial collection includes it.
//!
//! A light whose `lightLink` includes no receiver at all illuminates nothing
//! and leaves the light list: a camera-visible light at infinity becomes a
//! backdrop (the Moana island's `sky_dome_cam_llc`), an area light keeps only
//! its camera-visible geometry.

use crate::warning;
use moho::openusd;
use std::collections::HashMap;

use moho::scene_index::SceneIndex;
use moho::tokens::{collections, instancing, light as light_tokens};
use openusd::sdf;
use tracing::debug;

use crate::light::{EVERY_CLASS, Light, LightLinks as RuntimeLinks, LightList};
use crate::live::LinkRecord;
use crate::ray::{MASK_CAMERA, MASK_SHADOW, RayMask};
use crate::rt_world::LinkTarget;
use crate::volume::VolumeRegion;

use super::index::data::PrimData;

/// The mask bits that carry shadow classes (design D3): 3–30 for the most
/// populated classes, 31 shared by the rest.
const CLASS_BITS: u32 = !0b111;
/// The shared overflow bit.
const OVERFLOW_BIT: u32 = 1 << 31;
/// How many shadow classes get a bit of their own (3–30).
const ALLOCATED: usize = 28;

/// A non-trivial link collection: its category id (`light/lightLink` or
/// `light/shadowLink`).
pub(in crate::scene) struct Membership(String);

impl Membership {
    /// Whether a receiver carrying `categories` is a member.
    fn is_included(&self, categories: &[String]) -> bool {
        categories.contains(&self.0)
    }
}

/// The category id of the light `prim`'s link collection `name`
/// (`lightLink` / `shadowLink`) at `light/<name>`, `None` when the
/// collection is trivial (the empty id) or the light has none.
fn link_category(prim: &PrimData, name: &str) -> Option<Membership> {
    let id = match prim.locate([light_tokens::LIGHT, name])?.value()? {
        sdf::Value::Token(t) => t.as_str().to_owned(),
        sdf::Value::String(s) => s,
        _ => return None,
    };
    (!id.is_empty()).then_some(Membership(id))
}

/// The link category ids of the light at `path` of `index` as they are now:
/// what [`LightLinks::light_added`] records.
pub(in crate::scene) fn link_ids(index: &dyn SceneIndex, path: &sdf::Path) -> LinkIds {
    let prim = PrimData::at(index, path);
    (
        link_category(&prim, light_tokens::LIGHT_LINK).map(|m| m.0),
        link_category(&prim, light_tokens::SHADOW_LINK).map(|m| m.0),
    )
}

/// The names of the `categories` container `source`.
fn names(source: Option<moho::data_source::DataSource>) -> Vec<String> {
    source
        .and_then(|d| d.as_container().map(|c| c.names()))
        .unwrap_or_default()
        .iter()
        .map(|t| t.as_str().to_owned())
        .collect()
}

/// The link categories of the receiver at `path` in `index`, Hydra's: a
/// native instance's from its instancer's `instanceCategories` at its
/// instance location, any other prim's own `categories` (geometry, a point
/// instancer). Empty when no collection includes it.
pub(in crate::scene) fn receiver_categories(
    index: &dyn SceneIndex,
    path: &sdf::Path,
) -> Vec<String> {
    let prim = PrimData::at(index, path);
    let instancer = match prim
        .locate([instancing::INSTANCE, instancing::INSTANCER])
        .and_then(|d| d.value())
    {
        Some(sdf::Value::PathVec(mut v)) => v.pop(),
        _ => None,
    };
    let Some(instancer) = instancer else {
        return names(prim.locate([collections::CATEGORIES]));
    };
    let instancer = PrimData::at(index, &instancer);
    let locations = match instancer
        .locate([
            instancing::INSTANCER_TOPOLOGY,
            instancing::INSTANCE_LOCATIONS,
        ])
        .and_then(|d| d.value())
    {
        Some(sdf::Value::PathVec(v)) => v,
        _ => Vec::new(),
    };
    // A native instancer has one prototype, so an instance's index is its
    // place among the instance locations.
    let hinted = match prim
        .locate([instancing::INSTANCE, instancing::INSTANCE_INDEX])
        .and_then(|d| d.value())
    {
        Some(sdf::Value::Int(i)) => usize::try_from(i).ok(),
        _ => None,
    };
    let Some(at) = hinted
        .filter(|&i| locations.get(i) == Some(path))
        .or_else(|| locations.iter().position(|l| l == path))
    else {
        return Vec::new();
    };
    names(
        instancer
            .locate([
                collections::INSTANCE_CATEGORIES,
                collections::CATEGORIES_VALUES,
            ])
            .and_then(|d| d.as_vector().cloned())
            .and_then(|v| v.get(at)),
    )
}

/// A light with a restricting link, waiting for the last receiver.
struct Pending {
    path: sdf::Path,
    /// Its place in the import's [`LightList`] when it was added.
    index: usize,
    light: Option<Membership>,
    shadow: Option<Membership>,
}

/// A light's link category ids, `light/lightLink` and `light/shadowLink`:
/// `None` for a trivial collection.
pub(in crate::scene) type LinkIds = (Option<String>, Option<String>);

/// The record of one walk: the receivers (or, for the light layer, the
/// emitters) seen and the lights whose links restrict them.
#[derive(Default)]
pub(in crate::scene) struct LightLinks {
    pub(in crate::scene) record: LinkRecord,
    pending: Vec<Pending>,
}

impl LinkRecord {
    /// The interned paths, by id.
    fn by_id(&self) -> Vec<&sdf::Path> {
        let mut by_id: Vec<Option<&sdf::Path>> = vec![None; self.paths.len()];
        for (p, &id) in &self.paths {
            by_id[id as usize] = Some(p);
        }
        by_id.into_iter().flatten().collect()
    }

    /// `(geom_ids, prim path)` per prim that emitted geometry, in traversal
    /// order: the runs [`judge`] judges receivers by, kept for
    /// [`crate::live::Provenance`].
    pub(in crate::scene) fn geometry_runs(&self) -> Vec<(std::ops::Range<u32>, String)> {
        let by_id = self.by_id();
        self.runs
            .iter()
            .zip(&self.run_ends)
            .filter_map(|(&(first, id), &end)| {
                Some((first..end, by_id.get(id as usize)?.to_string()))
            })
            .collect()
    }

    /// The path of each prim that emitted a volume region, once each, in
    /// traversal order: kept for [`crate::live::Provenance`].
    pub(in crate::scene) fn volume_paths(&self) -> Vec<String> {
        let by_id = self.by_id();
        let mut seen = vec![false; self.paths.len()];
        self.volumes
            .iter()
            .filter(|&&(_, id)| !std::mem::replace(&mut seen[id as usize], true))
            .filter_map(|&(_, id)| Some(by_id.get(id as usize)?.to_string()))
            .collect()
    }

    fn intern(&mut self, path: &sdf::Path, categories: impl FnOnce() -> Vec<String>) -> u32 {
        if let Some(&id) = self.paths.get(path) {
            return id;
        }
        let id = self.paths.len() as u32;
        self.paths.insert(path.clone(), id);
        self.categories.push(categories());
        id
    }

    /// Reads again the categories of the interned prims at `paths` from
    /// `index` ([`receiver_categories`], as the walk read them): what a
    /// receiver's `categories` or an instancer's `instanceCategories`
    /// dirtied changes. A path the record does not hold emitted nothing.
    pub(in crate::scene) fn reread(&mut self, index: &dyn SceneIndex, paths: &[sdf::Path]) {
        for path in paths {
            if let Some(&id) = self.paths.get(path) {
                self.categories[id as usize] = receiver_categories(index, path);
            }
        }
    }
}

impl LightLinks {
    /// Records that the prim at `path` of `index` emitted the geometries
    /// `geoms` (ids `[start, end)`) and the volume regions `vols`. Nothing
    /// when it emitted neither.
    pub(in crate::scene) fn saw(
        &mut self,
        index: &dyn SceneIndex,
        path: &sdf::Path,
        geoms: std::ops::Range<usize>,
        vols: std::ops::Range<usize>,
    ) {
        if geoms.is_empty() && vols.is_empty() {
            return;
        }
        let record = &mut self.record;
        let id = record.intern(path, || receiver_categories(index, path));
        if !geoms.is_empty() {
            record.runs.push((geoms.start as u32, id));
            record.run_ends.push(geoms.end as u32);
        }
        for v in vols {
            record.volumes.push((v as u32, id));
        }
    }

    /// Records a light just added at `index` of the import's light list, if
    /// either of its links restricts it: its category ids, read from the
    /// light at `path` of `scene`, which it returns.
    pub(in crate::scene) fn light_added(
        &mut self,
        scene: &dyn SceneIndex,
        path: &sdf::Path,
        index: usize,
    ) -> LinkIds {
        let prim = PrimData::at(scene, path);
        let light = link_category(&prim, light_tokens::LIGHT_LINK);
        let shadow = link_category(&prim, light_tokens::SHADOW_LINK);
        let ids = (
            light.as_ref().map(|m| m.0.clone()),
            shadow.as_ref().map(|m| m.0.clone()),
        );
        if light.is_some() || shadow.is_some() {
            self.pending.push(Pending {
                path: path.clone(),
                index,
                light,
                shadow,
            });
        }
        ids
    }
}

/// What [`judge`] decided, for [`install`] to put into a world, its light
/// list and its volume regions.
pub(in crate::scene) struct LinkTables {
    /// The lights that illuminate nothing, by their index before any left,
    /// last first.
    nothing: Vec<(usize, sdf::Path)>,
    /// Where each light of the list goes ([`index_remap`]).
    remap: Vec<Option<usize>>,
    /// The light list's links, when any light keeps a restricted one.
    runtime: Option<RuntimeLinks>,
    /// The light-link class of every geometry, when light links classed the
    /// receivers.
    classes: Option<Vec<u16>>,
    /// Every geometry whose mask the links make other than unlinked, with
    /// that mask, ascending: demoted emitters and shadow classes.
    masks: Vec<(u32, RayMask)>,
    /// The emitters of demoted lights: no longer transparent emitters.
    demoted: Vec<u32>,
    /// Each volume region's light-link class and mask, by region.
    volumes: Vec<(u16, RayMask)>,
}

struct ShadowBits {
    runs: Vec<(std::ops::Range<u32>, Option<u32>)>,
    volumes: Vec<(u32, Option<u32>)>,
}

/// Judges the links once the last receiver and the last light have been
/// recorded: `receivers` the walk's record, `layer` the light layer's (its
/// emitters and its lights), `lights` the list the layer built, `n_geoms`
/// one past the last geometry id, `unlinked` each geometry's mask and
/// `unlinked_volumes` each volume region's before any link. Pure but for its
/// warnings: the same records give the same tables, at the commit or at an
/// in-place update, and [`install`] puts them in.
pub(in crate::scene) fn judge(
    receivers: &LinkRecord,
    layer: &LightLinks,
    lights: &LightList,
    n_geoms: usize,
    unlinked: &dyn Fn(u32) -> RayMask,
    unlinked_volumes: &[RayMask],
) -> LinkTables {
    let before = lights.count();
    let mut tables = LinkTables {
        nothing: Vec::new(),
        remap: index_remap(before, &[]),
        runtime: None,
        classes: None,
        masks: Vec::new(),
        demoted: Vec::new(),
        volumes: unlinked_volumes.iter().map(|&m| (EVERY_CLASS, m)).collect(),
    };
    if layer.pending.is_empty() {
        return tables;
    }
    // The receivers' and the layer's records as one: the layer's prims
    // interned after the receivers', its runs after theirs.
    let shift = receivers.paths.len() as u32;
    let cats: Vec<&[String]> = receivers
        .categories
        .iter()
        .chain(&layer.record.categories)
        .map(Vec::as_slice)
        .collect();
    let runs: Vec<(u32, u32)> = receivers
        .runs
        .iter()
        .copied()
        .chain(
            layer
                .record
                .runs
                .iter()
                .map(|&(start, id)| (start, id + shift)),
        )
        .collect();
    let vols: Vec<(u32, u32)> = receivers
        .volumes
        .iter()
        .copied()
        .chain(layer.record.volumes.iter().map(|&(v, id)| (v, id + shift)))
        .collect();
    let run_end = |k: usize| runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
    // How many geometries (and volumes) each path accounts for: the
    // population the shadow bits are allocated by.
    let mut weight = vec![0usize; cats.len()];
    for (k, &(start, id)) in runs.iter().enumerate() {
        weight[id as usize] += (run_end(k) - start) as usize;
    }
    for &(_, id) in &vols {
        weight[id as usize] += 1;
    }

    // 1. Lights whose `lightLink` includes no receiver leave the list.
    let mut nothing: Vec<usize> = Vec::new();
    let mut kept: Vec<&Pending> = Vec::new();
    for p in &layer.pending {
        match &p.light {
            Some(q) if !cats.iter().any(|c| q.is_included(c)) => nothing.push(p.index),
            _ => kept.push(p),
        }
    }
    nothing.sort_unstable_by(|a, b| b.cmp(a));
    tables.nothing = nothing
        .iter()
        .map(|&index| {
            let path = &layer
                .pending
                .iter()
                .find(|p| p.index == index)
                .expect("recorded")
                .path;
            (index, path.clone())
        })
        .collect();
    // Indices of the lights that stay, after the removals above.
    let remap = index_remap(before, &nothing);
    let shifted = |index: usize| remap[index].expect("a light that stays");

    let n = before - nothing.len();
    let mut links = RuntimeLinks {
        illuminates: vec![None; n],
        shadow_masks: vec![MASK_SHADOW; n],
        restricted: vec![false; n],
        nee_only: vec![false; n],
    };
    let mut any = false;

    // 2. Light-link classes: receivers with the same answer for every
    //    linked light share one.
    let linked: Vec<(usize, &Pending)> = kept
        .iter()
        .filter(|p| p.light.is_some())
        .map(|p| (shifted(p.index), *p))
        .collect();
    if !linked.is_empty() {
        let (class_of, classes) = classify(
            &cats,
            linked
                .iter()
                .map(|(_, p)| p.light.as_ref().expect("filtered")),
        );
        if classes.len() >= EVERY_CLASS as usize {
            warning!(
                LightLinkTooManyClasses,
                "{} light-link classes exceed crust's {}; light links are ignored",
                classes.len(),
                EVERY_CLASS
            );
        } else {
            let mut table = vec![EVERY_CLASS; n_geoms];
            for (k, &(start, id)) in runs.iter().enumerate() {
                let class = class_of[id as usize] as u16;
                for slot in &mut table[start as usize..run_end(k) as usize] {
                    *slot = class;
                }
            }
            tables.classes = Some(table);
            for &(v, id) in &vols {
                tables.volumes[v as usize].0 = class_of[id as usize] as u16;
            }
            for (bit, (index, p)) in linked.iter().enumerate() {
                let mut set = vec![0u64; classes.len().div_ceil(64)];
                let mut members = 0;
                for (c, key) in classes.iter().enumerate() {
                    if key.get(bit) {
                        set[c / 64] |= 1 << (c % 64);
                        members += 1;
                    }
                }
                debug!(
                    "{}: collection:lightLink illuminates {members} of {} receiver \
                     class(es)",
                    p.path,
                    classes.len()
                );
                links.illuminates[*index] = Some(set.into_boxed_slice());
            }
            any = true;
        }
    }

    // 3. Shadow classes, encoded in the masks' free bits (design D3).
    let restricted: Vec<(usize, &Pending)> = kept
        .iter()
        .filter(|p| p.shadow.is_some())
        .map(|p| (shifted(p.index), *p))
        .collect();
    let mut shadow = None;
    if !restricted.is_empty() {
        shadow = Some(encode_shadows(
            &cats,
            &weight,
            &runs,
            run_end,
            &vols,
            &restricted,
            &mut links,
        ));
        // Which bounce side each restricted light gets (lighting design
        // record, "Shadow linking"): its link twin, domes included, or
        // NEE alone under `CRUST_LINK_TWIN=0`.
        let twin = crate::config().link_twin;
        for (index, p) in &restricted {
            if !links.restricted[*index] {
                continue;
            }
            links.nee_only[*index] = !twin;
            debug!(
                "{}: {}",
                p.path,
                if twin {
                    "MIS-combined with a shadow-linked bounce twin at non-delta vertices"
                } else {
                    "sampled by NEE alone at non-delta vertices (CRUST_LINK_TWIN=0)"
                }
            );
        }
        any = true;
    }

    // The masks: a demoted area light keeps its camera-visible geometry
    // only, then every shadow caster takes its class bit.
    let mut masks: std::collections::BTreeMap<u32, RayMask> = Default::default();
    for &index in &nothing {
        let light = lights.light(index);
        if !light.at_infinity()
            && let Some(id) = light.geom_id()
        {
            masks.insert(id, unlinked(id) & MASK_CAMERA);
            tables.demoted.push(id);
        }
    }
    let mut authored = false;
    if let Some(shadow) = &shadow {
        for (ids, bit) in &shadow.runs {
            for g in ids.clone() {
                let was = unlinked(g);
                let m = encode(masks.get(&g).copied().unwrap_or(was), *bit, &mut authored);
                if m == was {
                    masks.remove(&g);
                } else {
                    masks.insert(g, m);
                }
            }
        }
        for &(v, bit) in &shadow.volumes {
            let region = &mut tables.volumes[v as usize].1;
            *region = encode(*region, bit, &mut authored);
        }
    }
    if authored {
        warning!(
            LightLinkRayMaskRewritten,
            "crust:rayMask bits 3-31 are rewritten by shadow linking, which encodes \
             occluder classes in them"
        );
    }
    tables.masks = masks.into_iter().collect();
    if any {
        tables.runtime = Some(links);
    }
    tables.remap = remap;
    tables
}

/// Installs what [`judge`] decided, into `target` (the commit's builder, or
/// a committed world whose light layer was just replaced), `lights` (the
/// list the layer built, before any light left it) and `volumes`: the
/// lights that illuminate nothing leave the list (a camera-visible light at
/// infinity becoming a backdrop, an area light keeping its camera-visible
/// geometry), every geometry gets its light-link class and its mask, every
/// region its class and mask, and the list its links. Returns where each
/// light went.
pub(in crate::scene) fn install(
    tables: LinkTables,
    target: &mut dyn LinkTarget,
    lights: &mut LightList,
    volumes: &mut [VolumeRegion],
) -> Vec<Option<usize>> {
    for (index, path) in &tables.nothing {
        demote(lights, *index, path);
    }
    for &id in &tables.demoted {
        // No longer a light: nothing for a bounce to collect there.
        target.set_transparent_emitter(id, false);
    }
    target.set_linked_masks(tables.masks);
    target.set_light_classes(tables.classes.unwrap_or_default());
    for (region, &(class, mask)) in volumes.iter_mut().zip(&tables.volumes) {
        region.light_class = class;
        region.mask = mask;
    }
    if let Some(links) = tables.runtime {
        lights.set_links(links);
    }
    tables.remap
}

/// Where each of `n` lights goes when the ones at `removed` leave the list:
/// its index among those that stay, or `None` for a removed one.
fn index_remap(n: usize, removed: &[usize]) -> Vec<Option<usize>> {
    let mut gone = 0;
    (0..n)
        .map(|i| {
            if removed.contains(&i) {
                gone += 1;
                None
            } else {
                Some(i - gone)
            }
        })
        .collect()
}

/// A bit vector over the linked lights: which of them a receiver is a
/// member of.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key(Vec<u64>);

impl Key {
    fn get(&self, bit: usize) -> bool {
        self.0[bit / 64] & (1 << (bit % 64)) != 0
    }
}

/// Each path's class and the classes' keys, over the given queries.
fn classify<'q>(
    paths: &[&[String]],
    queries: impl Iterator<Item = &'q Membership>,
) -> (Vec<u32>, Vec<Key>) {
    let queries: Vec<&Membership> = queries.collect();
    let words = queries.len().div_ceil(64);
    let mut ids: HashMap<Key, u32> = HashMap::new();
    let mut classes: Vec<Key> = Vec::new();
    // Most receivers carry no category (no non-trivial collection includes
    // them): their key is every bit clear, found once rather than hashed per
    // receiver (the Moana island's light layer rebuilt in a third of the time).
    let mut uncategorised: Option<u32> = None;
    let class_of = paths
        .iter()
        .map(|path| {
            if path.is_empty()
                && let Some(class) = uncategorised
            {
                return class;
            }
            let mut key = Key(vec![0u64; words]);
            for (bit, q) in queries.iter().enumerate() {
                if q.is_included(path) {
                    key.0[bit / 64] |= 1 << (bit % 64);
                }
            }
            let class = *ids.entry(key.clone()).or_insert_with(|| {
                classes.push(key);
                classes.len() as u32 - 1
            });
            if path.is_empty() {
                uncategorised = Some(class);
            }
            class
        })
        .collect();
    (class_of, classes)
}

/// The mask a geometry or volume region of `mask` gets from its shadow
/// class `bit` (design D3): `None` for class 0, which keeps `MASK_SHADOW`;
/// otherwise a shadow caster clears it and carries the class bit. Bits 3–31
/// authored on `mask` are rewritten (`authored` set).
fn encode(mask: RayMask, bit: Option<u32>, authored: &mut bool) -> RayMask {
    let high = mask.0 & CLASS_BITS;
    if high != 0 && high != CLASS_BITS {
        *authored = true;
    }
    let base = mask.0 & !CLASS_BITS;
    if base & MASK_SHADOW.0 == 0 {
        // Not a shadow caster: no class bit, so no shadow ray matches it.
        // Every hidden light source is one (`light_ray_mask`): a class
        // bit would let a restricted light's shadow rays see it again.
        return RayMask(base);
    }
    match bit {
        None => RayMask(base),
        Some(bit) => RayMask((base & !MASK_SHADOW.0) | bit),
    }
}

/// Design D3. Class 0 — occluders every restricted light is shadowed by —
/// keeps `MASK_SHADOW`; every other shadow caster clears it and carries one
/// class bit (3–30 by population, 31 shared). Unrestricted lights' rays carry
/// `MASK_SHADOW` and every class bit, so they are blocked exactly as before.
/// Returns each run's and volume region's bit, for [`install`].
#[allow(clippy::too_many_arguments)]
fn encode_shadows(
    paths: &[&[String]],
    weight: &[usize],
    runs: &[(u32, u32)],
    run_end: impl Fn(usize) -> u32,
    vols: &[(u32, u32)],
    restricted: &[(usize, &Pending)],
    links: &mut RuntimeLinks,
) -> ShadowBits {
    let (class_of, classes) = classify(
        paths,
        restricted
            .iter()
            .map(|(_, p)| p.shadow.as_ref().expect("filtered")),
    );
    let full = |key: &Key| (0..restricted.len()).all(|b| key.get(b));
    // Population per class, then the bit each gets.
    let mut population = vec![0usize; classes.len()];
    for (path, &c) in class_of.iter().enumerate() {
        population[c as usize] += weight[path];
    }
    let mut order: Vec<usize> = (0..classes.len()).filter(|&c| !full(&classes[c])).collect();
    order.sort_by_key(|&c| std::cmp::Reverse(population[c]));
    // `None` for class 0 (keeps MASK_SHADOW), else the class's bit.
    let mut bit_of: Vec<Option<u32>> = vec![None; classes.len()];
    for (rank, &c) in order.iter().enumerate() {
        bit_of[c] = Some(if rank < ALLOCATED {
            1 << (3 + rank)
        } else {
            OVERFLOW_BIT
        });
    }
    let bits = ShadowBits {
        runs: runs
            .iter()
            .enumerate()
            .map(|(k, &(start, id))| (start..run_end(k), bit_of[class_of[id as usize] as usize]))
            .collect(),
        volumes: vols
            .iter()
            .map(|&(v, id)| (v, bit_of[class_of[id as usize] as usize]))
            .collect(),
    };
    let overflow: Vec<usize> = order.iter().skip(ALLOCATED).copied().collect();
    // Unrestricted lights: blocked by every caster, whatever its class.
    for m in links.shadow_masks.iter_mut() {
        *m = RayMask(MASK_SHADOW.0 | CLASS_BITS);
    }
    for (bit, (index, p)) in restricted.iter().enumerate() {
        if classes.iter().all(|k| k.get(bit)) {
            // Shadowed by everything after all: an ordinary light, MIS kept.
            debug!("{}: collection:shadowLink includes every occluder", p.path);
            continue;
        }
        let in_overflow = overflow.iter().filter(|&&c| classes[c].get(bit)).count();
        if in_overflow != 0 && in_overflow != overflow.len() {
            warning!(
                LightLinkShadowLinkUnencodable,
                at = p.path,
                "{}: collection:shadowLink cannot be encoded ({} occluder classes share \
                 crust's overflow bit and it includes only some) — shadowed by every \
                 occluder",
                p.path,
                overflow.len()
            );
            continue;
        }
        let mut mask = MASK_SHADOW.0;
        for (c, b) in bit_of.iter().enumerate() {
            if let Some(b) = b
                && *b != OVERFLOW_BIT
                && classes[c].get(bit)
            {
                mask |= b;
            }
        }
        if in_overflow != 0 {
            mask |= OVERFLOW_BIT;
        }
        let excluded = order.iter().filter(|&&c| !classes[c].get(bit)).count();
        debug!(
            "{}: collection:shadowLink ignores {excluded} of {} occluder class(es)",
            p.path,
            classes.len()
        );
        links.shadow_masks[*index] = RayMask(mask);
        links.restricted[*index] = true;
    }
    bits
}

/// Takes the light at `index` out of the list: it illuminates nothing. A
/// light at infinity the camera sees becomes a backdrop; an area light keeps
/// its geometry for camera rays only, if the camera saw it ([`judge`] masks
/// it).
fn demote(lights: &mut LightList, index: usize, path: &sdf::Path) {
    let (light, escape_mask) = lights.remove(index);
    let role = if light.at_infinity() {
        if escape_mask.sees(MASK_CAMERA) {
            lights.add_backdrop(light);
            "a camera-only backdrop"
        } else {
            "dropped (hidden from the camera too)"
        }
    } else if light.geom_id().is_some() {
        "camera-visible geometry only, if the camera sees it"
    } else {
        "dropped"
    };
    debug!("{path}: collection:lightLink includes no receiver — illuminates nothing, {role}");
}

#[cfg(test)]
mod tests {
    use super::index_remap;

    #[test]
    fn the_remap_shifts_each_light_past_the_removed_ones_before_it() {
        // Removed: 1 (before the kept 2) and 3 (after it), in either order.
        for removed in [[1, 3], [3, 1]] {
            assert_eq!(
                index_remap(5, &removed),
                [Some(0), None, Some(1), None, Some(2)]
            );
        }
        assert_eq!(index_remap(3, &[]), [Some(0), Some(1), Some(2)]);
    }
}
