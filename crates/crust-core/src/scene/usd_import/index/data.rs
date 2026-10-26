//! A prim of the scene index as the index readers see it: its Hydra type, and
//! Moho's generic attribute data (`__usdAttributes`, `__usdRelationships`),
//! read with the direct importer's authored-versus-fallback rule.
//!
//! The direct readers took an attribute's value only when a layer authored it
//! or it differed from its schema fallback (`attrs::value_at`): an unauthored
//! attribute resolves to its fallback, so that is "the value when authored".
//! [`PrimData::value`] applies the same rule to the generic data, which
//! carries each value with whether it is authored, so a reader moved onto the
//! index takes the same values as the one it replaces. The decoders are the
//! direct readers' own (`attrs::decode_*`), so the two cannot disagree on
//! which authored types they accept.

use moho::data_source::{ContainerHandle, DataSource, locate};
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::scene_index::{SceneIndex, SceneIndexHandle};
use moho::tokens::usd as generic;

use super::super::attrs::{decode_bool, decode_f32, decode_i32, decode_text, decode_vec3};

/// One prim of a scene index, for an index reader.
#[derive(Clone)]
pub(in crate::scene::usd_import) struct PrimData {
    path: sdf::Path,
    ty: String,
    data: Option<ContainerHandle>,
    /// The index the prim was read from, when the reader may walk to its
    /// ancestors ([`PrimData::in_index`]).
    index: Option<SceneIndexHandle>,
}

impl PrimData {
    /// The prim at `path` in `index`; [`PrimData::is_defined`] says whether
    /// one is there.
    pub(in crate::scene::usd_import) fn at(index: &dyn SceneIndex, path: &sdf::Path) -> Self {
        let prim = index.prim(path);
        PrimData {
            path: path.clone(),
            ty: prim.ty.as_str().to_owned(),
            data: prim.data,
            index: None,
        }
    }

    /// [`PrimData::at`], keeping `index` so the reader can walk to the
    /// prim's ancestors ([`PrimData::parent`]).
    pub(in crate::scene::usd_import) fn in_index(
        index: &SceneIndexHandle,
        path: &sdf::Path,
    ) -> Self {
        PrimData {
            index: Some(index.clone()),
            ..PrimData::at(index.as_ref(), path)
        }
    }

    /// The prim at `path` of a scene index over `stage`, at the default time:
    /// how a unit test reads a prim as the import does.
    #[cfg(test)]
    pub(in crate::scene::usd_import) fn test_at(
        stage: &moho::openusd::usd::Stage,
        path: &sdf::Path,
    ) -> Self {
        let input = super::input_chain(stage.clone(), None, false);
        PrimData::in_index(&input.chain, path)
    }

    /// The prim's parent in the index it was read from with
    /// [`PrimData::in_index`]; `None` at the root or without an index.
    pub(in crate::scene::usd_import) fn parent(&self) -> Option<PrimData> {
        let parent = self.path.parent()?;
        Some(PrimData::in_index(self.index.as_ref()?, &parent))
    }

    /// The path of attribute `name`, for a warning that names it.
    pub(in crate::scene::usd_import) fn attribute_path(&self, name: &str) -> String {
        format!("{}.{name}", self.path)
    }

    pub(in crate::scene::usd_import) fn path(&self) -> &sdf::Path {
        &self.path
    }

    /// Whether a prim is at the path.
    #[allow(dead_code)]
    pub(in crate::scene::usd_import) fn is_defined(&self) -> bool {
        self.data.is_some()
    }

    /// The Hydra prim type (`renderSettings`, `mesh`, …), empty for none.
    pub(in crate::scene::usd_import) fn ty(&self) -> &str {
        &self.ty
    }

    /// The data source at `locator` under the prim.
    pub(in crate::scene::usd_import) fn locate<const N: usize>(
        &self,
        locator: [&str; N],
    ) -> Option<DataSource> {
        locate(self.data.as_ref()?, &Locator::new(locator))
    }

    /// The generic entry of attribute `name`, when the prim has it.
    fn attribute(&self, name: &str) -> Option<ContainerHandle> {
        self.locate([generic::ATTRIBUTES, name])?
            .as_container()
            .cloned()
    }

    /// Whether the prim has attribute `name` (authored, or declared by its
    /// schema).
    pub(in crate::scene::usd_import) fn has_attribute(&self, name: &str) -> bool {
        self.attribute(name).is_some()
    }

