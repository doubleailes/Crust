//! Crust's render delegate as a long-lived host holds it (design D1, D9):
//! the scene side ([`crust_core::SceneDelegate`], which reads the host's
//! scene index) with render control around it — the analogue of an
//! `HdRenderPass` and its `HdRenderBuffer`s.
//!
//! The host owns its stage and its stage scene index, and drives everything
//! from its own thread, the one that owns the index (Moho's indices are
//! `Rc`): it edits the stage, sets the time, applies its index's pending
//! updates, then calls [`Delegate::update`], which cancels and joins the
//! running render, brings the scene up to date (in place when the batch
//! allows it) and restarts the render when the scene changed. Renders run on
//! render threads over an `Arc<Renderer>`; what a host polls goes through a
//! [`RenderHandle`], which any thread may hold: [`RenderHandle::snapshot`],
//! [`RenderHandle::aov`], [`RenderHandle::cancel_render`] never wait behind
//! an update.
//!
//! **Panics.** A panic in a render thread ends that render: its first poll
//! reports it, and the next update renders again. A panic in an update drops
//! the scene; the next update commits afresh. This containment needs a build
//! that unwinds: the CLI's release profile aborts, and a host that wants it
//! (Mantle) sets `panic = "unwind"` in its own workspace.

use crate::Assets;
use crust_assets::FileAssets;
use crust_core::{
    AovFilm, AovRequest, Buffer, HostFacts, LightList, RenderControl, RenderOutcome,
    RenderSettings, Renderer, Scene, SceneDelegate, UpdateClass, WorldBuilder,
};
use moho::scene_index::SceneIndexHandle;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use tracing::debug;

/// Why a call did not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not yet: no image has been published, the render has not completed,
    /// or (for [`Delegate::pick`]) the scene no longer holds the geometry
    /// the render drew. Ask again once the next render has started.
    Busy(String),
    /// The scene could not be committed. The last image stays.
    UpdateFailed(String),
    /// crust panicked. The scene is dropped; the next update commits again.
    Panicked(String),
    /// The call itself was wrong: a missing AOV, nothing committed yet.
    InvalidArgument(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Busy(m) | Error::UpdateFailed(m) | Error::InvalidArgument(m) => f.write_str(m),
            Error::Panicked(m) => write!(f, "crust panicked: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// One image of the render as it refines.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Grows with every new image, across renders: a render that restarts
    /// is a new image too. A host that keeps the last one it copied knows
    /// whether to copy again.
    pub generation: u64,
    /// Which render of the delegate the image belongs to, from 1: every
    /// update that changed the scene starts the next one.
    pub render: u64,
    pub width: usize,
    pub height: usize,
    /// `width` × `height` RGBA, rows top-down (image space), alpha 1.
    pub rgba: Vec<f32>,
    /// The samples every pixel has taken, as the render's first sweep
    /// reaches them (1, 2, 4, …): a lower bound once adaptive sampling runs.
    pub spp: u32,
    /// Whether this is the final image of a render that completed.
    pub completed: bool,
    /// The scene's working space (`renderingColorSpace`): what a host
    /// encodes `rgba` from, as `crust render` encodes its PNG preview.
    pub working_space: crust_core::color::Space,
    /// The width of a pixel over its height the image was framed with: 1 for
    /// square pixels.
    pub pixel_aspect: f32,
    /// The time code of the index the scene was committed from, `None` for
    /// its default.
    pub time: Option<f64>,
}

/// One AOV of a completed render.
#[derive(Debug, Clone)]
pub struct Aov {
    pub width: usize,
    pub height: usize,
    /// Components per pixel: 1 for a scalar such as `depth`, 3 or 4 for
    /// colour, as the RenderVar declares.
    pub channels: usize,
    /// `width` × `height` × `channels`, interleaved, rows top-down.
    pub data: Vec<f32>,
}

/// What the camera ray through one pixel of a render meets first
/// ([`Delegate::pick`]), named by where the scene's parts came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The render the pick describes ([`Snapshot::render`]).
    pub render: u64,
    /// The hit geometry's `World` slot.
    pub geom_id: u32,
    /// The prim that emitted the hit geometry: for a hit inside a native
    /// instance or a `PointInstancer`, the instance or the instancer prim.
    /// `None` when the scene records no prim for it.
    pub prim: Option<String>,
    /// The material prim bound to it, `None` for geometry no material prim
    /// fills (an area light's emitter, the fallback material).
    pub material: Option<String>,
}

