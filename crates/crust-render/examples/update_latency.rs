//! The update-latency benchmark (`moho-hydra-parity` design D12): how long an
//! edit takes from the stage to Crust's restarted render, through a host
//! over Moho and the render delegate.
//!
//! ```text
//! cargo run --release -p crust-render --features delegate --example update_latency -- \
//!     [--runs N] --fixture MESHES
//! cargo run --release -p crust-render --features delegate --example update_latency -- \
//!     [--runs N] [--camera PATH] [--linked PATH] <scene>
//! ```
//!
//! The host opens the stage (payloads loaded), keeps its stage scene index,
//! Moho's standard chain with the scene globals filter, and a
//! [`Delegate`] rendering at 64×36. One run:
//!
//! 1. author the edit on the stage (openusd);
//! 2. `apply_pending_updates` on the stage scene index;
//! 3. the delegate's `update()`, which cancels and joins the render, flushes,
//!    brings the scene up to date and restarts the render;
//! 4. stop the clock when `render()` has moved.
//!
//! The four edits are Moho's (`moho-spike`'s `update`): a camera's
//! `focalLength`, its first transform op, a sphere light with trivial
//! collections added, and one whose light link includes one prim added. An
//! added light is removed after its run, untimed. The fixture is Moho's too:
//! a camera with a translate op and N meshes under `/world`, the linked light
//! including `/world/a`. Prints, per edit, the median (the mean of the two
//! middle runs for an even N) and worst, the split and the class.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crust_core::{StageHostFacts, UpdateClass};
use crust_render::delegate::{Delegate, Hooks, UpdateSplit};
use moho::chain::{self, RendererFilter};
use moho::openusd::sdf::{self, Value};
use moho::openusd::usd::{InitialLoadSet, Stage};
use moho::stage::StageSceneIndex;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "usage: update_latency [--runs N] --fixture MESHES\n       update_latency [--runs N] [--camera PATH] [--linked PATH] <scene>";

/// The scope the benchmark adds its lights under, and its render settings.
const LIGHTS: &str = "/UpdateBenchLights";
const SETTINGS: &str = "/UpdateBenchRender/settings";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    FocalLength,
    CameraMove,
    TrivialLight,
    LinkedLight,
}

impl Edit {
    const ALL: [Edit; 4] = [
        Edit::FocalLength,
        Edit::CameraMove,
        Edit::TrivialLight,
        Edit::LinkedLight,
    ];

