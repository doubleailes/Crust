//! A prim's primvars as Hydra gives them (`HdPrimvarsSchema`,
//! `HdPrimvarSchema`): `primvars/<name>/{primvarValue | indexedPrimvarValue,
//! indices, interpolation, role, colorSpace, elementSize}`.
//!
//! UsdImaging overlays the plain attributes it reads as primvars (`points`,
//! `normals`, `widths`, a NURBS curve's `pointWeights`) under the authored
//! `primvars:*` (so `primvars:normals` wins over `normals`), and the
//! flattening inherits constant primvars from ancestors. Crust's renderer
//! attributes on geometry and volumes are constant primvars
//! (`primvars:crust:<name>`, design D5), read here by their primvar name
//! (`crust:rayMask`).

use moho::openusd::sdf;
use moho::tokens::primvars as t;

use super::super::attrs::{decode_f32, decode_i32, decode_i32_array, decode_text};
use super::data::PrimData;

/// One primvar of a prim.
#[derive(Clone, Debug)]
pub(in crate::scene) struct Primvar {
    /// Its values: the authored array (`indexedPrimvarValue`) when it is
    /// indexed, else its value (`primvarValue`).
    pub(in crate::scene) value: sdf::Value,
    /// Its `indices`, when indexed.
    pub(in crate::scene) indices: Option<sdf::Value>,
    /// Its interpolation (`constant`, `uniform`, `vertex`, `varying`,
    /// `faceVarying`), `None` when Hydra gives none.
    pub(in crate::scene) interpolation: Option<String>,
    /// Its role (`color`, `normal`, `point`, `textureCoordinate`, …), empty
    /// for none.
    pub(in crate::scene) role: String,
    /// The colour space a colour is authored in, when it names one.
    pub(in crate::scene) color_space: Option<String>,
    /// Its `elementSize`, given when it is not 1.
    pub(in crate::scene) element_size: Option<i32>,
}

impl Primvar {
    /// The indices as an `int[]`; `Err` for indices of another type, which a
    /// reader refuses rather than reading the values unindexed.
    pub(in crate::scene) fn int_indices(&self) -> Result<Option<Vec<i32>>, ()> {
        match &self.indices {
            None => Ok(None),
            Some(v) => decode_i32_array(v.clone()).map(Some).ok_or(()),
        }
    }

    /// The values with the indices applied, element by element, as a
    /// reader that takes one value per point or per curve needs them: the
    /// value itself when the primvar is not indexed. `None` for indices
    /// that are not an `int[]` or point outside the values, and for an
    /// indexed value that is no numeric array.
    pub(in crate::scene) fn flattened(&self) -> Option<sdf::Value> {
        use sdf::Value as V;
        let Some(indices) = self.int_indices().ok()? else {
            return Some(self.value.clone());
        };
        fn pick<T: Clone>(values: &[T], indices: &[i32]) -> Option<Vec<T>> {
            indices
                .iter()
                .map(|&i| usize::try_from(i).ok().and_then(|i| values.get(i).cloned()))
                .collect()
        }
        let i = indices.as_slice();
        Some(match &self.value {
            V::IntVec(v) => V::IntVec(pick(v, i)?),
            V::HalfVec(v) => V::HalfVec(pick(v, i)?),
            V::FloatVec(v) => V::FloatVec(pick(v, i)?),
            V::DoubleVec(v) => V::DoubleVec(pick(v, i)?),
            V::Vec2hVec(v) => V::Vec2hVec(pick(v, i)?),
            V::Vec2fVec(v) => V::Vec2fVec(pick(v, i)?),
            V::Vec2dVec(v) => V::Vec2dVec(pick(v, i)?),
            V::Vec3hVec(v) => V::Vec3hVec(pick(v, i)?),
            V::Vec3fVec(v) => V::Vec3fVec(pick(v, i)?),
            V::Vec3dVec(v) => V::Vec3dVec(pick(v, i)?),
            V::Vec4hVec(v) => V::Vec4hVec(pick(v, i)?),
            V::Vec4fVec(v) => V::Vec4fVec(pick(v, i)?),
            V::Vec4dVec(v) => V::Vec4dVec(pick(v, i)?),
            _ => return None,
        })
    }

    /// The value as one number.
    pub(in crate::scene) fn f32(&self) -> Option<f32> {
        decode_f32(self.value.clone())
    }

    /// The value as one integer.
    pub(in crate::scene) fn i32(&self) -> Option<i32> {
        decode_i32(self.value.clone())
    }

    /// The value's type as a warning names it, in USD's spelling: its
    /// storage (`float3[]`, `int`) named by its role where the role has a
    /// type of its own (`point3f[]`, `color3f[]`, `normal3f[]`,
    /// `texCoord2f[]`).
    pub(in crate::scene) fn describe(&self) -> String {
        use sdf::Value as V;
        let (base, array) = match &self.value {
            V::Float(_) => ("float", false),
            V::Double(_) => ("double", false),
            V::Int(_) => ("int", false),
            V::Bool(_) => ("bool", false),
            V::Vec2f(_) => ("float2", false),
            V::Vec3f(_) => ("float3", false),
            V::Vec4f(_) => ("float4", false),
            V::Vec3d(_) => ("double3", false),
            V::Token(_) => ("token", false),
            V::String(_) => ("string", false),
            V::FloatVec(_) => ("float", true),
            V::DoubleVec(_) => ("double", true),
            V::IntVec(_) => ("int", true),
            V::BoolVec(_) => ("bool", true),
            V::Vec2fVec(_) => ("float2", true),
            V::Vec3fVec(_) => ("float3", true),
            V::Vec4fVec(_) => ("float4", true),
            V::Vec3dVec(_) => ("double3", true),
            V::TokenVec(_) => ("token", true),
            V::StringVec(_) => ("string", true),
            _ => ("value", false),
        };
        let typed = match (self.role.as_str(), base) {
            ("point", "float3") => "point3f",
            ("normal", "float3") => "normal3f",
            ("vector", "float3") => "vector3f",
            ("color", "float3") => "color3f",
            ("color", "float4") => "color4f",
            ("textureCoordinate", "float2") => "texCoord2f",
            ("textureCoordinate", "float3") => "texCoord3f",
            (_, base) => base,
        };
        if array {
            format!("{typed}[]")
        } else {
            typed.to_owned()
        }
    }
}

impl PrimData {
    /// The primvar `name` (without `primvars:`) Hydra gives the prim, `None`
    /// when it gives no value for it.
    pub(in crate::scene) fn primvar(&self, name: &str) -> Option<Primvar> {
        let get = |field: &str| {
            self.locate([t::PRIMVARS, name, field])
                .and_then(|d| d.value())
        };
        let (value, indices) = match get(t::INDEXED_PRIMVAR_VALUE) {
            Some(value) => (value, get(t::INDICES)),
            None => (get(t::PRIMVAR_VALUE)?, None),
        };
        Some(Primvar {
            value,
            indices,
            interpolation: get(t::INTERPOLATION).and_then(decode_text),
            role: get(t::ROLE).and_then(decode_text).unwrap_or_default(),
            color_space: get(t::COLOR_SPACE)
                .and_then(decode_text)
                .filter(|s| !s.is_empty()),
            element_size: get(t::ELEMENT_SIZE).and_then(decode_i32),
        })
    }
}