/// A host's hook for an update's warnings.
pub type WarningsHook = Arc<dyn Fn(&[crust_core::Warning]) + Send + Sync>;

/// What a host hooks into the delegate.
#[derive(Clone, Default)]
pub struct Hooks {
    /// Runs first on each render thread: where a host routes its log.
    pub on_thread: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Runs after each update that committed or changed the scene, with the
    /// scene's warnings.
    pub on_warnings: Option<WarningsHook>,
}

/// What a render left when it returned.
struct Finished {
    buffer: Buffer,
    film: Option<AovFilm>,
    outcome: RenderOutcome,
}

/// A render the host polls.
struct Running {
    /// Which render of the delegate this is, from 1.
    id: u64,
    control: RenderControl,
    /// The products the scene asks for: the AOVs a completed render holds.
    aovs: Arc<AovRequest>,
    working_space: crust_core::color::Space,
    pixel_aspect: f32,
    time: Option<f64>,
    finished: Mutex<Option<Arc<Finished>>>,
    /// The message of a panic that ended the render.
    panicked: Mutex<Option<String>>,
    /// Whether a poll returned the panic already.
    reported: AtomicBool,
}

impl Running {
    /// `Panicked` when a panic ended the render, to the first poll only;
    /// later ones are `Busy` until the next update renders again.
    fn check(&self) -> Result<(), Error> {
        match lock(&self.panicked).clone() {
            Some(_) if self.reported.swap(true, Ordering::SeqCst) => Err(Error::Busy(
                "the render panicked; the next update renders again".into(),
            )),
            Some(message) => Err(Error::Panicked(message)),
            None => Ok(()),
        }
    }
}

/// Locks `m`, through a poisoned lock: what it guards is replaced whole.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The message of a caught panic.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// The render polls, from any thread.
#[derive(Clone, Default)]
pub struct RenderHandle {
    /// The latest render started; kept after it is cancelled or an update
    /// fails, so the host keeps its last image.
    running: Arc<Mutex<Option<Arc<Running>>>>,
}

impl RenderHandle {
    fn running(&self) -> Option<Arc<Running>> {
        lock(&self.running).clone()
    }

    /// Asks the running render to stop, without waiting: one atomic store.
    /// The render returns once the per-pixel advances in flight finish.
    pub fn cancel_render(&self) {
        if let Some(running) = self.running() {
            running.control.cancel();
        }
    }

    /// Which render the delegate last started, from 1; 0 before the first.
    pub fn render(&self) -> u64 {
        self.running().map_or(0, |r| r.id)
    }

    /// The generation [`RenderHandle::snapshot`] would return now, without
    /// copying the image.
    pub fn generation(&self) -> Result<u64, Error> {
        let running = self
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        running.check()?;
        let finished = lock(&running.finished).is_some();
        let published = running.control.generation() + u64::from(finished);
        if published == 0 {
            return Err(Error::Busy("the render has published no image yet".into()));
        }
        Ok(generation(running.id, published))
    }

