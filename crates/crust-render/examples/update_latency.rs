//! The update-latency benchmark (`moho-hydra-parity` design D12,
//! `interactive-updates` design D7): how long an edit takes from the stage to
//! Crust's restarted render, through a host over Moho and the render
//! delegate, and what the process holds after it.
//!
//! ```text
//! cargo run --release -p crust-render --features delegate --example update_latency -- \
//!     [--runs N] [--edits LIST] [--recommit K] --fixture MESHES [--instanced PxM]
//! cargo run --release -p crust-render --features delegate --example update_latency -- \
//!     [--runs N] [--edits LIST] [--recommit K] [--camera PATH] [--linked PATH] \
//!     [--include PATH] [--baked PATH] [--proto-mesh PATH] [--pi-mesh PATH] <scene>
//! ```
//!
//! The host opens the stage (payloads loaded), keeps its stage scene index,
//! Moho's standard chain with the scene globals filter, and a
//! [`Delegate`] rendering at 64×36. It authors its own render settings and a
//! rect light, `/UpdateBenchLights/area`, which the area light edit moves.
//! One run:
//!
//! 1. author the edit on the stage (openusd);
//! 2. `apply_pending_updates` on the stage scene index;
//! 3. the delegate's `update()`, which revalidates the kept assets, cancels
//!    and joins the render, flushes, brings the scene up to date and restarts
//!    the render;
//! 4. stop the clock when `render()` has moved.
//!
//! The edits (`--edits`, by key, all by default):
//!
//! | key | edit |
//! |---|---|
//! | `focal` | the camera's `focalLength` |
//! | `move` | the camera's first transform op |
//! | `light` | a sphere light with trivial collections added, then the same light removed, both timed |
//! | `linked` | a sphere light whose light link includes one prim added (its removal untimed) |
//! | `area` | the rect light moved |
//! | `baked` | the points of a mesh placed once (baked) edited |
//! | `proto` | the points of a mesh inside a prototype that native instances place |
//! | `pi` | the points of a point instancer's prototype mesh |
//! | `categories` | a mesh added to (and, the next run, taken out of) a rect light's `collection:lightLink:includes`; the light is defined before the runs, untimed |
//!
//! The fixture is Moho's (a camera with a translate op and N one-triangle
//! meshes under `/world`, the linked light including `/world/a`), plus
//! `/world/baked`, a quad no other mesh shares; with `--instanced P×M`, a
//! prototype mesh `/lib/shape` placed by M native instances under `/inst`
//! and a point instancer `/pi` placing its own prototype mesh P times. On a
//! named scene, `--baked`, `--proto-mesh` and `--pi-mesh` name the meshes to
//! reshape, and an edit whose mesh is not named is skipped.
//!
//! Prints, per edit, the median (the mean of the two middle runs for an even
//! N) and worst, the split, the class and the resident memory after the last
//! run; the resident and peak memory after setup. `--recommit K` instead
//! forces K `full` updates (a mesh defined, then removed, in turn) and prints
//! the resident memory after each and the peak during each.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crust_core::{StageHostFacts, UpdateClass};
use crust_render::delegate::{Delegate, Hooks, UpdateSplit};
use moho::chain::{self, RendererFilter};
use moho::openusd::gf;
use moho::openusd::sdf::{self, Value};
use moho::openusd::usd::{InitialLoadSet, Stage};
use moho::stage::StageSceneIndex;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "usage: update_latency [--runs N] [--edits LIST] [--recommit K] --fixture MESHES [--instanced PxM]\n       update_latency [--runs N] [--edits LIST] [--recommit K] [--camera PATH] [--linked PATH] [--include PATH] [--baked PATH] [--proto-mesh PATH] [--pi-mesh PATH] <scene>";

/// The scope the benchmark adds its lights under, and its render settings.
const LIGHTS: &str = "/UpdateBenchLights";
const AREA: &str = "/UpdateBenchLights/area";
const LINKING: &str = "/UpdateBenchLights/linking";
const SETTINGS: &str = "/UpdateBenchRender/settings";
const RECOMMIT: &str = "/UpdateBenchRecommit";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edit {
    FocalLength,
    CameraMove,
    TrivialLight,
    LinkedLight,
    AreaMove,
    BakedReshape,
    ProtoReshape,
    InstancerReshape,
    Categories,
}

