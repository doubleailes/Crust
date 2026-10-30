//! What the delegate asks its host about the stage (design D1, D7): facts
//! that are not scene description, so no scene index carries them, and that
//! only the host, which opened the stage, can answer. The delegate names no
//! stage; the batch host answers from its stage
//! ([`crate::scene::usd_import::stage_facts::StageFacts`], public as
//! [`crate::StageHostFacts`]), and a host with no stage answers nothing
//! ([`NoHostFacts`]).

use std::path::PathBuf;

use moho::openusd::sdf;

/// The transform facts of a prim that Hydra does not carry, for the
/// `xform.*` diagnostics.
#[derive(Clone, Debug, Default)]
pub struct XformFacts {
    /// Whether its type is `Xformable`, as the schema registry says.
    pub xformable: bool,
    /// Its `xformOpOrder` (its fallback, empty, when unauthored).
    pub ops: Vec<String>,
    /// Its type, when it names a type the schema registry does not know.
    pub unknown_type: Option<String>,
    /// Why openusd cannot compose its ops, when it cannot.
    pub error: Option<String>,
}

/// What a host answers the delegate about its stage.
pub trait HostFacts {
    /// The stage's authored `startTimeCode` .. `endTimeCode`.
    fn time_range(&self) -> Option<(f64, f64)>;
    /// The files of every layer the stage composed.
    fn layers(&self) -> Vec<PathBuf>;
    /// The files of the stage's root layer stack.
    fn layer_stack(&self) -> Vec<PathBuf>;
    /// How many prims the strongest layer hides with an `over`.
    fn invisible_overrides(&self) -> Option<u32>;
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
    /// Whether the prim at `path` is an instance whose prototype the stage
    /// does not compose.
    fn instance_without_prototype(&self, path: &sdf::Path) -> bool;
}

/// A host with no stage to ask: every fact absent.
pub struct NoHostFacts;

impl HostFacts for NoHostFacts {
    fn time_range(&self) -> Option<(f64, f64)> {
        None
    }
    fn layers(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn layer_stack(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn invisible_overrides(&self) -> Option<u32> {
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
    fn instance_without_prototype(&self, _: &sdf::Path) -> bool {
        false
    }
}
