//! Import parity: every import compared with a baseline recorded from Crust's
//! direct importer (the `moho-import` change, spec `import-parity`).
//!
//! Compiled only with the test-only `parity` feature, which crust-core's and
//! crust-render's tests turn on through a dev-dependency. With it, every
//! successful [`load_scene`](crate::scene) ends in [`check_import`]: the scene's
//! [`digest`] is compared with the baseline recorded for that stage and those
//! options under `crust-core/tests/data/import_baselines/`, and any difference
//! panics on the importing thread, naming the stage and the first line that
//! differs. So every import a test performs — a committed sample, a fixture a
//! test writes, a live session's re-import — is checked, without a call site
//! having to ask.
//!
//! - `CRUST_RECORD_BASELINES=1` writes the baselines instead of comparing. It
//!   recorded them from the direct importer before anything migrated; after
//!   that it is a behavioural change of its own, never a way to accept a
//!   migration's difference.
//! - `CRUST_PARITY_IMAGES=1` also imports the stage a second time and renders
//!   it at 16 spp with the indirect clamp off, comparing a hash of its pixels
//!   (the `.image` baseline). With it, `CRUST_RECORD_BASELINES=1` records only
//!   the images; the digests are still compared. Meant for `cargo test --release`: an unoptimised
//!   build renders too slowly for every import of the suite.
//!
//! A baseline is keyed by a hash of every layer the import composed (paths and
//! text, with the stage's directory written as `$DIR`), the import options, the
//! environment switches and the importing test (its thread's name), so a
//! fixture written to a fresh temporary directory finds its baseline again.
//! Tests run on their own named threads, as libtest runs them by default; with
//! `--test-threads=1` they share one, and the keys move.

use std::cell::Cell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::rt_world::Fnv;
use crate::scene::Scene;

/// Where the baselines live.
pub fn baseline_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/import_baselines")
}

thread_local! {
    /// Set while [`check_import`] re-imports a stage to render it, so the
    /// nested import is not itself checked.
    static NESTED: Cell<bool> = const { Cell::new(false) };
}

/// Whether `CRUST_RECORD_BASELINES` asks to record rather than compare.
fn recording() -> bool {
    std::env::var("CRUST_RECORD_BASELINES").is_ok_and(|v| v == "1")
}

/// Whether `CRUST_PARITY_IMAGES` asks for the 16 spp image hash too.
fn with_images() -> bool {
    std::env::var("CRUST_PARITY_IMAGES").is_ok_and(|v| v == "1")
}

/// Rewrites the machine- and run-specific paths in `text`: the stage's
/// directory as `$DIR`, the workspace as `$ROOT`, the temporary directory as
/// `$TMP`, so a digest recorded on one machine compares on another.
pub fn normalise(text: &str, stage: &Path) -> String {
    let mut out = text.to_owned();
    let mut dirs: Vec<(String, &str)> = Vec::new();
    if let Some(dir) = stage.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(abs) = std::path::absolute(dir) {
            dirs.push((abs.display().to_string(), "$DIR"));
        }
        if let Ok(canon) = dir.canonicalize() {
            dirs.push((canon.display().to_string(), "$DIR"));
        }
        dirs.push((dir.display().to_string(), "$DIR"));
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if let Ok(root) = root.canonicalize() {
        dirs.push((root.display().to_string(), "$ROOT"));
    }
    let tmp = std::env::temp_dir();
    if let Ok(canon) = tmp.canonicalize() {
        dirs.push((canon.display().to_string(), "$TMP"));
    }
    dirs.push((tmp.display().to_string(), "$TMP"));
    // Longest first, so a directory inside another is replaced before it.
    dirs.sort_by_key(|(d, _)| std::cmp::Reverse(d.len()));
    for (dir, name) in dirs {
        if dir.len() > 1 {
            out = out.replace(&dir, name);
        }
    }
    out
}