impl Edit {
    const ALL: [Edit; 9] = [
        Edit::FocalLength,
        Edit::CameraMove,
        Edit::TrivialLight,
        Edit::LinkedLight,
        Edit::AreaMove,
        Edit::BakedReshape,
        Edit::ProtoReshape,
        Edit::InstancerReshape,
        Edit::Categories,
    ];

    fn key(self) -> &'static str {
        match self {
            Edit::FocalLength => "focal",
            Edit::CameraMove => "move",
            Edit::TrivialLight => "light",
            Edit::LinkedLight => "linked",
            Edit::AreaMove => "area",
            Edit::BakedReshape => "baked",
            Edit::ProtoReshape => "proto",
            Edit::InstancerReshape => "pi",
            Edit::Categories => "categories",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Edit::FocalLength => "camera focalLength",
            Edit::CameraMove => "camera transform",
            Edit::TrivialLight => "light, trivial collections",
            Edit::LinkedLight => "light linked to one prim",
            Edit::AreaMove => "area light moved",
            Edit::BakedReshape => "baked mesh reshaped",
            Edit::ProtoReshape => "native prototype's mesh reshaped",
            Edit::InstancerReshape => "point instancer prototype's mesh reshaped",
            Edit::Categories => "mesh included in a lightLink",
        }
    }
}

/// One timed run.
struct Sample {
    total: Duration,
    authoring: Duration,
    apply: Duration,
    split: UpdateSplit,
    class: UpdateClass,
}

/// Moho's fixture: a camera with a translate op and `meshes` meshes under
/// `/world`, the first, `/world/a`, the linked light's target; then
/// `/world/baked`, a quad placed once, and with `instanced` (P, M) the
/// native instances and the point instancer.
fn fixture_text(meshes: usize, instanced: Option<(usize, usize)>) -> String {
    let mut text =
        String::from("#usda 1.0\ndef Xform \"world\"\n{\n    def Mesh \"a\"\n    {\n    }\n");
    for i in 1..meshes {
        text.push_str(&format!(
            "    def Mesh \"m{i}\"\n    {{\n        int[] faceVertexCounts = [3]\n        int[] faceVertexIndices = [0, 1, 2]\n        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]\n    }}\n"
        ));
    }
    text.push_str(
        "    def Mesh \"baked\"\n    {\n        int[] faceVertexCounts = [4]\n        int[] faceVertexIndices = [0, 1, 2, 3]\n        point3f[] points = [(-2, -2, -5), (2, -2, -5), (2, 2, -5), (-2, 2, -5)]\n    }\n",
    );
    text.push_str(
        "}\ndef Camera \"cam\"\n{\n    float focalLength = 35\n    double3 xformOp:translate = (0, 0, 10)\n    uniform token[] xformOpOrder = [\"xformOp:translate\"]\n}\n",
    );
    if let Some((p, m)) = instanced {
        text.push_str(
            "class Xform \"lib\"\n{\n    def Mesh \"shape\"\n    {\n        int[] faceVertexCounts = [4]\n        int[] faceVertexIndices = [0, 1, 2, 3]\n        point3f[] points = [(0, 0, 0), (0.5, 0, 0), (0.5, 0.5, 0), (0, 0.5, 0)]\n    }\n}\ndef Xform \"inst\"\n{\n",
        );
        for i in 0..m {
            let (x, y) = grid(i, -3.0);
            text.push_str(&format!(
                "    def Xform \"i{i}\" (\n        instanceable = true\n        references = </lib>\n    )\n    {{\n        double3 xformOp:translate = ({x}, {y}, -4)\n        uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    }}\n"
            ));
        }
        text.push_str("}\n");
        let positions: Vec<String> = (0..p)
            .map(|i| {
                let (x, y) = grid(i, 3.0);
                format!("({x}, {y}, -6)")
            })
            .collect();
        text.push_str(&format!(
            "def PointInstancer \"pi\"\n{{\n    rel prototypes = [</pi/Protos/p>]\n    int[] protoIndices = [{}]\n    point3f[] positions = [{}]\n    def Scope \"Protos\"\n    {{\n        def Mesh \"p\"\n        {{\n            int[] faceVertexCounts = [3]\n            int[] faceVertexIndices = [0, 1, 2]\n            point3f[] points = [(0, 0, 0), (0.4, 0, 0), (0, 0.4, 0)]\n        }}\n    }}\n}}\n",
            vec!["0"; p].join(", "),
            positions.join(", "),
        ));
    }
    text
}

