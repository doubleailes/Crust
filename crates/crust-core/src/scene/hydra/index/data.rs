//! A prim of the scene index as the readers see it: its Hydra type, its
//! data container and the accessors over it (`locate`), UsdImaging's
//! `__usdPrimInfo` (the USD type, applied API schemas, a native instance's
//! prototype), and, for a prototype's prim, the USD prim it is named by
//! (`primOrigin`). A few root-level reads sit beside it: the up axis, the
//! time code (`sceneGlobals/currentFrame`) and the active render settings.

use moho::data_source::{ContainerHandle, DataSource, locate};
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::scene_index::{SceneIndex, SceneIndexHandle};

use super::super::attrs::decode_vec3;

/// One prim of a scene index, for an index reader.
#[derive(Clone)]
pub(in crate::scene) struct PrimData {
    path: sdf::Path,
    ty: String,
    data: Option<ContainerHandle>,
    /// The index the prim was read from, when the reader may walk to its
    /// ancestors ([`PrimData::in_index`]).
    index: Option<SceneIndexHandle>,
    /// For a prim of a prototype the instancing chain re-roots,
    /// `(data root, name root)`: its data are read under Moho's propagated
    /// prototype (or point-instancer copy), and it is named, in warnings and
    /// provenance, under the USD prototype it comes from (`primOrigin`).
    /// The path values its data hold (binding targets, connections) are left
    /// as the index re-rooted them (Moho's M11).
    rebase: Option<(sdf::Path, sdf::Path)>,
}

impl PrimData {
    /// The prim at `path` in `index`; [`PrimData::is_defined`] says whether
    /// one is there.
    pub(in crate::scene) fn at(index: &dyn SceneIndex, path: &sdf::Path) -> Self {
        let prim = index.prim(path);
        PrimData {
            path: path.clone(),
            ty: prim.ty.as_str().to_owned(),
            data: prim.data,
            index: None,
            rebase: None,
        }
    }

    /// The prim at `data` in `index`, under the re-rooted prototype at
    /// `data_root`, named under `name_root`, the USD prototype it comes from
    /// (see the `rebase` field).
    pub(in crate::scene) fn rebased(
        index: &SceneIndexHandle,
        data: &sdf::Path,
        data_root: &sdf::Path,
        name_root: &sdf::Path,
    ) -> Self {
        let path = Self::move_root(data, data_root, name_root).unwrap_or_else(|| data.clone());
        PrimData {
            path,
            rebase: Some((data_root.clone(), name_root.clone())),
            ..PrimData::in_index(index, data)
        }
    }

    /// The name of `path`, a path value the prim's data hold: under the USD
    /// prototype it comes from when it lies under the re-rooted one
    /// ([`PrimData::rebased`]), else itself.
    pub(in crate::scene) fn name_of(&self, path: &sdf::Path) -> sdf::Path {
        self.rebase
            .iter()
            .find_map(|(data_root, name_root)| Self::move_root(path, data_root, name_root))
            .unwrap_or_else(|| path.clone())
    }

    /// `path` moved from under `from` to under `to`, when it lies there.
    fn move_root(path: &sdf::Path, from: &sdf::Path, to: &sdf::Path) -> Option<sdf::Path> {
        if path == from {
            return Some(to.clone());
        }
        let rest = path.as_str().strip_prefix(from.as_str())?;
        if !rest.starts_with('/') {
            return None;
        }
        sdf::Path::new(format!("{to}{rest}").as_str()).ok()
    }

    /// [`PrimData::at`], keeping `index` so the reader can walk to the
    /// prim's ancestors ([`PrimData::parent`]).
    pub(in crate::scene) fn in_index(index: &SceneIndexHandle, path: &sdf::Path) -> Self {
        PrimData {
            index: Some(index.clone()),
            ..PrimData::at(index.as_ref(), path)
        }
    }

    /// The prim at `path` of a scene index over `stage`, at the default time:
    /// how a unit test reads a prim as the import does.
    #[cfg(test)]
    pub(in crate::scene) fn test_at(stage: &moho::openusd::usd::Stage, path: &sdf::Path) -> Self {
        let input = crate::scene::usd_import::input_chain(stage.clone(), None);
        PrimData::in_index(&input.chain, path)
    }

    /// The prim's parent in the index it was read from with
    /// [`PrimData::in_index`]; `None` at the root or without an index.
    pub(in crate::scene) fn parent(&self) -> Option<PrimData> {
        let parent = self.path.parent()?;
        let index = self.index.as_ref()?;
        if let Some((data_root, name_root)) = &self.rebase {
            // Within the prototype, its re-rooted data; above its root, the
            // USD prototype's own ancestors (the pseudo-root), as the stage
            // walks them.
            return Some(match Self::move_root(&parent, name_root, data_root) {
                Some(data) => PrimData::rebased(index, &data, data_root, name_root),
                None => PrimData::in_index(index, &parent),
            });
        }
        Some(PrimData::in_index(index, &parent))
    }

    /// The path of attribute `name`, for a warning that names it.
    pub(in crate::scene) fn attribute_path(&self, name: &str) -> String {
        format!("{}.{name}", self.path)
    }

    pub(in crate::scene) fn path(&self) -> &sdf::Path {
        &self.path
    }

    /// Whether a prim is at the path.
    #[allow(dead_code)]
    pub(in crate::scene) fn is_defined(&self) -> bool {
        self.data.is_some()
    }

