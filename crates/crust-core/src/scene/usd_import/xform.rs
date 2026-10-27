//! Prim transforms: openusd's `UsdGeomXformable` composition, and the
//! conversion to glam.

use crate::warning;
use glam::{Affine3A, Mat4 as GMat4, Vec3};
use moho::{openusd, openusd_schemas};
use openusd::gf::Mat4d;
use openusd::tf::Token;

use super::index::data::PrimData;

/// USD authors 4x4 matrices as row-vector row-major (translation in the
/// last row, indices 12..15). glam::Mat4 is column-major with the
/// column-vector convention, so USD's row-major layout is exactly the
/// column-major layout of the transposed matrix — which is what we want
/// for M * v evaluation.
fn usd_mat_to_glam(m: Mat4d) -> GMat4 {
    GMat4::from_cols_array(&m.0.map(|v| v as f32))
}

/// Every op kind `UsdGeomXformOp` defines. openusd composes any other kind as
/// identity without a word; C++ USD rejects it. This list decides a
/// warning only, never a matrix.
const XFORM_OP_KINDS: &[&str] = &[
    "translate",
    "translateX",
    "translateY",
    "translateZ",
    "scale",
    "scaleX",
    "scaleY",
    "scaleZ",
    "rotateX",
    "rotateY",
    "rotateZ",
    "rotateXYZ",
    "rotateXZY",
    "rotateYXZ",
    "rotateYZX",
    "rotateZXY",
    "rotateZYX",
    "orient",
    "transform",
];

/// The kind of an `xformOpOrder` entry (`!invert!xformOp:translate:pivot` →
/// `translate`), or `None` when the entry is not an `xformOp:` name at all.
fn op_kind(entry: &str) -> Option<&str> {
    let name = entry.strip_prefix("!invert!").unwrap_or(entry);
    name.strip_prefix("xformOp:")?.split(':').next()
}

/// `xformOpOrder`'s entries, as the prim composes it (its fallback, empty,
/// when unauthored).
fn op_order(prim: &PrimData) -> Vec<String> {
    match prim.resolved("xformOpOrder") {
        Some(openusd::sdf::Value::TokenVec(v)) => v.iter().map(|t| t.as_str().to_owned()).collect(),
        _ => Vec::new(),
    }
}

/// [`warn_unknown_ops`], from the prim's generic data.
fn warn_unknown_ops_data(prim: &PrimData) {
    let order = op_order(prim);
    let unknown: Vec<&str> = order
        .iter()
        .map(String::as_str)
        .filter(|e| *e != "!resetXformStack!")
        .filter(|e| !op_kind(e).is_some_and(|k| XFORM_OP_KINDS.contains(&k)))
        .collect();
    if !unknown.is_empty() {
        warning!(
            XformUnknownOp,
            at = prim.path(),
            "{}: xformOpOrder lists {} — not a UsdGeomXformOp kind, read as identity",
            prim.path(),
            unknown.join(", ")
        );
    }
}

/// [`warn_unknown_type`], from the prim's USD type (`__usdPrimInfo`) and
/// generic data.
fn warn_unknown_type_data(prim: &PrimData) {
    let ty = prim.usd_type();
    if ty.is_empty()
        || openusd_schemas::schema_registry().is_concrete_type(&Token::from(ty.as_str()))
    {
        return;
    }
    let authors_ops = match prim.resolved("xformOpOrder") {
        Some(openusd::sdf::Value::TokenVec(v)) => !v.is_empty(),
        _ => false,
    };
    if authors_ops {
        warning!(
            XformUnknownType,
            at = prim.path(),
            "{}: its type {ty} is not one the schema registry knows, so it is not Xformable and \
             its xformOps are ignored (further occurrences are counted in the import's \
             warnings)",
            prim.path()
        );
    }
}

/// The prim's local `xform` in the scene index — Moho's `xform` data, read
/// from the input's stage scene index, not the flattened chain, so the local
/// matrix stays `f64` until crust composes it as before — at `offset` from
/// the index's time, and whether it resets the stack. `None` when the
/// prim's ops have no effect (or its stack cannot be read): no `xform`.
/// `Some((None, resets))` when the local transform cannot be composed.
fn local_xform(prim: &PrimData, offset: f32) -> Option<(Option<Mat4d>, bool)> {
    use moho::tokens::xform as t;
    let resets = matches!(
        prim.locate([t::XFORM, t::RESET_XFORM_STACK])
            .and_then(|d| d.value()),
        Some(openusd::sdf::Value::Bool(true))
    );
    let matrix = prim.locate([t::XFORM, t::MATRIX])?;
    let local = match matrix.as_sampled().and_then(|s| s.value(offset)) {
        Some(openusd::sdf::Value::Matrix4d(m)) => Some(m),
        _ => None,
    };
    Some((local, resets))
}