    fn name(self) -> &'static str {
        match self {
            Edit::FocalLength => "camera focalLength",
            Edit::CameraMove => "camera transform",
            Edit::TrivialLight => "light, trivial collections",
            Edit::LinkedLight => "light linked to one prim",
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
/// `/world`, the first, `/world/a`, the linked light's target.
fn fixture_text(meshes: usize) -> String {
    let mut text =
        String::from("#usda 1.0\ndef Xform \"world\"\n{\n    def Mesh \"a\"\n    {\n    }\n");
    for i in 1..meshes {
        text.push_str(&format!(
            "    def Mesh \"m{i}\"\n    {{\n        int[] faceVertexCounts = [3]\n        int[] faceVertexIndices = [0, 1, 2]\n        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]\n    }}\n"
        ));
    }
    text.push_str(
        "}\ndef Camera \"cam\"\n{\n    float focalLength = 35\n    double3 xformOp:translate = (0, 0, 10)\n    uniform token[] xformOpOrder = [\"xformOp:translate\"]\n}\n",
    );
    text
}

/// The first prim of Hydra type `ty`, depth first.
fn first_of_type(index: &dyn moho::scene_index::SceneIndex, ty: &str) -> Option<sdf::Path> {
    let mut stack = vec![sdf::Path::abs_root()];
    while let Some(path) = stack.pop() {
        if index.prim(&path).ty == ty {
            return Some(path);
        }
        stack.extend(index.children(&path).into_iter().rev());
    }
    None
}

struct Bench {
    stage: Stage,
    index: Rc<StageSceneIndex>,
    facts: StageHostFacts,
    delegate: Delegate,
    camera: sdf::Path,
    target: sdf::Path,
    setup: Duration,
    next_light: usize,
}

impl Bench {
    fn open(path: &Path, camera: Option<&str>, linked: Option<&str>) -> Result<Bench> {
        let started = Instant::now();
        let stage = Stage::builder()
            .schema_registry(crust_core::usd_schema_registry())
            .load(InitialLoadSet::LoadAll)
            .open(path.to_str().ok_or("the scene path is not UTF-8")?)?;
        stage.define_typed_prim(LIGHTS, "Scope")?;
        let index = StageSceneIndex::new();
        index.set_stage(stage.clone());
        let find = |given: Option<&str>, ty: &str| -> Result<sdf::Path> {
            match given {
                Some(path) => Ok(sdf::path(path)?),
                None => first_of_type(index.as_ref(), ty)
                    .ok_or_else(|| format!("no {ty} in the scene").into()),
            }
        };
        let camera = find(camera, "camera")?;
        let target = find(linked, "mesh")?;
        // A small render through the camera edited: the settings the
        // benchmark authors, active as the stage's.
        let settings = stage.define_typed_prim(SETTINGS, "RenderSettings")?;
        settings
            .create_relationship("camera")?
            .set_targets([camera.clone()])?;
        settings
            .create_attribute("resolution", "int2")?
            .set(Value::Vec2i(moho::openusd::gf::Vec2i { x: 64, y: 36 }))?;
        stage.set_stage_metadata("renderSettingsPrimPath", Value::String(SETTINGS.to_owned()))?;
        index.apply_pending_updates();
        let standard = chain::renderer_filters(
            chain::standard(index.clone()),
            &[RendererFilter::scene_globals(|_| {})],
        );
        let facts = StageHostFacts::new(&stage, path, Some(standard.clone()));
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

    fn run(&mut self, edit: Edit, run: usize) -> Result<Sample> {
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
            Edit::CameraMove => self.move_camera(run)?,
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
        }
        let authoring = start.elapsed();
        let (apply, class) = self.update()?;
        if self.delegate.render() == render {
            return Err(format!(
                "{}: the render did not restart ({})",
                edit.name(),
                class.as_str()
            )
            .into());
        }
        let total = start.elapsed();
        let split = self.delegate.last_split();
        if matches!(edit, Edit::TrivialLight | Edit::LinkedLight) {
            self.next_light += 1;
            self.stage.remove_prim(light)?;
            self.update()?;
        }
        Ok(Sample {
            total,
            authoring,
            apply,
            split,
            class,
        })
    }

    /// Sets the camera's first transform op to a value it did not hold.
    fn move_camera(&self, run: usize) -> Result<()> {
        let order = self
            .stage
            .attribute(format!("{}.xformOpOrder", self.camera).as_str())?
            .get::<Value>()?
            .and_then(Value::try_as_token_vec)
            .unwrap_or_default();
        let op = order
            .first()
            .ok_or_else(|| format!("{} has no transform op", self.camera))?;
        let attribute = self
            .stage
            .attribute(format!("{}.{}", self.camera, op).as_str())?;
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
            other => return Err(format!("{}.{op} holds {other:?}", self.camera).into()),
        };
        attribute.set(moved)?;
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

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut runs = 20usize;
    let (mut camera, mut linked, mut fixture, mut scene) = (None, None, None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().ok_or("--runs needs a count")?.parse()?,
            "--camera" => camera = Some(args.next().ok_or("--camera needs a path")?),
            "--linked" => linked = Some(args.next().ok_or("--linked needs a path")?),
            "--fixture" => {
                fixture = Some(
                    args.next()
                        .ok_or("--fixture needs a count")?
                        .parse::<usize>()?,
                );
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
            std::fs::write(&path, fixture_text(meshes))?;
            path
        }
        (None, Some(scene)) => scene,
        _ => return Err("give a scene or --fixture".into()),
    };
    let mut bench = Bench::open(&path, camera.as_deref(), linked.as_deref())?;
    println!("scene: {}", path.display());
    println!(
        "setup (stage, chain, delegate, first commit and render start): {:.2} s, peak {:.2} GiB",
        bench.setup.as_secs_f64(),
        crust_core::peak_memory_bytes().unwrap_or(0) as f64 / (1u64 << 30) as f64
    );
    println!(
        "camera: {}, linked light target: {}, runs: {runs}",
        bench.camera, bench.target
    );
    println!();
    println!(
        "| edit | class | median (ms) | worst (ms) | authoring | apply_pending_updates | stop | flush | commit | restart |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for edit in Edit::ALL {
        let samples: Vec<Sample> = (0..runs)
            .map(|i| bench.run(edit, i))
            .collect::<Result<_>>()?;
        let pick =
            |f: &dyn Fn(&Sample) -> Duration| median(&samples.iter().map(f).collect::<Vec<_>>());
        let worst = samples.iter().map(|s| s.total).max().unwrap_or_default();
        let class = samples.last().map_or("-", |s| s.class.as_str());
        println!(
            "| {} | {class} | {} | {} | {} | {} | {} | {} | {} | {} |",
            edit.name(),
            ms(pick(&|s| s.total)),
            ms(worst),
            ms(pick(&|s| s.authoring)),
            ms(pick(&|s| s.apply)),
            ms(pick(&|s| s.split.stop)),
            ms(pick(&|s| s.split.flush)),
            ms(pick(&|s| s.split.commit)),
            ms(pick(&|s| s.split.restart)),
        );
    }
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