/// Where the `i`-th placement of a grid of 100 a row goes, the rows from `y0`.
#[allow(clippy::cast_precision_loss)]
fn grid(i: usize, y0: f64) -> (f64, f64) {
    ((i % 100) as f64 * 0.6 - 30.0, (i / 100) as f64 * 0.6 + y0)
}

/// The first prim of Hydra type `ty` after `skip` others, depth first.
fn nth_of_type(
    index: &dyn moho::scene_index::SceneIndex,
    ty: &str,
    mut skip: usize,
) -> Option<sdf::Path> {
    let mut stack = vec![sdf::Path::abs_root()];
    while let Some(path) = stack.pop() {
        if index.prim(&path).ty == ty {
            if skip == 0 {
                return Some(path);
            }
            skip -= 1;
        }
        stack.extend(index.children(&path).into_iter().rev());
    }
    None
}

/// The meshes the reshaping edits edit, when the scene has them.
#[derive(Default)]
struct Meshes {
    baked: Option<sdf::Path>,
    proto: Option<sdf::Path>,
    pi: Option<sdf::Path>,
}

/// The prims named on the command line.
#[derive(Default)]
struct Named {
    camera: Option<String>,
    linked: Option<String>,
    include: Option<String>,
    baked: Option<String>,
    proto: Option<String>,
    pi: Option<String>,
}

struct Bench {
    stage: Stage,
    index: Rc<StageSceneIndex>,
    facts: StageHostFacts,
    delegate: Delegate,
    camera: sdf::Path,
    target: sdf::Path,
    /// The second mesh the categories edit includes in turn, when the scene
    /// has one (or one was named): only that edit needs it.
    include: Option<sdf::Path>,
    meshes: Meshes,
    setup: Duration,
    next_light: usize,
}

/// The process's resident memory, in GiB.
#[allow(clippy::cast_precision_loss)]
fn resident() -> f64 {
    crust_core::current_memory_bytes().unwrap_or(0) as f64 / (1u64 << 30) as f64
}

/// The process's peak resident memory, in GiB.
#[allow(clippy::cast_precision_loss)]
fn peak() -> f64 {
    crust_core::peak_memory_bytes().unwrap_or(0) as f64 / (1u64 << 30) as f64
}

/// Resets the peak resident size (Linux's `clear_refs`), so the next
/// [`peak`] is the peak from here. Nothing where the kernel refuses it.
fn reset_peak() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