/// The scene's import-parity digest, as stable text: one fact per line, so
/// two digests are compared by their first differing line.
///
/// It covers the geometry in id order ([`crate::World`]'s pieces, materials,
/// flags and side tables), the light list, the volumes, the camera, the
/// settings, the AOV request, the warnings in order, the provenance, the
/// layers and the scene's [`crate::scene::Source`]. A material's parameters
/// and a light's radiance are not in it — no material or light describes
/// itself — and are covered by the 16 spp image (`CRUST_PARITY_IMAGES=1`).
pub fn digest(scene: &Scene, stage: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "time: {:?}", scene.time);
    let _ = writeln!(out, "working space: {}", scene.working_space.name());
    let _ = writeln!(out, "camera path: {:?}", scene.camera_path);
    let _ = writeln!(out, "camera: {:?}", scene.camera);
    let _ = writeln!(out, "settings prim: {:?}", scene.settings_prim);
    let _ = writeln!(out, "settings: {:?}", scene.settings);
    let _ = writeln!(out, "aovs: {:?}", scene.aovs);
    let _ = writeln!(out, "pass: {:?}", scene.pass);
    let _ = writeln!(out, "source: {:?}", scene.source);
    for layer in &scene.layers {
        let _ = writeln!(out, "layer: {}", layer.display());
    }
    for w in &scene.warnings {
        let _ = writeln!(out, "warning: {w:?}");
    }
    let lights = &scene.lights;
    let _ = writeln!(out, "lights: {}", lights.count());
    for i in 0..lights.count() {
        let kind = match lights.light(i) {
            crate::light::LightKind::Area(_) => "area",
            crate::light::LightKind::Distant(_) => "distant",
            crate::light::LightKind::Dome(_) => "dome",
        };
        let _ = writeln!(
            out,
            "light {i}: {kind} name {:?} lpe {:?} pmf {:?} shadow {:?} restricted {} nee-only {} \
             twinned {} gains {:?}",
            lights.name(i),
            lights.lpe_tag(i),
            lights.pmf(i),
            lights.shadow_mask(i),
            lights.restricted(i),
            lights.nee_only(i),
            lights.twinned(i),
            lights.lobe_gains(i),
        );
    }
    let _ = writeln!(out, "volumes: {}", scene.volumes.len());
    for (i, v) in scene.volumes.iter().enumerate() {
        let _ = writeln!(
            out,
            "volume {i}: sigma_s {:?} sigma_a {:?} g {:?} emission {:?} mask {:?} class {} \
             homogeneous {}",
            v.sigma_s,
            v.sigma_a,
            v.g,
            v.emission,
            v.mask,
            v.light_class,
            v.is_homogeneous(),
        );
    }
    scene.world.parity_digest(&mut out);
    if let Some(p) = &scene.provenance {
        p.parity_digest(&mut out);
    }
    normalise(&out, stage)
}