/// A prim's transform given its parent's, over the scene index (`parent ·
/// local`, or `local` alone when its stack resets): `prim` is the prim in the
/// input's stage scene index, `stage` its stage (for the one diagnostic the
/// index does not carry, `xform.uncomposable`, design D7) and `time` the
/// index's time.
pub(super) fn compose_index(
    stage: &openusd::usd::Stage,
    prim: &PrimData,
    parent: GMat4,
    time: Option<f64>,
) -> GMat4 {
    compose_data(stage, prim, parent, time, 0.0, true)
}

/// The local-to-world of the prim at `path` on `stage`, for a reader that is
/// handed a stage rather than the walk's index (a camera's lens, a live
/// rebuild): [`index_local_to_world`] over a stage scene index of its own, at
/// time code `time` (`None`, the default time).
pub(super) fn stage_local_to_world(
    stage: &openusd::usd::Stage,
    path: &openusd::sdf::Path,
    time: Option<f64>,
) -> GMat4 {
    let index = moho::stage::StageSceneIndex::new();
    if let Some(t) = time {
        index.set_time(openusd::usd::TimeCode::new(t));
    }
    index.set_stage(stage.clone());
    let handle: moho::scene_index::SceneIndexHandle = index;
    index_local_to_world(stage, &handle, path, time)
}

/// A prim's local-to-world over the scene index: [`compose_index`] folded
/// from the root down `path`'s ancestors in the stage scene index `index`
/// over `stage`, at the index's `time` — `camera::local_to_world` (the
/// stage walk) on the index, for a reader outside the walk.
pub(super) fn index_local_to_world(
    stage: &openusd::usd::Stage,
    index: &moho::scene_index::SceneIndexHandle,
    path: &openusd::sdf::Path,
    time: Option<f64>,
) -> GMat4 {
    let mut chain = vec![path.clone()];
    let mut cur = path.clone();
    while let Some(parent) = cur.parent() {
        chain.push(parent.clone());
        if parent.is_abs_root() {
            break;
        }
        cur = parent;
    }
    chain.iter().rev().fold(GMat4::IDENTITY, |acc, p| {
        compose_index(stage, &PrimData::at(index.as_ref(), p), acc, time)
    })
}

fn compose_data(
    stage: &openusd::usd::Stage,
    prim: &PrimData,
    parent: GMat4,
    time: Option<f64>,
    offset: f32,
    report: bool,
) -> GMat4 {
    if !prim.is_a("Xformable") {
        if report {
            warn_unknown_type_data(prim);
        }
        return parent;
    }
    if report {
        warn_unknown_ops_data(prim);
    }
    let (local, resets) = match local_xform(prim, offset) {
        Some((Some(m), resets)) => (usd_mat_to_glam(m), resets),
        found => {
            // No `xform`: the ops have no effect, or the stack cannot be read
            // — which openusd says, when it is so.
            let resets = found.is_some_and(|(_, r)| r);
            if report {
                let at = time.map(|t| t + f64::from(offset));
                if let Some(e) = super::index::stage_facts::xform_error(stage, prim.path(), at) {
                    warning!(
                        XformUncomposable,
                        at = prim.path(),
                        "{}: could not compose its xformOp stack ({e}) — its local transform is \
                         identity",
                        prim.path()
                    );
                }
            }
            (GMat4::IDENTITY, resets)
        }
    };
    if resets { local } else { parent * local }
}

/// What the prim's own op stack says about time, from the scene index: whether the prim's local transform
/// might vary (`TransformMightBeTimeVarying`: its ops have an effect, Moho's
/// `xform` is there, and an op attribute might vary, its generic
/// `mightBeTimeVarying`, M9), and whether it resets its parent's.
fn own_timing_data(prim: &PrimData) -> (bool, bool) {
    if !prim.is_a("Xformable") {
        return (false, false);
    }
    let Some((_, resets)) = local_xform(prim, 0.0) else {
        return (false, false);
    };
    // The ops after the last reset, as `GetOrderedXformOps` lists them.
    let order = op_order(prim);
    let start = order
        .iter()
        .rposition(|e| e == "!resetXformStack!")
        .map_or(0, |i| i + 1);
    let varies = order[start..]
        .iter()
        .any(|e| prim.might_vary(e.strip_prefix("!invert!").unwrap_or(e)));
    (varies, resets)
}