    /// The latest image of the render: its final image once it returned,
    /// else the latest it published. `Busy` before the first.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        let running = self
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        running.check()?;
        // Read before the image: a stage is reached only once every unit of
        // it published, so an image cloned after a count of N holds N
        // samples everywhere.
        let spp = running.control.max_samples_reached();
        let finished = lock(&running.finished).clone();
        let (published, buffer, completed) = match finished {
            Some(f) => (
                running.control.generation() + 1,
                f.buffer.clone(),
                f.outcome == RenderOutcome::Completed,
            ),
            None => {
                let (published, buffer) = running
                    .control
                    .snapshot()
                    .ok_or_else(|| Error::Busy("the render has published no image yet".into()))?;
                (published, buffer, false)
            }
        };
        let (width, height) = buffer.size();
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let (r, g, b) = buffer.get_rgb(x, y);
                rgba.extend_from_slice(&[r, g, b, 1.0]);
            }
        }
        Ok(Snapshot {
            generation: generation(running.id, published),
            render: running.id,
            width,
            height,
            rgba,
            spp,
            completed,
            working_space: running.working_space,
            pixel_aspect: running.pixel_aspect,
            time: running.time,
        })
    }

    /// Whether the render returned complete. A cancelled render never
    /// converges; one a panic ended counts as converged, so a host stops
    /// redrawing for it.
    pub fn converged(&self) -> bool {
        self.running().is_some_and(|r| {
            lock(&r.panicked).is_some()
                || lock(&r.finished)
                    .as_ref()
                    .is_some_and(|f| f.outcome == RenderOutcome::Completed)
        })
    }

    /// How the latest render returned, `None` while it runs (or before the
    /// first).
    pub fn outcome(&self) -> Option<RenderOutcome> {
        let running = self.running()?;
        lock(&running.finished).as_ref().map(|f| f.outcome)
    }

    /// The AOV named `name` (a RenderVar's layer name, as the products
    /// write it) of the completed render. `Busy` until the render completes.
    pub fn aov(&self, name: &str) -> Result<Aov, Error> {
        let running = self
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        running.check()?;
        let finished = lock(&running.finished)
            .clone()
            .filter(|f| f.outcome == RenderOutcome::Completed)
            .ok_or_else(|| Error::Busy("the render has not completed".into()))?;
        let var = running
            .aovs
            .products
            .iter()
            .flat_map(|p| &p.vars)
            .find(|v| v.name == name)
            .ok_or_else(|| {
                Error::InvalidArgument(format!("the scene's products write no AOV {name:?}"))
            })?;
        let film = finished.film.as_ref().ok_or_else(|| {
            Error::InvalidArgument(format!("the render gathered no AOVs for {name:?}"))
        })?;
        let planes = film.var_channels(&finished.buffer, var);
        let (width, height) = film.dimensions();
        let channels = planes.len();
        let mut data = Vec::with_capacity(width * height * channels);
        for i in 0..width * height {
            data.extend(planes.iter().map(|p| p[i]));
        }
        Ok(Aov {
            width,
            height,
            channels,
            data,
        })
    }
}

/// The committed scene, kept as the renderer every render shares and what
/// an update puts back together with the renderer's parts.
struct Held {
    renderer: Arc<Renderer>,
    /// The products a render writes, as `crust render` selects them.
    aovs: Arc<AovRequest>,
    pixel_aspect: f32,
    /// `World` slot → the material prim filling it.
    material_of_slot: HashMap<u32, String>,
    /// The scene less its camera, world, lights and volumes.
    rest: Scene,
}

impl Held {
    fn of(mut scene: Scene, frame: f64) -> Held {
        let mut aovs = scene.aovs.clone();
        let _ = crate::select_products(&mut aovs, None, frame);
        let material_of_slot = scene
            .provenance
            .iter()
            .flat_map(|p| p.materials())
            .flat_map(|(path, origin)| origin.slots.iter().map(move |&s| (s, path.to_owned())))
            .collect();
        let pixel_aspect = scene.source.lens.as_ref().map_or(1.0, |l| l.pixel_aspect);
        let volumes = std::mem::take(&mut scene.volumes);
        let empty = Scene::new(
            scene.camera,
            WorldBuilder::new().commit(),
            LightList::new(),
            scene.settings,
        );
        let world = std::mem::replace(&mut scene.world, empty.world);
        let lights = std::mem::replace(&mut scene.lights, empty.lights);
        let renderer =
            Renderer::new(scene.camera, world, lights, scene.settings).with_volumes(volumes);
        Held {
            renderer: Arc::new(renderer),
            aovs: Arc::new(aovs),
            pixel_aspect,
            material_of_slot,
            rest: scene,
        }
    }

