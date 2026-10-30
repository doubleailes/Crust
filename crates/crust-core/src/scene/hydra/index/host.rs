//! What the delegate asks its host about the stage: one thing, the `.mtlx`
//! document a material references (`host-facts-mtlx-only`). openusd has no
//! MaterialX file format, so such a reference composes nothing into the
//! index and only the host, which opened the stage, can say what it names;
//! `mtlx-format-plugin` will compose it and retire this question. Everything
//! else the delegate reads, warns about or reports comes from the scene
//! index. The batch host answers from its stage
//! ([`crate::scene::usd_import::stage_facts::StageFacts`], public as
//! [`crate::StageHostFacts`]); a host with no stage answers nothing
//! ([`NoHostFacts`]).

use std::path::PathBuf;

use moho::openusd::sdf;

/// What a host answers the delegate about its stage.
pub trait HostFacts {
    /// The `.mtlx` document (and the material in it) the material at
    /// `material` references, which composes nothing without a MaterialX
    /// file-format plugin.
    fn mtlx_reference(&self, material: &sdf::Path) -> Option<(PathBuf, String)>;
}

/// A host with no stage to ask: no `.mtlx` reference known.
pub struct NoHostFacts;

impl HostFacts for NoHostFacts {
    fn mtlx_reference(&self, _: &sdf::Path) -> Option<(PathBuf, String)> {
        None
    }
}
