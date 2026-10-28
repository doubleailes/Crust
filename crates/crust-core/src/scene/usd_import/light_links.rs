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
use crate::ray::{MASK_CAMERA, MASK_SHADOW, RayMask};
use crate::rt_world::WorldBuilder;
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
pub(super) struct Membership(String);

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
pub(super) fn receiver_categories(index: &dyn SceneIndex, path: &sdf::Path) -> Vec<String> {
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

/// The traversal's record of receivers and linked lights.
#[derive(Default)]
pub(super) struct LightLinks {
    /// Every prim that emitted geometry or a volume, interned.
    paths: HashMap<sdf::Path, u32>,
    /// Each interned prim's link categories ([`receiver_categories`]).
    categories: Vec<Vec<String>>,
    /// `(first geom_id, path)` per emitting prim, in traversal order: the
    /// prim owns every id up to the next entry's.
    runs: Vec<(u32, u32)>,
    /// One past the last geom_id of `runs[k]`, as the prim emitted it: what
    /// the provenance's reverse lookup reads (the links keep reading a run as
    /// ending where the next begins).
    run_ends: Vec<u32>,
    /// `(volume index, path)` per volume-region prim.
    volumes: Vec<(u32, u32)>,
    pending: Vec<Pending>,
}

impl LightLinks {
    /// `(geom_ids, prim path)` per prim that emitted geometry, in traversal
    /// order: the runs [`LightLinks::resolve`] judges receivers by,
    /// kept for [`crate::live::Provenance`].
    pub(super) fn geometry_runs(&self) -> Vec<(std::ops::Range<u32>, String)> {
        let mut by_id: Vec<Option<&sdf::Path>> = vec![None; self.paths.len()];
        for (p, &id) in &self.paths {
            by_id[id as usize] = Some(p);
        }
        self.runs
            .iter()
            .zip(&self.run_ends)
            .filter_map(|(&(first, id), &end)| Some((first..end, by_id[id as usize]?.to_string())))
            .collect()
    }

    /// The path of each prim that emitted a volume region, once each, in
    /// traversal order: kept for [`crate::live::Provenance`].
    pub(super) fn volume_paths(&self) -> Vec<String> {
        let mut by_id: Vec<Option<&sdf::Path>> = vec![None; self.paths.len()];
        for (p, &id) in &self.paths {
            by_id[id as usize] = Some(p);
        }
        let mut seen = vec![false; self.paths.len()];
        self.volumes
            .iter()
            .filter(|&&(_, id)| !std::mem::replace(&mut seen[id as usize], true))
            .filter_map(|&(_, id)| Some(by_id[id as usize]?.to_string()))
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

    /// Records that the prim at `path` of `index` emitted the geometries
    /// `geoms` (ids `[start, end)`) and the volume regions `vols`. Nothing
    /// when it emitted neither.
    pub(super) fn saw(
        &mut self,
        index: &dyn SceneIndex,
        path: &sdf::Path,
        geoms: std::ops::Range<usize>,
        vols: std::ops::Range<usize>,
    ) {
        if geoms.is_empty() && vols.is_empty() {
            return;
        }
        let id = self.intern(path, || receiver_categories(index, path));
        if !geoms.is_empty() {
            self.runs.push((geoms.start as u32, id));
            self.run_ends.push(geoms.end as u32);
        }
        for v in vols {
            self.volumes.push((v as u32, id));
        }
    }

    /// Records a light just added at `index` of the import's light list, if
    /// either of its links restricts it: its category ids, read from the
    /// light at `path` of `scene`.
    pub(super) fn light_added(&mut self, scene: &dyn SceneIndex, path: &sdf::Path, index: usize) {
        let prim = PrimData::at(scene, path);
        let light = link_category(&prim, light_tokens::LIGHT_LINK);
        let shadow = link_category(&prim, light_tokens::SHADOW_LINK);
        if light.is_none() && shadow.is_none() {
            return;
        }
        self.pending.push(Pending {
            path: path.clone(),
            index,
            light,
            shadow,
        });
    }

    /// Installs the links once the last receiver has been traversed. Must
    /// run before the light list's selection and the world's commit.
    ///
    /// Returns where each light the list held went: its index after the
    /// lights that illuminate nothing left the list, or `None` for one of
    /// them ([`index_remap`]).
    pub(super) fn resolve(
        self,
        lights: &mut LightList,
        world: &mut WorldBuilder,
        volumes: &mut [VolumeRegion],
    ) -> Vec<Option<usize>> {
        let before = lights.count();
        if self.pending.is_empty() {
            return index_remap(before, &[]);
        }
        // The interned paths, by id.
        let mut by_id: Vec<Option<&sdf::Path>> = vec![None; self.paths.len()];
        for (p, &id) in &self.paths {
            by_id[id as usize] = Some(p);
        }
        let paths: Vec<&sdf::Path> = by_id.into_iter().map(|p| p.expect("dense ids")).collect();
        let cats: Vec<&[String]> = self.categories.iter().map(Vec::as_slice).collect();
        // How many geometries (and volumes) each path accounts for: the
        // population the shadow bits are allocated by.
        let n_geoms = world.count();
        let mut weight = vec![0usize; paths.len()];
        for (k, &(start, id)) in self.runs.iter().enumerate() {
            let end = self.runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
            weight[id as usize] += (end - start) as usize;
        }
        for &(_, id) in &self.volumes {
            weight[id as usize] += 1;
        }

        // 1. Lights whose `lightLink` includes no receiver leave the list.
        let mut nothing: Vec<usize> = Vec::new();
        let mut kept: Vec<&Pending> = Vec::new();
        for p in &self.pending {
            match &p.light {
                Some(q) if !cats.iter().any(|c| q.is_included(c)) => nothing.push(p.index),
                _ => kept.push(p),
            }
        }
        nothing.sort_unstable_by(|a, b| b.cmp(a));
        for &index in &nothing {
            let path = &self
                .pending
                .iter()
                .find(|p| p.index == index)
                .expect("recorded")
                .path;
            demote(lights, world, index, path);
        }
        // Indices of the lights that stay, after the removals above.
        let remap = index_remap(before, &nothing);
        let shifted = |index: usize| remap[index].expect("a light that stays");

        let n = lights.count();
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
                for (k, &(start, id)) in self.runs.iter().enumerate() {
                    let end = self.runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
                    let class = class_of[id as usize] as u16;
                    for slot in &mut table[start as usize..end as usize] {
                        *slot = class;
                    }
                }
                world.set_light_classes(table);
                for &(v, id) in &self.volumes {
                    volumes[v as usize].light_class = class_of[id as usize] as u16;
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
        if !restricted.is_empty() {
            encode_shadows(
                &cats,
                &weight,
                &self.runs,
                &self.volumes,
                &restricted,
                world,
                volumes,
                &mut links,
            );
            // Which bounce side each restricted light gets (lighting design
            // record, "Shadow linking"): a link twin, or none — a dome's
            // twin would cost a shadow ray on every bounce.
            let twin = crate::config().link_twin;
            for (index, p) in &restricted {
                if !links.restricted[*index] {
                    continue;
                }
                let dome = lights.light(*index).is_dome();
                links.nee_only[*index] = dome || !twin;
                debug!(
                    "{}: {}",
                    p.path,
                    if dome {
                        "a restricted dome is sampled by NEE alone at non-delta vertices"
                    } else if !twin {
                        "sampled by NEE alone at non-delta vertices (CRUST_LINK_TWIN=0)"
                    } else {
                        "MIS-combined with a shadow-linked bounce twin at non-delta vertices"
                    }
                );
            }
            any = true;
        }

        if any {
            lights.set_links(links);
        }
        remap
    }
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
    let class_of = paths
        .iter()
        .map(|path| {
            let mut key = Key(vec![0u64; words]);
            for (bit, q) in queries.iter().enumerate() {
                if q.is_included(path) {
                    key.0[bit / 64] |= 1 << (bit % 64);
                }
            }
            *ids.entry(key.clone()).or_insert_with(|| {
                classes.push(key);
                classes.len() as u32 - 1
            })
        })
        .collect();
    (class_of, classes)
}

/// Design D3. Class 0 — occluders every restricted light is shadowed by —
/// keeps `MASK_SHADOW`; every other shadow caster clears it and carries one
/// class bit (3–30 by population, 31 shared). Unrestricted lights' rays carry
/// `MASK_SHADOW` and every class bit, so they are blocked exactly as before.
#[allow(clippy::too_many_arguments)]
fn encode_shadows(
    paths: &[&[String]],
    weight: &[usize],
    runs: &[(u32, u32)],
    vols: &[(u32, u32)],
    restricted: &[(usize, &Pending)],
    world: &mut WorldBuilder,
    volumes: &mut [VolumeRegion],
    links: &mut RuntimeLinks,
) {
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
    let encode = |mask: RayMask, class: usize, authored: &mut bool| -> RayMask {
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
        match bit_of[class] {
            None => RayMask(base),
            Some(bit) => RayMask((base & !MASK_SHADOW.0) | bit),
        }
    };
    let mut authored = false;
    let n_geoms = world.count();
    for (k, &(start, id)) in runs.iter().enumerate() {
        let end = runs.get(k + 1).map_or(n_geoms as u32, |r| r.0);
        let class = class_of[id as usize] as usize;
        for g in start..end {
            let m = encode(world.mask(g), class, &mut authored);
            world.set_mask(g, m);
        }
    }
    for &(v, id) in vols {
        let region = &mut volumes[v as usize];
        region.mask = encode(region.mask, class_of[id as usize] as usize, &mut authored);
    }
    if authored {
        warning!(
            LightLinkRayMaskRewritten,
            "crust:rayMask bits 3-31 are rewritten by shadow linking, which encodes \
             occluder classes in them"
        );
    }
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
}

/// Takes the light at `index` out of the list: it illuminates nothing. A
/// light at infinity the camera sees becomes a backdrop; an area light keeps
/// its geometry for camera rays only, if the camera saw it.
fn demote(lights: &mut LightList, world: &mut WorldBuilder, index: usize, path: &sdf::Path) {
    let (light, escape_mask) = lights.remove(index);
    let role = if light.at_infinity() {
        if escape_mask.sees(MASK_CAMERA) {
            lights.add_backdrop(light);
            "a camera-only backdrop"
        } else {
            "dropped (hidden from the camera too)"
        }
    } else if let Some(id) = light.geom_id() {
        let mask = world.mask(id) & MASK_CAMERA;
        world.set_mask(id, mask);
        // No longer a light: nothing for a bounce to collect there.
        world.set_transparent_emitter(id, false);
        if mask.sees(MASK_CAMERA) {
            "camera-visible geometry only"
        } else {
            "dropped (hidden from the camera too)"
        }
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