    /// The scene back together, once no render holds the renderer.
    fn into_scene(self) -> Option<Scene> {
        let Held { renderer, rest, .. } = self;
        let renderer = Arc::into_inner(renderer)?;
        let (camera, world, lights, _, volumes) = renderer.into_parts();
        let mut scene = rest;
        scene.camera = camera;
        scene.world = world;
        scene.lights = lights;
        scene.volumes = volumes.into_regions();
        Some(scene)
    }
}

/// Crust's render delegate over a host's index. Dropping it cancels the
/// render and joins its thread.
pub struct Delegate {
    scene_delegate: SceneDelegate,
    held: Option<Held>,
    handle: RenderHandle,
    hooks: Hooks,
    render: Option<JoinHandle<()>>,
    renders: u64,
    /// What [`Delegate::start`] asked for: the sample count and region every
    /// restart renders at.
    spp: Option<u32>,
    region: Option<crust_core::PixelRect>,
    started: bool,
    /// The loader every commit goes through, keeping its decodes (unless
    /// `CRUST_SCENE_DELTAS=0`, where each commit gets a fresh one).
    assets: Arc<dyn Assets>,
    last_class: Option<UpdateClass>,
}

impl Delegate {
    /// A delegate over `standard`, the host's `moho::chain::standard` over
    /// its stage scene index, whose stage was opened from `path`. Nothing is
    /// committed until the first [`Delegate::update`].
    pub fn new(standard: SceneIndexHandle, path: &Path, hooks: Hooks) -> Delegate {
        let assets: Arc<dyn Assets> = if crust_core::config().scene_deltas {
            Arc::new(FileAssets::new().keep_decoded())
        } else {
            Arc::new(FileAssets::new())
        };
        Delegate::with_assets(standard, path, hooks, assets)
    }

    /// [`Delegate::new`] through `assets`, kept across updates: how a test
    /// brings a loader of its own.
    pub fn with_assets(
        standard: SceneIndexHandle,
        path: &Path,
        hooks: Hooks,
        assets: Arc<dyn Assets>,
    ) -> Delegate {
        let loader: Arc<dyn crust_core::AssetLoader> = assets.clone();
        Delegate {
            scene_delegate: SceneDelegate::new(
                standard,
                path,
                crust_core::UsdImportOptions::default(),
                loader,
            ),
            held: None,
            handle: RenderHandle::default(),
            hooks,
            render: None,
            renders: 0,
            spp: None,
            region: None,
            started: false,
            assets,
            last_class: None,
        }
    }

    /// The polls, for any thread.
    pub fn handle(&self) -> RenderHandle {
        self.handle.clone()
    }

    /// Starts a progressive render of the committed scene at `spp` samples
    /// per pixel (the scene's own without one) over `region` (the whole
    /// frame without one); every update that changes the scene restarts it
    /// so. `InvalidArgument` before the first update.
    pub fn start(
        &mut self,
        spp: Option<u32>,
        region: Option<crust_core::PixelRect>,
    ) -> Result<(), Error> {
        self.stop();
        self.spp = spp;
        self.region = region;
        self.started = true;
        self.launch()
    }