/// `text` with each native prototype's number written `#`
/// (`/__Prototype_12/geo` → `/__Prototype_#/geo`): the one listed parity
/// exception (spec `import-parity`). openusd numbers a stage's prototypes in
/// the order something first asks for them, and the scene index's population
/// walk asks in another order than the direct importer's walk did, so the
/// same prototype gets another number. The number is an artifact of the
/// composition, not of the scene — USD gives prototype names no stability —
/// and a streamed import numbers each chunk's stage from 0, so the number
/// cannot even be mapped between two imports; only the digit run is written
/// out, everything else in the path is compared.
pub fn without_prototype_numbers(text: &str) -> String {
    const MARK: &str = "__Prototype_";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARK) {
        let after = &rest[at + MARK.len()..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        out.push_str(&rest[..at + MARK.len()]);
        if digits > 0 {
            out.push('#');
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    out
}

/// The first line where `actual` differs from `expected`, with its number;
/// `None` when they are equal. Prototype numbers are not compared
/// ([`without_prototype_numbers`]).
pub fn first_difference(expected: &str, actual: &str) -> Option<String> {
    let (expected, actual) = (
        without_prototype_numbers(expected),
        without_prototype_numbers(actual),
    );
    let mut a = expected.lines();
    let mut b = actual.lines();
    let mut n = 1;
    loop {
        match (a.next(), b.next()) {
            (None, None) => return None,
            (x, y) if x == y => n += 1,
            (x, y) => {
                return Some(format!(
                    "line {n}:\n  baseline: {}\n  import:   {}",
                    x.unwrap_or("<end>"),
                    y.unwrap_or("<end>")
                ));
            }
        }
    }
}

/// The key a stage's baselines are stored under. Its inputs: every layer the
/// import composed (its path and text, directories normalised), the import
/// options and the environment switches ([`crate::config`]); and who imported
/// it — the test, by its thread's name, or for a `crust` process the command
/// line — since a host's asset loader and colour config are not in the
/// options. A test importing the same files twice numbers the imports.
pub fn key(stage: &Path, options: &crate::UsdImportOptions, scene: &Scene) -> String {
    let mut hash = Fnv::default();
    let mut layers: Vec<&Path> = scene.layers.iter().map(PathBuf::as_path).collect();
    if !layers.contains(&stage) {
        layers.push(stage);
    }
    for layer in layers {
        hash.bytes(normalise(&layer.display().to_string(), stage).as_bytes());
        let text = std::fs::read(layer).unwrap_or_default();
        hash.bytes(normalise(&String::from_utf8_lossy(&text), stage).as_bytes());
    }
    hash.bytes(normalise(&format!("{options:?}"), stage).as_bytes());
    hash.bytes(format!("{:?}", crate::config()).as_bytes());
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    hash.bytes(thread.as_bytes());
    if thread == "main" {
        let args: Vec<String> = std::env::args().skip(1).collect();
        hash.bytes(normalise(&args.join(" "), stage).as_bytes());
    }
    let content = hash.0;
    let nth = SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        let n = seen.entry(content).or_insert(0u32);
        *n += 1;
        *n
    });
    let name = stage
        .file_name()
        .map(|n| {
            n.to_string_lossy()
                .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        })
        .unwrap_or_default();
    format!("{name}-{content:016x}-{nth}")
}

thread_local! {
    /// How many times this thread imported each key's inputs.
    static SEEN: std::cell::RefCell<std::collections::HashMap<u64, u32>> = Default::default();
}

/// The 16 spp image of a freshly imported `scene`, indirect clamp off, as a
/// hash of its pixels.
fn image_hash(scene: Scene) -> String {
    let settings = scene
        .settings
        .with_samples_per_pixel(16)
        .with_indirect_clamp(0.0);
    let (w, h) = settings.get_dimensions();
    let renderer = crate::Renderer::new(scene.camera, scene.world, scene.lights, settings)
        .with_volumes(scene.volumes);
    let buffer = renderer.render();
    let mut hash = Fnv::default();
    for y in 0..h {
        for x in 0..w {
            hash.floats(&buffer.get_pixel(x, y).to_array());
        }
    }
    format!("{w}x{h} 16spp clamp-off {:016x}\n", hash.0)
}