/// A moving prim's local-to-world at shutter open and at shutter close.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Pair {
    pub(super) open: GMat4,
    pub(super) close: GMat4,
}

/// What the traversal knows of a prim's world transform over time.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum Timing {
    /// Its transform and every ancestor's are static, or nobody asked.
    #[default]
    Static,
    /// Its transform varies over time, but the shutter is instantaneous (or
    /// has no frame), so nothing moves.
    Varies,
    /// Its transform varies over time and moves over the shutter. Boxed:
    /// every stack entry of the traversal carries a timing, and few move.
    Moves(Box<Pair>),
}

/// A placement's local-to-world over the shutter: at shutter open, and at
/// shutter close when its transform varies over the shutter. Every motion
/// the import produces starts at `open` and ends at [`Span::end`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Span {
    pub(super) open: GMat4,
    pub(super) close: Option<GMat4>,
}

impl Span {
    /// A placement that does not move.
    pub(super) fn fixed(world: GMat4) -> Self {
        Span {
            open: world,
            close: None,
        }
    }

    /// The span of a prim at `world` (its transform at the frame) with
    /// `timing`: from shutter open to shutter close when it moves.
    pub(super) fn of(world: GMat4, timing: &Timing) -> Self {
        match timing {
            Timing::Moves(pair) => Span {
                open: pair.open,
                close: Some(pair.close),
            },
            Timing::Static | Timing::Varies => Span::fixed(world),
        }
    }

    /// Whether the transform differs between the shutter's ends.
    pub(super) fn moves(&self) -> bool {
        self.close.is_some_and(|c| c != self.open)
    }

    /// The end transform the kernel interpolates to from `open`:
    /// `T(v) · X(close)`, with `v` the prim's authored
    /// `crust:motion:translate`. `None` when nothing moves: no `v` and no
    /// close distinct from open. An authored `v` alone keeps the end
    /// `T(v) · X(open)` it always had, bit for bit.
    pub(super) fn end(&self, v: Option<Vec3>) -> Option<Affine3A> {
        let close = self.close.filter(|_| self.moves());
        match (close, v) {
            (None, None) => None,
            (Some(c), None) => Some(Affine3A::from_mat4(c)),
            (c, Some(v)) => {
                Some(Affine3A::from_translation(v) * Affine3A::from_mat4(c.unwrap_or(self.open)))
            }
        }
    }
}

/// Prims met by the traversal for one cause of time-sampled motion not being
/// produced, for a summarised warning: how many, and the first.
#[derive(Default, Debug)]
pub(super) struct Tally {
    pub(super) count: u64,
    pub(super) first: Option<String>,
}

impl Tally {
    pub(super) fn saw(&mut self, path: &openusd::sdf::Path) {
        self.count += 1;
        if self.first.is_none() {
            self.first = Some(path.to_string());
        }
    }
}

/// How the import turns time-sampled transforms into motion over the
/// shutter, and what it met that it did not move.
///
/// The question "might this transform vary over time" is asked only when
/// its answer can matter — the shutter's offsets open an interval, or a
/// `motionvector` var is requested (whose zero vectors are then warned
/// about) — so a static render with neither asks nothing.
#[derive(Default, Debug)]
pub(super) struct TransformMotion {
    /// Shutter open and shutter close in time codes, `None` when nothing
    /// moves.
    interval: Option<(f64, f64)>,
    /// Shutter open is the frame itself: the transform at the frame is
    /// already the open one.
    open_at_frame: bool,
    /// Moving placements are posed at the frame rather than at shutter open
    /// (`disableMotionBlur`, see [`TransformMotion::span`]).
    frame_pose: bool,
    /// Whether prims are asked about time at all.
    query: bool,
    /// The frame, the scene index's time, from which the shutter's ends are
    /// read as offsets ([`TransformMotion::timing_index`]).
    frame: Option<f64>,
    /// Drawn prims whose transform varies over time, while the shutter
    /// moves nothing.
    pub(super) varying: Tally,
    /// Prims whose transform moves over the shutter, but which get no
    /// motion: curves, lights, volumes, prims inside a prototype, a baked
    /// non-invertible mesh.
    pub(super) ignored: Tally,
}

