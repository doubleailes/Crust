//! What the delegate asks its host about the stage (design D1, D7): facts
//! that are not scene description, so no scene index carries them, and that
//! only the host, which opened the stage, can answer. The delegate's readers
//! name no stage; the batch host answers from its stage
//! ([`super::stage_facts::StageFacts`]), and a host with no stage answers
//! nothing ([`NoHostFacts`]).

use std::path::PathBuf;

use moho::openusd::sdf;

/// The transform facts of a prim that Hydra does not carry, for the
/// `xform.*` diagnostics ([`super::super::xform::xform_diagnostics`]).
#[derive(Clone, Debug, Default)]
pub(in crate::scene) struct XformFacts {
    /// Whether its type is `Xformable`, as the schema registry says.
    pub(in crate::scene) xformable: bool,
    /// Its `xformOpOrder` (its fallback, empty, when unauthored).
    pub(in crate::scene) ops: Vec<String>,
    /// Its type, when it names a type the schema registry does not know.
    pub(in crate::scene) unknown_type: Option<String>,
    /// Why openusd cannot compose its ops, when it cannot.
    pub(in crate::scene) error: Option<String>,
}

/// The host's answers.
pub(in crate::scene) trait HostFacts {
    /// The stage's `metersPerUnit`, when a layer authors it.
    fn meters_per_unit(&self) -> Option<f64>;
    /// Why the prim at `pass` is no `RenderPass`, for the host's error.
    fn not_a_pass(&self, pass: &sdf::Path) -> String;
    /// The `.mtlx` document (and the material in it) the material at
    /// `material` references, which composes nothing without a MaterialX
    /// file-format plugin.
    fn mtlx_reference(&self, material: &sdf::Path) -> Option<(PathBuf, String)>;
    /// The specs the prims at `paths` compose from outside `root`.
    fn foreign_spec_paths(&self, paths: &[sdf::Path], root: &sdf::Path) -> Vec<String>;
    /// The [`XformFacts`] of the prim at `path`, at time code `time`.
    fn xform_facts(&self, path: &sdf::Path, time: Option<f64>) -> XformFacts;
    /// The paths of the prim's `prim_stack` sites.
    fn prim_stack_paths(&self, path: &sdf::Path) -> Vec<String>;
    /// Whether the prim at `path` is an instance whose prototype the stage
    /// does not compose.
    fn instance_without_prototype(&self, path: &sdf::Path) -> bool;
    /// The directory of the layer authoring the value of the attribute at
    /// `attr`, where a relative asset path in it is anchored.
    fn authoring_layer_dir(&self, attr: &sdf::Path) -> Option<PathBuf>;
}

/// A host with no stage to ask: every fact absent.
pub(in crate::scene) struct NoHostFacts;

impl HostFacts for NoHostFacts {
    fn meters_per_unit(&self) -> Option<f64> {
        None
    }
    fn not_a_pass(&self, _: &sdf::Path) -> String {
        "the host has no stage to say why".to_string()
    }
    fn mtlx_reference(&self, _: &sdf::Path) -> Option<(PathBuf, String)> {
        None
    }
    fn foreign_spec_paths(&self, _: &[sdf::Path], _: &sdf::Path) -> Vec<String> {
        Vec::new()
    }
    fn xform_facts(&self, _: &sdf::Path, _: Option<f64>) -> XformFacts {
        XformFacts {
            xformable: true,
            ..XformFacts::default()
        }
    }
    fn prim_stack_paths(&self, _: &sdf::Path) -> Vec<String> {
        Vec::new()
    }
    fn instance_without_prototype(&self, _: &sdf::Path) -> bool {
        false
    }
    fn authoring_layer_dir(&self, _: &sdf::Path) -> Option<PathBuf> {
        None
    }
}