/// Compares `scene`, just imported from `stage` with `options`, with its
/// baselines, or records them (module docs). `reimport` imports the stage
/// again, for the image.
///
/// # Panics
/// On any difference from the baseline, or a missing baseline: this is a
/// test-only check, and a panic fails the test that imported the stage.
pub fn check_import(
    stage: &Path,
    options: &crate::UsdImportOptions,
    scene: &Scene,
    reimport: &dyn Fn() -> Result<Scene, crate::Error>,
) {
    if NESTED.with(Cell::get) {
        return;
    }
    let dir = baseline_dir();
    let key = key(stage, options, scene);
    let mut files = vec![(dir.join(format!("{key}.digest")), digest(scene, stage))];
    if with_images() {
        NESTED.with(|n| n.set(true));
        let again = reimport();
        NESTED.with(|n| n.set(false));
        match again {
            Ok(again) => files.push((dir.join(format!("{key}.image")), image_hash(again))),
            Err(e) => panic!(
                "parity: re-importing {} for its image failed: {e}",
                stage.display()
            ),
        }
    }
    for (file, actual) in files {
        // With images on, the digests are compared, never recorded: the image
        // run (an optimised build) records only the images, and checks that it
        // imports exactly what the unoptimised run recorded.
        let is_image = file.extension().is_some_and(|e| e == "image");
        if recording() && (is_image || !with_images()) {
            if let Err(e) =
                std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, actual))
            {
                panic!("parity: cannot record {}: {e}", file.display());
            }
            continue;
        }
        let Ok(expected) = std::fs::read_to_string(&file) else {
            panic!(
                "parity: no baseline {} for {} — record the direct importer's with \
                 CRUST_RECORD_BASELINES=1",
                file.display(),
                stage.display()
            );
        };
        if let Some(diff) = first_difference(&expected, &actual) {
            panic!(
                "parity: importing {} differs from its baseline {}, {diff}",
                stage.display(),
                file.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_difference_is_named() {
        let a = "camera: x\ngeom 0: a\ngeom 1: b\n";
        let b = "camera: x\ngeom 0: b\ngeom 1: a\n";
        let diff = first_difference(a, b).expect("they differ");
        assert!(diff.starts_with("line 2:"), "{diff}");
        assert!(
            diff.contains("geom 0: a") && diff.contains("geom 0: b"),
            "{diff}"
        );
        assert_eq!(first_difference(a, a), None);
        assert!(first_difference(a, "camera: x\n").is_some());
    }

    /// Two scenes with the same two spheres attached in the other order.
    fn two_spheres(swapped: bool) -> Scene {
        use crate::rt::Geometry;
        let mut world = crate::WorldBuilder::new();
        let mut spheres = [
            Geometry::Sphere {
                center: glam::Vec3A::ZERO,
                radius: 1.0,
            },
            Geometry::Sphere {
                center: glam::Vec3A::X,
                radius: 0.5,
            },
        ];
        if swapped {
            spheres.reverse();
        }
        for s in spheres {
            world.attach(
                s,
                std::sync::Arc::new(crate::OpenPBR::diffuse(glam::Vec3A::splat(0.5))),
            );
        }
        let camera = crate::Camera::new(
            glam::Vec3A::new(0.0, 0.0, 5.0),
            glam::Vec3A::ZERO,
            glam::Vec3A::Y,
            40.0,
            1.0,
            0.0,
            5.0,
        );
        Scene::new(
            camera,
            world.commit(),
            crate::LightList::new(),
            Default::default(),
        )
    }

    #[test]
    fn a_reordered_geometry_is_named_by_its_id() {
        let stage = Path::new("scene.usda");
        let a = digest(&two_spheres(false), stage);
        let b = digest(&two_spheres(true), stage);
        let diff = first_difference(&a, &b).expect("the order differs");
        assert!(diff.contains("geom 0:"), "{diff}");
        assert_eq!(
            first_difference(&a, &digest(&two_spheres(false), stage)),
            None
        );
    }

    #[test]
    fn prototype_numbers_are_not_compared() {
        assert_eq!(
            without_prototype_numbers("/__Prototype_83/MATERIAL /__Prototype_/x __Prototype_7"),
            "/__Prototype_#/MATERIAL /__Prototype_/x __Prototype_#"
        );
        assert_eq!(
            first_difference("/__Prototype_1/a\n", "/__Prototype_0/a\n"),
            None
        );
        assert!(first_difference("/__Prototype_1/a\n", "/__Prototype_0/b\n").is_some());
    }

    #[test]
    fn paths_are_written_relative_to_the_stage() {
        let stage = std::env::temp_dir().join("parity-test/scene.usda");
        let text = format!("layer: {}", stage.display());
        assert_eq!(normalise(&text, &stage), "layer: $DIR/scene.usda");
    }
}