    /// Whether a layer authors attribute `name`'s value.
    pub(in crate::scene::usd_import) fn authored(&self, name: &str) -> bool {
        self.attribute(name)
            .and_then(|a| a.get(&generic::AUTHORED.into()))
            .and_then(|d| d.value())
            .is_some_and(|v| matches!(v, sdf::Value::Bool(true)))
    }

    /// Attribute `name`'s value at the index's time when a layer authors it,
    /// else `None` — the direct readers' `value_at`: an unauthored attribute
    /// resolves to its schema fallback, which the readers take as
    /// "unauthored".
    pub(in crate::scene::usd_import) fn value(&self, name: &str) -> Option<sdf::Value> {
        if !self.authored(name) {
            return None;
        }
        self.attribute(name)?.get(&generic::VALUE.into())?.value()
    }

    /// Attribute `name`'s value at the index's time, authored or its schema
    /// fallback — a typed read (`Attribute::get_at`), not `value_at`.
    pub(in crate::scene::usd_import) fn resolved(&self, name: &str) -> Option<sdf::Value> {
        self.attribute(name)?.get(&generic::VALUE.into())?.value()
    }

    /// Attribute `name`'s metadatum `key` (`interpolation`, `elementSize`,
    /// `colorSpace`), when authored.
    pub(in crate::scene::usd_import) fn metadata(
        &self,
        name: &str,
        key: &str,
    ) -> Option<sdf::Value> {
        self.attribute(name)?.get(&key.into())?.value()
    }

    /// Attribute `name`'s value type name (`normal3f[]`, `color3f`), empty
    /// when the prim has no such attribute.
    pub(in crate::scene::usd_import) fn type_name(&self, name: &str) -> String {
        match self
            .attribute(name)
            .and_then(|a| a.get(&generic::TYPE_NAME.into()))
            .and_then(|d| d.value())
        {
            Some(sdf::Value::Token(t)) => t.as_str().to_owned(),
            Some(sdf::Value::String(s)) => s,
            _ => String::new(),
        }
    }

    pub(in crate::scene::usd_import) fn custom_i32_array(&self, name: &str) -> Option<Vec<i32>> {
        self.value(name)
            .and_then(super::super::attrs::decode_i32_array)
    }

    pub(in crate::scene::usd_import) fn custom_f32_array(&self, name: &str) -> Option<Vec<f32>> {
        self.value(name)
            .and_then(super::super::attrs::decode_f32_array)
    }

    /// A colour attribute in the working space: `attrs::custom_color`, its
    /// space from [`color_space`].
    pub(in crate::scene::usd_import) fn custom_color(
        &self,
        name: &str,
        working: crate::color::Space,
    ) -> Option<glam::Vec3A> {
        let rgb = self.value(name).and_then(decode_vec3)?;
        Some(match color_space(self, name) {
            Some(space) => crate::color::convert(rgb, space, working),
            None => rgb,
        })
    }

    pub(in crate::scene::usd_import) fn custom_i32(&self, name: &str) -> Option<i32> {
        self.value(name).and_then(decode_i32)
    }

    pub(in crate::scene::usd_import) fn custom_f32(&self, name: &str) -> Option<f32> {
        self.value(name).and_then(decode_f32)
    }

    pub(in crate::scene::usd_import) fn custom_f64(&self, name: &str) -> Option<f64> {
        self.value(name).and_then(super::super::attrs::decode_f64)
    }

    pub(in crate::scene::usd_import) fn custom_bool(&self, name: &str) -> Option<bool> {
        self.value(name).and_then(decode_bool)
    }

    pub(in crate::scene::usd_import) fn custom_token(&self, name: &str) -> Option<String> {
        self.value(name).and_then(decode_text)
    }

    #[allow(dead_code)]
    pub(in crate::scene::usd_import) fn custom_color3(&self, name: &str) -> Option<glam::Vec3A> {
        self.value(name).and_then(decode_vec3)
    }

    /// The names of the prim's attributes, the schema's declared ones
    /// included, in the order the prim lists them.
    pub(in crate::scene::usd_import) fn attribute_names(&self) -> Vec<String> {
        self.locate([generic::ATTRIBUTES])
            .and_then(|d| d.as_container().map(|c| c.names()))
            .unwrap_or_default()
            .into_iter()
            .map(|n| n.as_str().to_owned())
            .collect()
    }