impl TransformMotion {
    /// `interval` is the shutter's; `open_at_frame` when its open end is the
    /// frame; `frame_pose` when moving placements start at the frame
    /// ([`TransformMotion::span`]); `query` when transforms are asked about
    /// time at all.
    pub(super) fn new(
        interval: Option<(f64, f64)>,
        open_at_frame: bool,
        frame_pose: bool,
        query: bool,
    ) -> Self {
        TransformMotion {
            interval,
            open_at_frame,
            frame_pose: frame_pose && !open_at_frame,
            query: query || interval.is_some(),
            ..Default::default()
        }
    }

    /// The span of a prim at `world` (its transform at the frame) with
    /// `timing`.
    ///
    /// A moving prim normally spans shutter open to shutter close. Posed at
    /// the frame (`disableMotionBlur`, which UsdRender defines as a `[0, 0]`
    /// shutter at the current time code), it starts at `world` instead and
    /// moves by the shutter's world-space displacement,
    /// `X(close) · X(open)⁻¹`: the camera rays, all at the span's start, see
    /// it exactly where it is at the frame, and the `motionvector` AOV
    /// measures the shutter's motion from there. A singular shutter-open
    /// transform has no displacement to carry, and keeps its shutter span.
    pub(super) fn span(&self, world: GMat4, timing: &Timing) -> Span {
        let span = Span::of(world, timing);
        let Some(close) = span.close.filter(|_| self.frame_pose) else {
            return span;
        };
        if span.open.determinant().abs() < 1e-12 {
            return span;
        }
        Span {
            open: world,
            close: Some(close * span.open.inverse() * world),
        }
    }

    /// The frame the shutter's ends are offsets from (the index's time).
    pub(super) fn with_frame(mut self, frame: Option<f64>) -> Self {
        self.frame = frame;
        self
    }

    /// How `prim` (in the input's stage scene index) moves over the shutter,
    /// below a parent timed `parent`. Its transform at the shutter's ends is
    /// Moho's `xform/matrix` at those ends' offsets from the frame, `f32`
    /// as Hydra's sampled data sources take them: an offset `f32` cannot hold
    /// exactly reads another time than the direct importer's `f64` one, the
    /// change's listed parity exception (`import-parity`).
    pub(super) fn timing_index(
        &self,
        stage: &openusd::usd::Stage,
        prim: &PrimData,
        parent: &Timing,
        parent_world: GMat4,
        world: GMat4,
    ) -> Timing {
        if !self.query {
            return Timing::Static;
        }
        let (varies, resets) = own_timing_data(prim);
        if !varies && (resets || *parent == Timing::Static) {
            return Timing::Static;
        }
        let Some((open, close)) = self.interval else {
            return Timing::Varies;
        };
        let parent = match parent {
            Timing::Moves(p) => **p,
            Timing::Static | Timing::Varies => Pair {
                open: parent_world,
                close: parent_world,
            },
        };
        let frame = self.frame.unwrap_or(0.0);
        #[allow(clippy::cast_possible_truncation)]
        let at = |end: f64, parent: GMat4| {
            compose_data(stage, prim, parent, self.frame, (end - frame) as f32, false)
        };
        Timing::Moves(Box::new(Pair {
            open: if self.open_at_frame {
                world
            } else {
                at(open, parent.open)
            },
            close: at(close, parent.close),
        }))
    }

    /// Counts `prim`, whose transform has `timing` but which gets no motion.
    pub(super) fn unmoved(&mut self, path: &openusd::sdf::Path, timing: &Timing) {
        match timing {
            Timing::Static => {}
            Timing::Varies => self.varying.saw(path),
            Timing::Moves(_) => self.ignored.saw(path),
        }
    }

    /// Counts a drawn prim whose transform varies but which the shutter does
    /// not move.
    pub(super) fn drawn(&mut self, path: &openusd::sdf::Path, timing: &Timing) {
        if *timing == Timing::Varies {
            self.varying.saw(path);
        }
    }