impl Bench {
    fn open(path: &Path, named: &Named) -> Result<Bench> {
        let started = Instant::now();
        let stage = Stage::builder()
            .schema_registry(crust_core::usd_schema_registry())
            .load(InitialLoadSet::LoadAll)
            .open(path.to_str().ok_or("the scene path is not UTF-8")?)?;
        stage.define_typed_prim(LIGHTS, "Scope")?;
        let index = StageSceneIndex::new();
        index.set_stage(stage.clone());
        let find = |given: &Option<String>, ty: &str, skip: usize| -> Result<sdf::Path> {
            match given {
                Some(path) => Ok(sdf::path(path.as_str())?),
                None => nth_of_type(index.as_ref(), ty, skip)
                    .ok_or_else(|| format!("no {ty} in the scene").into()),
            }
        };
        let camera = find(&named.camera, "camera", 0)?;
        let target = find(&named.linked, "mesh", 0)?;
        let include = match &named.include {
            Some(path) => Some(sdf::path(path.as_str())?),
            None => nth_of_type(index.as_ref(), "mesh", 1),
        };
        let mesh = |given: &Option<String>| -> Result<Option<sdf::Path>> {
            Ok(match given {
                Some(path) => Some(sdf::path(path.as_str())?),
                None => None,
            })
        };
        let meshes = Meshes {
            baked: mesh(&named.baked)?,
            proto: mesh(&named.proto)?,
            pi: mesh(&named.pi)?,
        };
        // A small render through the camera edited: the settings the
        // benchmark authors, active as the stage's.
        let settings = stage.define_typed_prim(SETTINGS, "RenderSettings")?;
        settings
            .create_relationship("camera")?
            .set_targets([camera.clone()])?;
        settings
            .create_attribute("resolution", "int2")?
            .set(Value::Vec2i(gf::Vec2i { x: 64, y: 36 }))?;
        stage.set_stage_metadata("renderSettingsPrimPath", Value::String(SETTINGS.to_owned()))?;
        // The rect light the area light edit moves.
        let area = stage.define_typed_prim(AREA, "RectLight")?;
        area.create_attribute("inputs:intensity", "float")?
            .set(5.0f32)?;
        area.create_attribute("xformOp:translate", "double3")?
            .set(Value::Vec3d(gf::Vec3d {
                x: 0.0,
                y: 4.0,
                z: 2.0,
            }))?;
        area.create_attribute("xformOpOrder", "token[]")?
            .set(Value::TokenVec(vec!["xformOp:translate".into()]))?;
        index.apply_pending_updates();
        let standard = chain::renderer_filters(
            chain::standard(index.clone()),
            &[RendererFilter::scene_globals(|_| {})],
        );
        let facts = StageHostFacts::new(&stage);
        let mut delegate = Delegate::new(standard, path, Hooks::default());
        delegate.update(&facts)?;
        delegate.start(Some(16), None)?;
        Ok(Bench {
            stage,
            index,
            facts,
            delegate,
            camera,
            target,
            include,
            meshes,
            setup: started.elapsed(),
            next_light: 0,
        })
    }

    /// One update, as a host makes it.
    fn update(&mut self) -> Result<(Duration, UpdateClass)> {
        let applying = Instant::now();
        self.index.apply_pending_updates();
        let apply = applying.elapsed();
        let class = self.delegate.update(&self.facts)?;
        Ok((apply, class))
    }

    /// The mesh `edit` reshapes, `None` when the scene has none named.
    fn reshaped(&self, edit: Edit) -> Option<&sdf::Path> {
        match edit {
            Edit::BakedReshape => self.meshes.baked.as_ref(),
            Edit::ProtoReshape => self.meshes.proto.as_ref(),
            Edit::InstancerReshape => self.meshes.pi.as_ref(),
            _ => None,
        }
    }