    /// The forwarded targets of relationship `name` (a target that is itself a
    /// relationship replaced by its targets), empty when it has none.
    pub(in crate::scene::usd_import) fn forwarded_targets(&self, name: &str) -> Vec<sdf::Path> {
        match self
            .locate([generic::RELATIONSHIPS, name, generic::FORWARDED_TARGETS])
            .and_then(|d| d.value())
        {
            Some(sdf::Value::PathVec(targets)) => targets,
            _ => Vec::new(),
        }
    }

    /// Whether the prim has relationship `name`.
    pub(in crate::scene::usd_import) fn has_relationship(&self, name: &str) -> bool {
        self.locate([generic::RELATIONSHIPS, name]).is_some()
    }

    /// The targets of relationship `name`, empty when it has none or the prim
    /// has no such relationship.
    pub(in crate::scene::usd_import) fn targets(&self, name: &str) -> Vec<sdf::Path> {
        match self
            .locate([generic::RELATIONSHIPS, name, generic::TARGETS])
            .and_then(|d| d.value())
        {
            Some(sdf::Value::PathVec(targets)) => targets,
            _ => Vec::new(),
        }
    }
}

/// The stage metadatum `field` from the absolute root's `__usdStageMetadata`
/// (Moho's M3), with whether a layer authors it.
pub(in crate::scene::usd_import) fn stage_metadata(
    index: &dyn SceneIndex,
    field: &str,
) -> Option<(sdf::Value, bool)> {
    let root = PrimData::at(index, &sdf::Path::abs_root());
    let value = root
        .locate([generic::STAGE_METADATA, field, generic::VALUE])?
        .value()?;
    let authored = matches!(
        root.locate([generic::STAGE_METADATA, field, generic::AUTHORED])
            .and_then(|d| d.value()),
        Some(sdf::Value::Bool(true))
    );
    Some((value, authored))
}

/// The colour space attribute `name` of `prim` is authored in, as
/// `attrs::attr_color_space` resolves it on the stage: the attribute's own
/// `colorSpace` metadatum, else the `colorSpace:name` of its prim, else of
/// the nearest ancestor that authors one (walked in the index `prim` was read
/// from, [`PrimData::in_index`]) — through the OCIO config. A name the config
/// does not know is refused with a warning, and is `None` too.
pub(in crate::scene::usd_import) fn color_space(
    prim: &PrimData,
    name: &str,
) -> Option<crate::color::Space> {
    let own = prim
        .metadata(name, generic::COLOR_SPACE)
        .and_then(decode_text)
        .filter(|n| !n.is_empty());
    let named = own.or_else(|| {
        let mut at = Some(PrimData {
            path: prim.path.clone(),
            ty: prim.ty.clone(),
            data: prim.data.clone(),
            index: prim.index.clone(),
        });
        while let Some(p) = at.filter(|p| !p.path.is_abs_root()) {
            let named = p
                .value("colorSpace:name")
                .and_then(decode_text)
                .filter(|n| !n.is_empty());
            if named.is_some() {
                return named;
            }
            at = p.parent();
        }
        None
    })?;
    let space = crate::color::Space::named(&named);
    if space.is_none() {
        let at = prim.attribute_path(name);
        crate::warning!(
            ColorUnknownSpace,
            at = at.as_str(),
            "{at}: colorSpace `{named}` is not defined by the OCIO config — the value is used as \
             authored"
        );
    }
    space
}

/// `crust:rayMask` — `attrs::prim_ray_mask` read from the generic data.
pub(in crate::scene::usd_import) fn ray_mask(prim: &PrimData) -> crate::ray::RayMask {
    prim.custom_i32("crust:rayMask")
        .map(|m| crate::ray::RayMask(m as u32))
        .unwrap_or(crate::ray::MASK_ALL)
}

/// `crust:motion:translate` — `attrs::prim_motion_translate` read from the
/// generic data.
pub(in crate::scene::usd_import) fn motion_translate(prim: &PrimData) -> Option<glam::Vec3> {
    prim.value("crust:motion:translate")
        .and_then(decode_vec3)
        .map(|v| glam::Vec3::new(v.x, v.y, v.z))
}