    /// The Hydra prim type (`renderSettings`, `mesh`, …), empty for none.
    pub(in crate::scene) fn ty(&self) -> &str {
        &self.ty
    }

    /// The USD prim type name (`Shader`, `Xform`), empty for a typeless prim:
    /// `__usdPrimInfo/typeName` (Hydra's `UsdImagingUsdPrimInfoSchema`,
    /// Moho's M8), for a reader the Hydra type does not tell enough.
    pub(in crate::scene) fn usd_type(&self) -> String {
        use moho::tokens::usd_prim_info as info;
        match self
            .locate([info::USD_PRIM_INFO, info::TYPE_NAME])
            .and_then(|d| d.value())
        {
            Some(sdf::Value::Token(t)) => t.as_str().to_owned(),
            Some(sdf::Value::String(s)) => s,
            _ => String::new(),
        }
    }

    /// Whether the prim's USD type is `schema` or derives from it, as the
    /// schema registry says (`UsdPrim::IsA`): the typed views' `get`.
    pub(in crate::scene) fn is_a(&self, schema: &str) -> bool {
        let ty = self.usd_type();
        !ty.is_empty()
            && moho::openusd_schemas::schema_registry().is_a(
                &moho::openusd::tf::Token::from(ty.as_str()),
                &moho::openusd::tf::Token::from(schema),
            )
    }

    /// The API schemas applied to the prim (`ShapingAPI`, `CollectionAPI:x`):
    /// `__usdPrimInfo/apiSchemas` (Moho's M8).
    pub(in crate::scene) fn api_schemas(&self) -> Vec<String> {
        use moho::tokens::usd_prim_info as info;
        match self
            .locate([info::USD_PRIM_INFO, info::API_SCHEMAS])
            .and_then(|d| d.value())
        {
            Some(sdf::Value::TokenVec(v)) => v.iter().map(|t| t.as_str().to_owned()).collect(),
            Some(sdf::Value::StringVec(v)) => v,
            _ => Vec::new(),
        }
    }

    /// The index the prim was read from with [`PrimData::in_index`], if any.
    pub(in crate::scene) fn index(&self) -> Option<&SceneIndexHandle> {
        self.index.as_ref()
    }

    /// The data source at `locator` under the prim.
    pub(in crate::scene) fn locate<const N: usize>(
        &self,
        locator: [&str; N],
    ) -> Option<DataSource> {
        locate(self.data.as_ref()?, &Locator::new(locator))
    }
}

/// The stage's up axis, `__usdUpAxis/upAxis` at the absolute root (Hydra's
/// `UsdImagingUsdUpAxisSchema`): `Y` or `Z`, `None` when the index has none.
pub(in crate::scene) fn up_axis(index: &dyn SceneIndex) -> Option<String> {
    use moho::tokens::usd_up_axis as t;
    match PrimData::at(index, &sdf::Path::abs_root())
        .locate([t::USD_UP_AXIS, t::UP_AXIS])?
        .value()?
    {
        sdf::Value::Token(axis) => Some(axis.as_str().to_owned()),
        sdf::Value::String(axis) => Some(axis),
        _ => None,
    }
}

/// The time code the host's index is at: `sceneGlobals/currentFrame` at the
/// absolute root, which the host's scene globals filter carries
/// (`HdsiSceneGlobalsSceneIndex::SetCurrentFrame`). `None` when the host set
/// none: the index's default time, time code 0, without `-f`.
pub(in crate::scene) fn current_frame(index: &dyn SceneIndex) -> Option<f64> {
    use moho::tokens::scene_globals as t;
    match PrimData::at(index, &sdf::Path::abs_root())
        .locate([t::SCENE_GLOBALS, t::CURRENT_FRAME])?
        .value()?
    {
        sdf::Value::Double(frame) if frame.is_finite() => Some(frame),
        _ => None,
    }
}

/// The active render settings prim the stage names
/// (`sceneGlobals/activeRenderSettingsPrim`, from `renderSettingsPrimPath`).
pub(in crate::scene) fn active_render_settings(index: &dyn SceneIndex) -> Option<sdf::Path> {
    use moho::tokens::scene_globals as t;
    match PrimData::at(index, &sdf::Path::abs_root())
        .locate([t::SCENE_GLOBALS, t::ACTIVE_RENDER_SETTINGS_PRIM])?
        .value()?
    {
        sdf::Value::PathVec(mut paths) => paths.pop(),
        _ => None,
    }
}

/// A geometry prim's ray mask: its constant primvar
/// `primvars:crust:rayMask`, inherited as Hydra flattens it (design D5),
/// every ray type when it has none.
pub(in crate::scene) fn ray_mask(prim: &PrimData) -> crate::ray::RayMask {
    prim.primvar("crust:rayMask")
        .and_then(|p| p.i32())
        .map(|m| crate::ray::RayMask(m as u32))
        .unwrap_or(crate::ray::MASK_ALL)
}

/// A geometry prim's authored velocity over the shutter: its constant
/// primvar `primvars:crust:motion:translate` (design D5).
pub(in crate::scene) fn motion_translate(prim: &PrimData) -> Option<glam::Vec3> {
    prim.primvar("crust:motion:translate")
        .and_then(|p| decode_vec3(p.value))
        .map(|v| glam::Vec3::new(v.x, v.y, v.z))
}