    /// Why `edit` cannot run on this scene, `None` when it can.
    fn missing(&self, edit: Edit) -> Option<&'static str> {
        match edit {
            Edit::BakedReshape | Edit::ProtoReshape | Edit::InstancerReshape
                if self.reshaped(edit).is_none() =>
            {
                Some("no mesh named")
            }
            Edit::Categories if self.include.is_none() => Some("no second mesh to include"),
            _ => None,
        }
    }

    /// What an edit needs before its runs, untimed.
    fn prepare(&mut self, edit: Edit) -> Result<()> {
        if edit == Edit::Categories {
            let light = self.stage.define_typed_prim(LINKING, "RectLight")?;
            light
                .create_attribute("inputs:intensity", "float")?
                .set(5.0f32)?;
            self.stage
                .attribute(format!("{LINKING}.collection:lightLink:includeRoot").as_str())?
                .set(false)?;
            self.stage
                .create_relationship(format!("{LINKING}.collection:lightLink:includes").as_str())?
                .set_targets([self.target.clone()])?;
            self.update()?;
        }
        Ok(())
    }

    /// What an edit's runs leave behind, taken away, untimed.
    fn finish(&mut self, edit: Edit) -> Result<()> {
        if edit == Edit::Categories {
            self.stage.remove_prim(LINKING)?;
            self.update()?;
        }
        Ok(())
    }

    /// One run of `edit`, and for a light added with trivial collections,
    /// the timed run of its removal.
    fn run(&mut self, edit: Edit, run: usize) -> Result<(Sample, Option<Sample>)> {
        let light = sdf::path(format!("{LIGHTS}/light{}", self.next_light))?;
        let render = self.delegate.render();
        let start = Instant::now();
        match edit {
            Edit::FocalLength => {
                #[allow(clippy::cast_precision_loss)]
                let focal = 35.0 + (run % 64) as f32 * 0.5 + 0.5;
                self.stage
                    .attribute(format!("{}.focalLength", self.camera).as_str())?
                    .set(focal)?;
            }
            Edit::CameraMove => self.move_first_op(&self.camera, run)?,
            Edit::AreaMove => self.move_first_op(&sdf::path(AREA)?, run)?,
            Edit::TrivialLight => {
                self.stage.define_typed_prim(light.clone(), "SphereLight")?;
            }
            Edit::LinkedLight => {
                self.stage.define_typed_prim(light.clone(), "SphereLight")?;
                self.stage
                    .attribute(format!("{light}.collection:lightLink:includeRoot").as_str())?
                    .set(false)?;
                self.stage
                    .create_relationship(format!("{light}.collection:lightLink:includes").as_str())?
                    .set_targets([self.target.clone()])?;
            }
            Edit::BakedReshape | Edit::ProtoReshape | Edit::InstancerReshape => {
                let mesh = self.reshaped(edit).ok_or("no mesh to reshape")?;
                self.reshape(mesh, run)?;
            }
            Edit::Categories => {
                let include = self.include.clone().ok_or("no second mesh to include")?;
                let targets = if run.is_multiple_of(2) {
                    vec![self.target.clone(), include]
                } else {
                    vec![self.target.clone()]
                };
                self.stage
                    .create_relationship(
                        format!("{LINKING}.collection:lightLink:includes").as_str(),
                    )?
                    .set_targets(targets)?;
            }
        }
        let authoring = start.elapsed();
        let (apply, class) = self.update()?;
        self.restarted(edit, render, class)?;
        let sample = Sample {
            total: start.elapsed(),
            authoring,
            apply,
            split: self.delegate.last_split(),
            class,
        };
        let mut removal = None;
        if matches!(edit, Edit::TrivialLight | Edit::LinkedLight) {
            self.next_light += 1;
            let render = self.delegate.render();
            let start = Instant::now();
            self.stage.remove_prim(light)?;
            let authoring = start.elapsed();
            let (apply, class) = self.update()?;
            self.restarted(edit, render, class)?;
            if edit == Edit::TrivialLight {
                removal = Some(Sample {
                    total: start.elapsed(),
                    authoring,
                    apply,
                    split: self.delegate.last_split(),
                    class,
                });
            }
        }
        Ok((sample, removal))
    }

    fn restarted(&self, edit: Edit, render: u64, class: UpdateClass) -> Result<()> {
        if self.delegate.render() == render {
            return Err(format!(
                "{}: the render did not restart ({})",
                edit.name(),
                class.as_str()
            )
            .into());
        }
        Ok(())
    }

    /// Moves the first point of the mesh at `mesh` to a place it did not
    /// hold: a shape no other mesh has.
    fn reshape(&self, mesh: &sdf::Path, run: usize) -> Result<()> {
        let attribute = self.stage.attribute(format!("{mesh}.points").as_str())?;
        let Some(Value::Vec3fVec(mut points)) = attribute.get::<Value>()? else {
            return Err(format!("{mesh} has no points").into());
        };
        let first = points
            .first_mut()
            .ok_or_else(|| format!("{mesh} has no points"))?;
        #[allow(clippy::cast_precision_loss)]
        let step = (run % 64) as f32 * 0.001 + 0.001;
        first.z += if run % 128 < 64 { step } else { -step };
        attribute.set(Value::Vec3fVec(points))?;
        Ok(())
    }

    /// Sets the first transform op of `prim` to a value it did not hold.
    fn move_first_op(&self, prim: &sdf::Path, run: usize) -> Result<()> {
        let order = self
            .stage
            .attribute(format!("{prim}.xformOpOrder").as_str())?
            .get::<Value>()?
            .and_then(Value::try_as_token_vec)
            .unwrap_or_default();
        let op = order
            .first()
            .ok_or_else(|| format!("{prim} has no transform op"))?;
        let attribute = self.stage.attribute(format!("{prim}.{op}").as_str())?;
        #[allow(clippy::cast_precision_loss)]
        let step = (run % 64) as f64 * 0.01 + 0.01;
        let moved = match attribute.get::<Value>()? {
            Some(Value::Matrix4d(mut m)) => {
                m.0[12] += step;
                Value::Matrix4d(m)
            }
            Some(Value::Vec3d(mut v)) => {
                v.x += step;
                Value::Vec3d(v)
            }
            #[allow(clippy::cast_possible_truncation)]
            Some(Value::Vec3f(mut v)) => {
                v.x += step as f32;
                Value::Vec3f(v)
            }
            Some(Value::Double(d)) => Value::Double(d + step),
            #[allow(clippy::cast_possible_truncation)]
            Some(Value::Float(f)) => Value::Float(f + step as f32),
            other => return Err(format!("{prim}.{op} holds {other:?}").into()),
        };
        attribute.set(moved)?;
        Ok(())
    }

    /// `k` updates that commit afresh, a mesh defined and removed in turn,
    /// each followed by the resident memory and the peak during it.
    fn recommit(&mut self, k: usize) -> Result<()> {
        println!("| re-commit | class | time (ms) | resident (GiB) | peak during (GiB) |");
        println!("|---|---|---|---|---|");
        let mesh = format!("{RECOMMIT}/m");
        let point = |x: f32, y: f32| gf::Vec3f { x, y, z: -1.0 };
        for i in 0..k {
            reset_peak();
            let start = Instant::now();
            if i.is_multiple_of(2) {
                let prim = self.stage.define_typed_prim(mesh.as_str(), "Mesh")?;
                prim.create_attribute("faceVertexCounts", "int[]")?
                    .set(vec![3i32])?;
                prim.create_attribute("faceVertexIndices", "int[]")?
                    .set(vec![0i32, 1, 2])?;
                prim.create_attribute("points", "point3f[]")?.set(vec![
                    point(0.0, 0.0),
                    point(0.3, 0.0),
                    point(0.0, 0.7),
                ])?;
            } else {
                self.stage.remove_prim(mesh.as_str())?;
            }
            let (_, class) = self.update()?;
            println!(
                "| {} | {} | {} | {:.3} | {:.3} |",
                i + 1,
                class.as_str(),
                ms(start.elapsed()),
                resident(),
                peak()
            );
        }
        Ok(())
    }
}

