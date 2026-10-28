//! The delegate's managed prims (design D2): one per prim of the observed
//! index, of the kind its Hydra type names. A managed prim records what
//! happens to it — added, dirtied at locators, released — into the batch its
//! factory shares; the commit reads the index through the observer, in path
//! order ([`super::commit`]).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use moho::locator::LocatorSet;
use moho::openusd::sdf;
use moho::prim_managing::{ManagedPrim, PrimManagingObserver};
use moho::scene_index::Dirtied;
use moho::tokens::prim_type as t;

/// The kinds of prim the delegate manages, by Hydra type family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::scene) enum Kind {
    Mesh,
    Curves,
    Sphere,
    /// `cube`, `cone`, `cylinder`, `capsule`, `plane`: GeomUtil meshes.
    Implicit,
    /// `volume` and its field assets. A geometry prim carrying
    /// `primvars:crust:volume:type` is a volume region too, decided at the
    /// commit, which reads its primvars.
    Volume,
    /// A light with a shape: sphere, rect, disk, cylinder, and a mesh
    /// light. Its emitters are geometry.
    AreaLight,
    /// A light at infinity: distant, dome.
    InfiniteLight,
    LightFilter,
    Material,
    Camera,
    Instancer,
    RenderSettings,
    RenderProduct,
    RenderVar,
    RenderPass,
    /// Any other prim: a transform, a scope, a type the delegate does not
    /// read. Still managed, for the diagnostics it may raise and for what is
    /// below it.
    Other,
}

impl Kind {
    /// The kind of a prim of Hydra type `ty`.
    pub(in crate::scene) fn of(ty: &str) -> Kind {
        match ty {
            t::MESH => Kind::Mesh,
            t::BASIS_CURVES | t::NURBS_CURVES => Kind::Curves,
            t::SPHERE => Kind::Sphere,
            t::CUBE | t::CONE | t::CYLINDER | t::CAPSULE | t::PLANE => Kind::Implicit,
            t::VOLUME | t::OPENVDB_ASSET | t::FIELD3D_ASSET => Kind::Volume,
            t::LIGHT | t::SPHERE_LIGHT | t::RECT_LIGHT | t::DISK_LIGHT | t::CYLINDER_LIGHT => {
                Kind::AreaLight
            }
            t::DISTANT_LIGHT | t::DOME_LIGHT => Kind::InfiniteLight,
            t::LIGHT_FILTER => Kind::LightFilter,
            t::MATERIAL => Kind::Material,
            t::CAMERA => Kind::Camera,
            t::INSTANCER => Kind::Instancer,
            t::RENDER_SETTINGS => Kind::RenderSettings,
            t::RENDER_PRODUCT => Kind::RenderProduct,
            t::RENDER_VAR => Kind::RenderVar,
            t::RENDER_PASS => Kind::RenderPass,
            _ => Kind::Other,
        }
    }

    /// Whether the kind is a light, which the `lights` class adds and
    /// removes.
    pub(in crate::scene) fn is_light(self) -> bool {
        matches!(self, Kind::AreaLight | Kind::InfiniteLight)
    }
}

/// What one update brought: the prims added and removed, by kind, and the
/// locators dirtied, by prim. Recorded by the managed prims and their
/// factory as the observer hands them the batching index's notices; taken
/// by [`super::delegate::HydraScene::update`].
#[derive(Default, Debug)]
pub(in crate::scene) struct Batch {
    pub(in crate::scene) added: Vec<(sdf::Path, Kind)>,
    pub(in crate::scene) removed: Vec<(sdf::Path, Kind)>,
    pub(in crate::scene) dirtied: BTreeMap<sdf::Path, (Kind, LocatorSet)>,
}

impl Batch {
    pub(in crate::scene) fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.dirtied.is_empty()
    }
}

/// The batch shared by the factory and every managed prim.
pub(in crate::scene) type SharedBatch = Rc<RefCell<Batch>>;

/// A managed prim: its path and kind, and the batch it records into.
pub(in crate::scene) struct CrustPrim {
    pub(in crate::scene) path: sdf::Path,
    pub(in crate::scene) kind: Kind,
    batch: SharedBatch,
}

impl CrustPrim {
    pub(in crate::scene) fn new(path: sdf::Path, kind: Kind, batch: SharedBatch) -> Self {
        CrustPrim { path, kind, batch }
    }
}

impl ManagedPrim for CrustPrim {
    /// Records the locators only; the commit reads (design D2).
    fn dirty(&self, entry: &Dirtied, _: &PrimManagingObserver) {
        let mut batch = self.batch.borrow_mut();
        batch
            .dirtied
            .entry(self.path.clone())
            .or_insert_with(|| (self.kind, LocatorSet::new()))
            .1
            .insert_set(&entry.locators);
    }
}

impl Drop for CrustPrim {
    /// Released: the prim was removed, or replaced by a resync.
    fn drop(&mut self) {
        if let Ok(mut batch) = self.batch.try_borrow_mut() {
            batch.removed.push((self.path.clone(), self.kind));
        }
    }
}