    /// Brings the scene up to date with the index's batched notices
    /// ([`SceneDelegate::update`]): cancels and joins the running render,
    /// updates, and restarts the render when the scene changed. The class
    /// the update reached the scene with.
    pub fn update(&mut self, facts: &dyn HostFacts) -> Result<UpdateClass, Error> {
        self.stop();
        let scene = self.held.take().and_then(Held::into_scene);
        if scene.is_none() {
            let unused = self.assets.sweep();
            if unused > 0 {
                debug!("{unused} decoded asset(s) the scene no longer uses let go");
            }
        }
        let frame = facts.time().unwrap_or(1.0);
        let updated = catch_unwind(AssertUnwindSafe(|| {
            self.scene_delegate.update(facts, scene)
        }));
        let updated = match updated {
            Ok(Ok(updated)) => updated,
            Ok(Err(e)) => return Err(Error::UpdateFailed(e.to_string())),
            Err(payload) => return Err(Error::Panicked(panic_message(&*payload))),
        };
        let class = updated.class;
        self.last_class = Some(class);
        self.held = Some(Held::of(updated.scene, frame));
        if class != UpdateClass::Unchanged {
            self.report();
        }
        if self.started && (class != UpdateClass::Unchanged || self.render_panicked()) {
            self.launch()?;
        }
        Ok(class)
    }

    /// How the last update reached the scene.
    pub fn applied(&self) -> Option<UpdateClass> {
        self.last_class
    }

    /// The committed scene's warnings.
    pub fn warnings(&self) -> &[crust_core::Warning] {
        self.held.as_ref().map_or(&[], |h| &h.rest.warnings)
    }

    /// Cancels the running render and waits for it to stop. The last image
    /// stays.
    pub fn cancel(&mut self) {
        self.stop();
    }

    /// See [`RenderHandle::cancel_render`].
    pub fn cancel_render(&self) {
        self.handle.cancel_render();
    }

    /// What the camera ray through the centre of pixel (`x`, `y`) of the
    /// current render's image meets first — through the lens centre, at
    /// shutter open, before any opacity — with the render's own camera and
    /// image size. The pixel is the snapshot's: rows top-down, counted in
    /// the region the render covers. `Ok(None)` when the ray meets nothing.
    pub fn pick(&self, x: usize, y: usize) -> Result<Option<Pick>, Error> {
        let running = self
            .handle
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        let held = self.held.as_ref().ok_or_else(|| {
            Error::Busy(format!(
                "the scene no longer holds the geometry render {} drew",
                running.id
            ))
        })?;
        let settings = self.settings(held);
        let r = settings.region();
        let (width, height) = (r.x1 - r.x0, r.y1 - r.y0);
        if x >= width || y >= height {
            return Err(Error::InvalidArgument(format!(
                "({x}, {y}) is outside render {}'s {width}x{height} image",
                running.id
            )));
        }
        let (fw, fh) = settings.get_dimensions();
        let (px, py) = (r.x0 + x, r.y0 + y);
        // Image rows go down, the camera's `t` up (as the tracer's raster).
        let s = (px as f32 + 0.5) / fw as f32;
        let t = ((fh - 1 - py) as f32 + 0.5) / fh as f32;
        let ray = held.renderer.camera().get_ray(s, t, [0.5, 0.5], 0.0);
        let hit = held
            .renderer
            .world()
            .intersect(&ray, crust_core::TRACE_T_MIN, f32::INFINITY);
        Ok(hit.map(|h| Pick {
            render: running.id,
            geom_id: h.geom_id,
            prim: held
                .rest
                .provenance
                .as_ref()
                .and_then(|p| p.prim_of_geometry(h.geom_id))
                .map(str::to_owned),
            material: held.material_of_slot.get(&h.geom_id).cloned(),
        }))
    }