/// The median of `times`: the middle one, or the mean of the two middle
/// ones for an even count.
fn median(times: &[Duration]) -> Duration {
    let mut times = times.to_vec();
    times.sort();
    let mid = times.len() / 2;
    match (
        times.len() % 2,
        mid.checked_sub(1).and_then(|l| times.get(l)),
        times.get(mid),
    ) {
        (0, Some(lower), Some(upper)) => (*lower + *upper) / 2,
        (_, _, Some(middle)) => *middle,
        _ => Duration::ZERO,
    }
}

fn ms(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}

/// One table row: `samples` of the edit called `name`.
fn row(name: &str, samples: &[Sample]) {
    let pick = |f: &dyn Fn(&Sample) -> Duration| median(&samples.iter().map(f).collect::<Vec<_>>());
    let worst = samples.iter().map(|s| s.total).max().unwrap_or_default();
    let class = samples.last().map_or("-", |s| s.class.as_str());
    println!(
        "| {name} | {class} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3} |",
        ms(pick(&|s| s.total)),
        ms(worst),
        ms(pick(&|s| s.authoring)),
        ms(pick(&|s| s.apply)),
        ms(pick(&|s| s.split.revalidate)),
        ms(pick(&|s| s.split.stop)),
        ms(pick(&|s| s.split.flush)),
        ms(pick(&|s| s.split.commit)),
        ms(pick(&|s| s.split.restart)),
        resident(),
    );
}

