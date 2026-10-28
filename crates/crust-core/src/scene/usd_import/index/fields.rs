//! A Hydra container read by field name: a prim's schema container
//! (`renderSettings`, `camera`, `light`), a container nested in one
//! (`namespacedSettings`), or an entry of a vector of containers (a render
//! product of `renderSettings/renderProducts`, a var of its `renderVars`).
//!
//! The readers read Hydra's values as Hydra gives them, fallbacks included
//! (design D4): a field is `None` only when Hydra gives no data there, which
//! reads as its default ("Missing data reads as its default"). The decoders
//! are the readers' own (`attrs::decode_*`), so a value Hydra carries in
//! another numeric type than the reader asks for still reads.

use moho::data_source::{ContainerHandle, DataSource};
use moho::openusd::sdf;

use super::super::attrs::{decode_bool, decode_f32, decode_i32, decode_text};
use super::data::PrimData;

/// A Hydra container, with the prim it belongs to (for warnings).
#[derive(Clone)]
pub(in crate::scene::usd_import) struct Fields {
    path: sdf::Path,
    data: Option<ContainerHandle>,
}

impl Fields {
    /// The container at `locator` of `prim`'s data (none when absent).
    pub(in crate::scene::usd_import) fn of<const N: usize>(
        prim: &PrimData,
        locator: [&str; N],
    ) -> Fields {
        Fields {
            path: prim.path().clone(),
            data: prim.locate(locator).and_then(|d| d.as_container().cloned()),
        }
    }

    /// No data at all: every field reads as absent.
    pub(in crate::scene::usd_import) fn empty(path: &sdf::Path) -> Fields {
        Fields {
            path: path.clone(),
            data: None,
        }
    }

    /// The prim the container belongs to: an entry's own `path` field when
    /// it names one (a render product or var), else the prim read.
    pub(in crate::scene::usd_import) fn path(&self) -> &sdf::Path {
        &self.path
    }

    /// The field `name`'s data source.
    pub(in crate::scene::usd_import) fn get(&self, name: &str) -> Option<DataSource> {
        self.data.as_ref()?.get(&name.into())
    }

    /// The field names, in the container's order.
    pub(in crate::scene::usd_import) fn names(&self) -> Vec<String> {
        self.data
            .as_ref()
            .map(|d| d.names().iter().map(|n| n.as_str().to_owned()).collect())
            .unwrap_or_default()
    }

    /// The field `name`'s value, at the index's time.
    pub(in crate::scene::usd_import) fn value(&self, name: &str) -> Option<sdf::Value> {
        self.get(name)?.value()
    }

    /// The container at field `name`.
    pub(in crate::scene::usd_import) fn child(&self, name: &str) -> Fields {
        Fields {
            path: self.path.clone(),
            data: self.get(name).and_then(|d| d.as_container().cloned()),
        }
    }

    /// The containers of the vector at field `name`, each named by its own
    /// `path` field when it has one.
    pub(in crate::scene::usd_import) fn entries(&self, name: &str) -> Vec<Fields> {
        let Some(vector) = self.get(name).and_then(|d| d.as_vector().cloned()) else {
            return Vec::new();
        };
        (0..vector.len())
            .filter_map(|i| vector.get(i)?.as_container().cloned())
            .map(|data| {
                let mut entry = Fields {
                    path: self.path.clone(),
                    data: Some(data),
                };
                if let Some(path) = entry.paths("path").into_iter().next() {
                    entry.path = path;
                }
                entry
            })
            .collect()
    }

    pub(in crate::scene::usd_import) fn i32(&self, name: &str) -> Option<i32> {
        self.value(name).and_then(decode_i32)
    }

    pub(in crate::scene::usd_import) fn f32(&self, name: &str) -> Option<f32> {
        self.value(name).and_then(decode_f32)
    }

    pub(in crate::scene::usd_import) fn bool(&self, name: &str) -> Option<bool> {
        self.value(name).and_then(decode_bool)
    }

    /// A token or a string.
    pub(in crate::scene::usd_import) fn text(&self, name: &str) -> Option<String> {
        self.value(name).and_then(decode_text)
    }

    /// A path array (Moho's form of a Hydra path or path array).
    pub(in crate::scene::usd_import) fn paths(&self, name: &str) -> Vec<sdf::Path> {
        match self.value(name) {
            Some(sdf::Value::PathVec(v)) => v,
            _ => Vec::new(),
        }
    }

    /// A string or string array holding absolute prim paths, as Crust's path
    /// settings are authored (design D5): each one that parses as a path (the
    /// caller checks what it names). A malformed one is left out.
    pub(in crate::scene::usd_import) fn path_strings(&self, name: &str) -> Vec<sdf::Path> {
        let texts = match self.value(name) {
            Some(sdf::Value::String(s)) => vec![s],
            Some(sdf::Value::Token(t)) => vec![t.as_str().to_owned()],
            Some(sdf::Value::StringVec(v)) => v,
            Some(sdf::Value::TokenVec(v)) => v.iter().map(|t| t.as_str().to_owned()).collect(),
            _ => Vec::new(),
        };
        texts
            .iter()
            .filter_map(|t| sdf::Path::new(t.as_str()).ok())
            .collect()
    }
}