    /// See [`RenderHandle::snapshot`].
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.handle.snapshot()
    }

    /// See [`RenderHandle::generation`].
    pub fn generation(&self) -> Result<u64, Error> {
        self.handle.generation()
    }

    /// See [`RenderHandle::render`].
    pub fn render(&self) -> u64 {
        self.handle.render()
    }

    /// See [`RenderHandle::aov`].
    pub fn aov(&self, name: &str) -> Result<Aov, Error> {
        self.handle.aov(name)
    }

    /// See [`RenderHandle::converged`].
    pub fn converged(&self) -> bool {
        self.handle.converged()
    }

    /// See [`RenderHandle::outcome`].
    pub fn outcome(&self) -> Option<RenderOutcome> {
        self.handle.outcome()
    }

    /// The settings a render of `held` runs with: the scene's, at the
    /// sample count and region [`Delegate::start`] asked for.
    fn settings(&self, held: &Held) -> RenderSettings {
        let mut settings = held.renderer.settings();
        if let Some(spp) = self.spp {
            settings = settings.with_samples_per_pixel(spp);
        }
        if let Some(region) = self.region
            && let Ok(s) = settings.with_region(region)
        {
            settings = s;
        }
        settings
    }

    /// Cancels the running render and joins its thread, so the scene is no
    /// longer shared with it.
    fn stop(&mut self) {
        self.handle.cancel_render();
        if let Some(thread) = self.render.take() {
            let _ = thread.join();
        }
    }

    /// Whether a panic ended the last render.
    fn render_panicked(&self) -> bool {
        self.handle
            .running()
            .is_some_and(|r| lock(&r.panicked).is_some())
    }

    /// Hands the scene's warnings to the host.
    fn report(&self) {
        if let (Some(held), Some(hook)) = (&self.held, &self.hooks.on_warnings) {
            hook(&held.rest.warnings);
        }
    }

    /// Starts rendering the scene on a fresh control.
    fn launch(&mut self) -> Result<(), Error> {
        let held = self
            .held
            .as_mut()
            .ok_or_else(|| Error::InvalidArgument("nothing is committed yet".into()))?;
        let settings = {
            let mut settings = held.renderer.settings();
            if let Some(spp) = self.spp {
                settings = settings.with_samples_per_pixel(spp);
            }
            if let Some(region) = self.region {
                settings = settings
                    .with_region(region)
                    .map_err(|e| Error::InvalidArgument(e.to_string()))?;
            }
            settings
        };
        if let Some(renderer) = Arc::get_mut(&mut held.renderer) {
            renderer.reconfigure(settings);
        }
        self.renders += 1;
        let running = Arc::new(Running {
            id: self.renders,
            control: RenderControl::new(),
            aovs: held.aovs.clone(),
            working_space: held.rest.working_space,
            pixel_aspect: held.pixel_aspect,
            time: held.rest.time,
            finished: Mutex::new(None),
            panicked: Mutex::new(None),
            reported: AtomicBool::new(false),
        });
        *lock(&self.handle.running) = Some(running.clone());
        let renderer = held.renderer.clone();
        let on_thread = self.hooks.on_thread.clone();
        let thread = std::thread::Builder::new()
            .name(format!("crust-render-{}", running.id))
            .spawn(move || {
                if let Some(enter) = &on_thread {
                    enter();
                }
                // The AOVs `crust render` requests for the stage: none
                // without products, so the beauty is the CLI's either way.
                let request = (!running.aovs.products.is_empty()).then_some(&*running.aovs);
                // A panic on a rayon worker reaches this thread through
                // rayon's join, and stops here.
                let rendered = catch_unwind(AssertUnwindSafe(|| {
                    renderer.render_with_control(true, None, request, &running.control)
                }));
                match rendered {
                    Ok(rendered) => {
                        *lock(&running.finished) = Some(Arc::new(Finished {
                            buffer: rendered.buffer,
                            film: rendered.film,
                            outcome: rendered.outcome,
                        }))
                    }
                    Err(payload) => *lock(&running.panicked) = Some(panic_message(&*payload)),
                }
            })
            .map_err(|e| Error::InvalidArgument(format!("cannot start the render thread: {e}")))?;
        self.render = Some(thread);
        Ok(())
    }
}

impl Drop for Delegate {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The delegate-wide generation of a render's `published`-th image: a render
/// publishes fewer than 2^32 times, and its id keeps the count growing
/// across restarts.
fn generation(render: u64, published: u64) -> u64 {
    (render << 32) | published.min(u64::from(u32::MAX))
}