/// The edits named by `list`'s keys, comma-separated.
fn edits(list: &str) -> Result<Vec<Edit>> {
    list.split(',')
        .map(|key| {
            Edit::ALL
                .into_iter()
                .find(|e| e.key() == key)
                .ok_or_else(|| format!("no edit {key}").into())
        })
        .collect()
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut runs = 20usize;
    let mut named = Named::default();
    let (mut fixture, mut scene, mut instanced, mut chosen, mut recommit) =
        (None, None, None, None, None);
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| format!("{arg} needs {what}"));
        match arg.as_str() {
            "--runs" => runs = value("a count")?.parse()?,
            "--camera" => named.camera = Some(value("a path")?),
            "--linked" => named.linked = Some(value("a path")?),
            "--include" => named.include = Some(value("a path")?),
            "--baked" => named.baked = Some(value("a path")?),
            "--proto-mesh" => named.proto = Some(value("a path")?),
            "--pi-mesh" => named.pi = Some(value("a path")?),
            "--fixture" => fixture = Some(value("a count")?.parse::<usize>()?),
            "--recommit" => recommit = Some(value("a count")?.parse::<usize>()?),
            "--edits" => chosen = Some(edits(&value("a list")?)?),
            "--instanced" => {
                let spec = value("PxM")?;
                let (p, m) = spec.split_once('x').ok_or("--instanced takes PxM")?;
                instanced = Some((p.parse::<usize>()?, m.parse::<usize>()?));
            }
            _ if scene.is_none() && !arg.starts_with('-') => scene = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument {arg}").into()),
        }
    }
    let path = match (fixture, scene) {
        (Some(meshes), None) => {
            let dir =
                std::env::temp_dir().join(format!("crust_update_latency_{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("update.usda");
            std::fs::write(&path, fixture_text(meshes, instanced))?;
            named.baked.get_or_insert_with(|| "/world/baked".into());
            if instanced.is_some() {
                named.proto.get_or_insert_with(|| "/lib/shape".into());
                named.pi.get_or_insert_with(|| "/pi/Protos/p".into());
            }
            path
        }
        (None, Some(scene)) => scene,
        _ => return Err("give a scene or --fixture".into()),
    };
    let mut bench = Bench::open(&path, &named)?;
    println!("scene: {}", path.display());
    println!(
        "setup (stage, chain, delegate, first commit and render start): {:.2} s, resident {:.3} GiB, peak {:.3} GiB",
        bench.setup.as_secs_f64(),
        resident(),
        peak()
    );
    println!(
        "camera: {}, linked light target: {}, runs: {runs}",
        bench.camera, bench.target
    );
    println!();
    if let Some(k) = recommit {
        return bench.recommit(k);
    }
    println!(
        "| edit | class | median (ms) | worst (ms) | authoring | apply_pending_updates | revalidate | stop | flush | commit | restart | resident (GiB) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    for edit in chosen.unwrap_or_else(|| Edit::ALL.to_vec()) {
        if let Some(why) = bench.missing(edit) {
            println!("| {} | skipped: {why} | | | | | | | | | | |", edit.name());
            continue;
        }
        bench.prepare(edit)?;
        let mut samples = Vec::with_capacity(runs);
        let mut removals = Vec::new();
        for i in 0..runs {
            let (sample, removal) = bench.run(edit, i)?;
            samples.push(sample);
            removals.extend(removal);
        }
        row(edit.name(), &samples);
        if !removals.is_empty() {
            row("light removed", &removals);
        }
        bench.finish(edit)?;
    }
    println!();
    println!("peak over the run: {:.3} GiB", peak());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("update_latency: {e}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene of one mesh opens, every edit but the categories one runs on
    /// it, and that one says why it cannot.
    #[test]
    fn a_one_mesh_scene_opens_and_skips_only_the_categories_edit() {
        let dir = std::env::temp_dir().join("crust_update_latency_one_mesh");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("one.usda");
        std::fs::write(
            &path,
            "#usda 1.0\ndef Mesh \"only\"\n{\n    int[] faceVertexCounts = [3]\n    int[] faceVertexIndices = [0, 1, 2]\n    point3f[] points = [(0, 0, -2), (1, 0, -2), (0, 1, -2)]\n}\ndef Camera \"cam\"\n{\n    double3 xformOp:translate = (0, 0, 5)\n    uniform token[] xformOpOrder = [\"xformOp:translate\"]\n}\n",
        )
        .expect("write the stage");
        let mut bench = Bench::open(&path, &Named::default()).expect("a one-mesh scene opens");
        assert_eq!(
            bench.missing(Edit::Categories),
            Some("no second mesh to include")
        );
        assert_eq!(bench.missing(Edit::FocalLength), None);
        assert_eq!(bench.missing(Edit::TrivialLight), None);
        bench
            .run(Edit::FocalLength, 0)
            .expect("a focal length edit runs");
    }
}