    /// Counts `prim`, inside a prototype, when its own ops vary: motion
    /// inside a prototype is not produced.
    /// `xform` is the prim in the stage scene index, `path` its name.
    pub(super) fn in_prototype(&mut self, xform: &PrimData, path: &openusd::sdf::Path) {
        if self.query && own_timing_data(xform).0 {
            match self.interval {
                Some(_) => self.ignored.saw(path),
                None => self.varying.saw(path),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::usd_import::stage_builder;
    use glam::Vec3;
    use openusd::sdf;

    /// `/W/P` of a stage whose `Xform` `P` authors `ops` under a parent `W`
    /// translated by `(0, 0, 7)`, with its world transform.
    fn world_of(name: &str, ops: &str) -> GMat4 {
        world_of_type(name, "Xform", ops)
    }

    /// [`world_of`] with `P` of type `ty` (empty: an untyped `def`).
    fn world_of_type(name: &str, ty: &str, ops: &str) -> GMat4 {
        let dir = std::env::temp_dir().join("crust_xform_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(
            &path,
            format!(
                "#usda 1.0\ndef Xform \"W\"\n{{\n    double3 xformOp:translate = (0, 0, 7)\n    \
                 uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    \
                 def {ty} \"P\"\n    {{\n{ops}\n    }}\n}}\n"
            ),
        )
        .expect("write stage");
        let stage = stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        stage_local_to_world(&stage, &sdf::path("/W/P").unwrap(), None)
    }

    fn assert_near(a: GMat4, b: GMat4) {
        assert!(a.abs_diff_eq(b, 1e-5), "expected\n{b}\ngot\n{a}");
    }

    #[test]
    fn an_unknown_op_kind_is_identity_and_the_rest_composes() {
        let m = world_of(
            "unknown_kind",
            r#"        double3 xformOp:translate = (1, 2, 3)
        double3 xformOp:bogus = (5, 5, 5)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:bogus"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(1.0, 2.0, 10.0)));
    }

    /// A reset after the first entry keeps only the ops listed after it and
    /// drops the parent, as C++ `GetOrderedXformOps` does. (openusd 0.7
    /// refused such a stack: identity, with the parent kept.)
    #[test]
    fn a_mid_stack_reset_keeps_only_the_ops_after_it() {
        let m = world_of(
            "mid_stack_reset",
            r#"        double3 xformOp:translate = (1, 2, 3)
        double3 xformOp:translate:after = (0, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "!resetXformStack!", "xformOp:translate:after"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(0.0, 2.0, 0.0)));
    }

    /// A prim that is not `Xformable` contributes nothing, whatever ops it
    /// authors, as in C++ USD: it passes its parent's transform through.
    #[test]
    fn ops_on_a_prim_that_is_not_xformable_are_ignored() {
        let ops = r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]"#;
        let parent = GMat4::from_translation(Vec3::new(0.0, 0.0, 7.0));
        assert_near(world_of_type("scope_ops", "Scope", ops), parent);
        assert_near(world_of_type("untyped_ops", "", ops), parent);
    }

    /// Every `Xformable` type C++ USD defines outside UsdGeom and UsdLux composes
    /// its ops: the schema registry knows these types only because crust-core
    /// compiles their families in, and an unknown type would read as not
    /// `Xformable`.
    #[test]
    fn xformable_prims_of_every_family_compose_their_ops() {
        let ops = r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]"#;
        let placed = GMat4::from_translation(Vec3::new(1.0, 2.0, 10.0));
        for ty in [
            "SkelRoot",
            "Skeleton",
            "Volume",
            "OpenVDBAsset",
            "Field3DAsset",
            "ParticleField",
            "ParticleField3DGaussianSplat",
            "GenerativeProcedural",
            "SpatialAudio",
        ] {
            assert_near(world_of_type(&format!("{ty}_ops"), ty, ops), placed);
        }
    }

    #[test]
    fn a_leading_reset_drops_the_parent() {
        let m = world_of(
            "leading_reset",
            r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(1.0, 2.0, 3.0)));
    }

    /// The pivot pair, `!invert!` and a three-axis rotation, against the
    /// matrix C++ USD computes for the same stack (row-vector:
    /// `0 2 0 0 / -2 0 0 0 / 0 0 2 0 / 5 2 0 1`).
    #[test]
    fn a_pivot_stack_matches_cpp_usd() {
        let m = world_of(
            "pivot",
            r#"        double3 xformOp:translate = (2, 3, 0)
        double3 xformOp:translate:pivot = (1, 1, 0)
        float3 xformOp:rotateXYZ = (0, 0, 90)
        float3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:translate:pivot", "xformOp:rotateXYZ", "xformOp:scale", "!invert!xformOp:translate:pivot"]"#,
        );
        let cpp = GMat4::from_cols_array(&[
            0.0, 2.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 5.0, 2.0, 0.0, 1.0,
        ]);
        assert_near(m, GMat4::from_translation(Vec3::new(0.0, 0.0, 7.0)) * cpp);
    }

    /// Opens `text` as a stage and returns it with the prims at `paths`.
    fn open(name: &str, text: &str) -> openusd::usd::Stage {
        let dir = std::env::temp_dir().join("crust_xform_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, text).expect("write stage");
        stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens")
    }

    const ANIMATED: &str = r#"#usda 1.0
def Xform "A"
{
    double3 xformOp:translate.timeSamples = {
        0: (0, 0, 0),
        10: (10, 0, 0),
    }
    uniform token[] xformOpOrder = ["xformOp:translate"]
    def Xform "Child"
    {
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "Reset"
    {
        double3 xformOp:translate = (0, 2, 0)
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"]
    }
}
def Xform "Still"
{
    double3 xformOp:translate = (0, 0, 3)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}
"#;

    /// The input's stage scene index over `stage` at `time`, as the walk
    /// reads transforms (before the flattening).
    fn stage_index(
        stage: &openusd::usd::Stage,
        time: Option<f64>,
    ) -> moho::scene_index::SceneIndexHandle {
        let index = moho::stage::StageSceneIndex::new();
        if let Some(t) = time {
            index.set_time(openusd::usd::TimeCode::new(t));
        }
        index.set_stage(stage.clone());
        index
    }

    /// `path` in `index`, as the walk reads it.
    fn at_path(index: &moho::scene_index::SceneIndexHandle, path: &str) -> PrimData {
        PrimData::in_index(index, &sdf::path(path).unwrap())
    }

    /// At an offset from the index's time: Moho's sampled `xform/matrix`.
    #[test]
    fn an_offset_reads_another_time() {
        let stage = open("compose_at", ANIMATED);
        let index = stage_index(&stage, Some(2.0));
        let a = at_path(&index, "/A");
        let at = |m: GMat4| m.transform_point3(Vec3::ZERO);
        assert_eq!(
            at(compose_data(
                &stage,
                &a,
                GMat4::IDENTITY,
                Some(2.0),
                5.0,
                false
            )),
            Vec3::new(7.0, 0.0, 0.0)
        );
        assert_eq!(
            at(compose_index(&stage, &a, GMat4::IDENTITY, Some(2.0))),
            Vec3::new(2.0, 0.0, 0.0)
        );
    }

    /// The pair the traversal carries, prim by prim: nothing on a static
    /// prim, a moving pair on a static child of a moving parent, and nothing
    /// on a child that resets the stack with static ops of its own.
    #[test]
    fn the_shutter_pair_follows_moving_ancestors_but_not_a_reset() {
        let stage = open("pair", ANIMATED);
        let index = stage_index(&stage, Some(2.0));
        let motion =
            TransformMotion::new(Some((2.0, 3.0)), true, false, false).with_frame(Some(2.0));
        let timing = |path: &str, parent: Timing, parent_world: GMat4| {
            let p = at_path(&index, path);
            let world = compose_index(&stage, &p, parent_world, Some(2.0));
            (
                motion.timing_index(&stage, &p, &parent, parent_world, world),
                world,
            )
        };
        let (still, _) = timing("/Still", Timing::Static, GMat4::IDENTITY);
        assert_eq!(still, Timing::Static);

        let (a, a_world) = timing("/A", Timing::Static, GMat4::IDENTITY);
        let Timing::Moves(pair) = &a else {
            panic!("/A moves: {a:?}")
        };
        assert_eq!(pair.open, a_world, "shutter open at the frame reuses it");
        assert_eq!(
            pair.close.transform_point3(Vec3::ZERO),
            Vec3::new(3.0, 0.0, 0.0)
        );

        let (child, _) = timing("/A/Child", a.clone(), a_world);
        let Timing::Moves(pair) = &child else {
            panic!("/A/Child inherits the motion: {child:?}")
        };
        assert_eq!(
            pair.open.transform_point3(Vec3::ZERO),
            Vec3::new(2.0, 1.0, 0.0)
        );
        assert_eq!(
            pair.close.transform_point3(Vec3::ZERO),
            Vec3::new(3.0, 1.0, 0.0)
        );

        let (reset, _) = timing("/A/Reset", a, a_world);
        assert_eq!(reset, Timing::Static);
    }

    /// A shutter that opens before the frame: the span runs from shutter open
    /// to shutter close, unless the placement is posed at the frame
    /// (`disableMotionBlur`), when it starts at the frame and moves by the
    /// shutter's displacement from there. Opening at the frame, both agree.
    #[test]
    fn a_frame_pose_starts_at_the_frame_and_moves_by_the_shutter() {
        let stage = open("frame_pose", ANIMATED);
        let index = stage_index(&stage, Some(2.0));
        let a = at_path(&index, "/A");
        let world = compose_index(&stage, &a, GMat4::IDENTITY, Some(2.0));
        let at = |m: GMat4| m.transform_point3(Vec3::ZERO);
        let ends = |motion: TransformMotion| {
            let motion = motion.with_frame(Some(2.0));
            let timing = motion.timing_index(&stage, &a, &Timing::Static, GMat4::IDENTITY, world);
            let span = motion.span(world, &timing);
            (at(span.open), at(span.close.expect("it moves")))
        };

        let centred = TransformMotion::new(Some((1.75, 2.25)), false, false, false);
        assert_eq!(
            ends(centred),
            (Vec3::new(1.75, 0.0, 0.0), Vec3::new(2.25, 0.0, 0.0))
        );
        let posed = TransformMotion::new(Some((1.75, 2.25)), false, true, false);
        assert_eq!(
            ends(posed),
            (Vec3::new(2.0, 0.0, 0.0), Vec3::new(2.5, 0.0, 0.0))
        );
        let at_frame = TransformMotion::new(Some((2.0, 2.5)), true, true, false);
        let unposed = TransformMotion::new(Some((2.0, 2.5)), true, false, false);
        assert_eq!(ends(at_frame), ends(unposed));
    }

    /// With an instantaneous shutter the question is still answered when
    /// asked (for the warnings), at the default time too (M9), but nothing is
    /// composed twice; unasked, a moving prim reads static. A static stage
    /// with a shutter creates no pair at all.
    #[test]
    fn without_an_interval_a_varying_prim_varies_and_moves_nothing() {
        let stage = open("varies", ANIMATED);
        let index = stage_index(&stage, None);
        let a = at_path(&index, "/A");
        let world = compose_index(&stage, &a, GMat4::IDENTITY, None);
        let asked = TransformMotion::new(None, true, false, true);
        assert_eq!(
            asked.timing_index(&stage, &a, &Timing::Static, GMat4::IDENTITY, world),
            Timing::Varies
        );
        let unasked = TransformMotion::new(None, true, false, false);
        assert_eq!(
            unasked.timing_index(&stage, &a, &Timing::Static, GMat4::IDENTITY, world),
            Timing::Static
        );
        let moving =
            TransformMotion::new(Some((0.0, 1.0)), true, false, false).with_frame(Some(0.0));
        let still = at_path(&index, "/Still");
        let still_world = compose_index(&stage, &still, GMat4::IDENTITY, None);
        assert_eq!(
            moving.timing_index(
                &stage,
                &still,
                &Timing::Static,
                GMat4::IDENTITY,
                still_world
            ),
            Timing::Static
        );
    }

    /// The end transform: an authored translation alone keeps the end it
    /// always had, bit for bit; with motion it moves the close transform.
    #[test]
    fn the_end_adds_the_authored_translation_to_the_close_transform() {
        let open = GMat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let close = GMat4::from_translation(Vec3::new(2.0, 2.0, 3.0));
        let v = Vec3::new(0.0, 0.5, 0.0);
        let fixed = Span::fixed(open);
        assert_eq!(fixed.end(None), None);
        assert_eq!(
            fixed.end(Some(v)),
            Some(Affine3A::from_translation(v) * Affine3A::from_mat4(open))
        );
        let moving = Span {
            open,
            close: Some(close),
        };
        assert!(moving.moves());
        assert_eq!(moving.end(None), Some(Affine3A::from_mat4(close)));
        let end = moving.end(Some(v)).unwrap();
        assert_eq!(end.translation, glam::Vec3A::new(2.0, 2.5, 3.0));
        // A close equal to the open is no motion.
        let held = Span {
            open,
            close: Some(open),
        };
        assert!(!held.moves());
        assert_eq!(held.end(None), None);
    }
}
