//! The delegate's prim factory and the order notices reach it (design D1,
//! D2): a managed prim for every prim of the index, and the prim types'
//! priorities the batching index sends them in — materials, then lights,
//! then light filters, geometry, instancers, cameras and the render prims,
//! hdPrman's intent. The commit does not depend on the order.

use std::rc::Rc;

use moho::filter::prim_type_notice_batching::PrimTypePriority;
use moho::openusd::tf::Token;
use moho::prim_managing::{ManagedPrim, PrimFactory, PrimManagingObserver};
use moho::scene_index::Added;

use super::prims::{CrustPrim, Kind, SharedBatch};

/// Makes a [`CrustPrim`] for every added prim, recording it in the batch.
pub(in crate::scene) struct CrustPrimFactory {
    pub(in crate::scene) batch: SharedBatch,
}

impl PrimFactory for CrustPrimFactory {
    fn create_prim(&self, entry: &Added, _: &PrimManagingObserver) -> Option<Rc<dyn ManagedPrim>> {
        let kind = Kind::of(entry.ty.as_str());
        self.batch
            .borrow_mut()
            .added
            .push((entry.path.clone(), kind));
        Some(Rc::new(CrustPrim::new(
            entry.path.clone(),
            kind,
            self.batch.clone(),
        )))
    }
}

/// D1's priorities.
pub(in crate::scene) struct CrustPriority;

impl PrimTypePriority for CrustPriority {
    fn priority(&self, prim_type: &Token) -> usize {
        match Kind::of(prim_type.as_str()) {
            Kind::Material => 0,
            Kind::AreaLight | Kind::InfiniteLight => 1,
            Kind::LightFilter => 2,
            Kind::Mesh | Kind::Curves | Kind::Sphere | Kind::Implicit | Kind::Volume => 3,
            Kind::Instancer => 4,
            Kind::Camera => 5,
            Kind::RenderSettings | Kind::RenderProduct | Kind::RenderVar | Kind::RenderPass => 6,
            Kind::Other => 7,
        }
    }

    fn num_priorities(&self) -> usize {
        8
    }
}
