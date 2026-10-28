//! The batch host (design D1): opens the stage, builds Moho's standard chain
//! over it at the import's time, answers what only the stage knows
//! ([`stage_facts`]), and hands the index to the delegate's import
//! ([`crate::scene::hydra`]). `crust render`, `check`, `diagnostic` and
//! `light-paths` import once and render. `crust ls` lists what a stage holds
//! ([`listing`]), and in-place rebuilds of a scene still read masked stages
//! ([`live`]).

use moho::{openusd, openusd_schemas};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use moho::chain;
use moho::scene_index::SceneIndexHandle;
use moho::stage::StageSceneIndex;
#[cfg(feature = "mmap")]
use openusd::ar::DefaultResolver;
use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, SchemaRegistry, Stage, StageBuilder, TimeCode};
use tracing::debug;

use super::hydra;
use crate::scene::{AssetLoader, Scene};

pub(in crate::scene) mod listing;
mod live;
pub(in crate::scene) mod stage_facts;

pub(crate) use listing::{list_prims, list_records};
pub(crate) use live::{prim_frames, rebuild};

/// Moho's standard chain over `stage`'s scene index, at `time` (`None`
/// reads at the index's own time, time code 0, as a new
/// `UsdImagingStageSceneIndex`).
pub(in crate::scene) fn standard_chain(stage: &Stage, time: Option<f64>) -> SceneIndexHandle {
    let index = StageSceneIndex::new();
    if let Some(t) = time {
        index.set_time(TimeCode::new(t));
    }
    index.set_stage(stage.clone());
    chain::standard(index)
}

/// The delegate's input over `stage`: its standard chain with the
/// delegate's renderer filters. For a reader handed a stage of its own (an
/// in-place rebuild, `crust ls`, a test).
pub(in crate::scene) fn input_chain(stage: Stage, time: Option<f64>) -> hydra::index::Input {
    hydra::index::Input::over(standard_chain(&stage, time))
}

pub(crate) fn load_scene(
    path: &Path,
    assets: &dyn AssetLoader,
    options: &crate::UsdImportOptions,
) -> Result<Scene, crate::Error> {
    let valid = hydra::ValidOptions::check(options)?;
    // Every coded warning this import raises, on this thread, is recorded
    // here and handed back on the scene.
    let warning_scope = crate::WarningScope::enter();
    let import_start = Instant::now();

    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;

    // One stage, payloads loaded, read through Moho (design D4).
    let open_start = Instant::now();
    let stage = open_stage(path, path_str)?;
    let facts = stage_facts::StageFacts::new(&stage);
    let checks_elapsed = options.stage_checks.then(|| {
        let started = Instant::now();
        stage_facts::stage_checks(&stage);
        started.elapsed()
    });
    let standard = standard_chain(&stage, options.frame);
    let time_range = stage_facts::time_range(&stage, standard.as_ref());
    let layers = stage_facts::layer_files(
        &stage_facts::layer_identifiers(&stage).into_iter().collect(),
        path,
    );
    let layer_stack = stage_facts::stack_files(&stage_facts::layer_stack(&stage), path);
    let invisible_overrides = Some(stage_facts::invisible_overrides(&stage));
    let keep = options.skip_stage_teardown;
    let host = hydra::Host {
        standard,
        facts: &facts,
        path,
        time_range,
        layers,
        layer_stack,
        invisible_overrides,
        import_start,
        open_start,
        checks_elapsed,
        warning_scope,
        release: Box::new(move || release_stage(stage, keep)),
    };
    hydra::import(host, assets, options, valid)
}

/// `Stage::prim` for a path that is already an `sdf::Path`.
///
/// openusd 0.7 widened the argument to any path-like value, so the call now
/// returns a `Result` whose error is a *parse* failure. Every caller here
/// hands it a path that was parsed or composed already, which is why the
/// error arm is unreachable rather than merely unlikely. A `Prim` handle
/// asserts nothing about composed content either way — querying it is what
/// finds out whether a prim is there.
pub(in crate::scene) fn prim_at(stage: &Stage, path: sdf::Path) -> Prim {
    stage
        .prim(path)
        .expect("an already-parsed sdf::Path cannot fail to parse")
}

/// Drops a stage the traversal is done with, or — for a single-stage import
/// whose host asked for it (`UsdImportOptions::skip_stage_teardown`) — leaves
/// it allocated, since freeing openusd's index cache is a long tail of small
/// deallocations (45 s on ALab) that a render-and-exit process never needs.
fn release_stage(stage: Stage, keep: bool) {
    if keep {
        debug!("Leaving the final composed stage allocated (skip_stage_teardown)");
        std::mem::forget(stage);
    } else {
        drop(stage);
    }
}

/// The typed schemas' registry every stage the import opens carries: the
/// families crust-core enables on openusd-schemas (`geom`, `lux`, `shade`,
/// `render`, `skel`, `vol`, `proc`, `media`). Built once per process. A stage
/// that authors the scene (`crust mcp`'s session) opens with this one, so
/// it resolves a declared type and a schema fallback as the import does.
pub fn usd_schema_registry() -> Arc<SchemaRegistry> {
    openusd_schemas::schema_registry()
}

/// `Stage::builder()` carrying the typed schemas' registry, and reading files
/// through memory mappings when the `mmap` feature is built in, unless
/// `CRUST_USD_MMAP=0`. Every stage the import
/// reads must open through this: a stage without the registry knows only the
/// core `usd` family, so every typed `get` (`Mesh`, `SphereLight`,
/// `Settings`, …) answers `None` and no schema fallback resolves — the scene
/// would import as empty, without an error.
pub(in crate::scene) fn stage_builder() -> StageBuilder {
    let builder = Stage::builder().schema_registry(usd_schema_registry());
    #[cfg(feature = "mmap")]
    if crate::config().usd_mmap {
        return builder.resolver(mapping_resolver());
    }
    builder
}

/// openusd's filesystem resolver, serving every file from a read-only memory
/// mapping: a layer holds no copy of its file, and only the pages a read
/// touches are loaded.
// The crate's one `unsafe` outside a test: `map_files` is an `unsafe fn` so
// that its caller states the promise below; it does nothing unsafe itself.
#[cfg(feature = "mmap")]
#[allow(unsafe_code)]
fn mapping_resolver() -> DefaultResolver {
    // SAFETY: `map_files` asks that no mapped file be modified while anything
    // read from it is alive — a layer, a package index, an asset view. Here
    // that is only the stage: the import copies every value out (`sdf::Value`
    // owns its data) and takes no asset bytes from openusd, so nothing read
    // from a mapping outlives the stage that read it, and every stage this
    // builder opens lives for one import, or one `crust ls` listing. The final
    // stage a render leaves allocated (`skip_stage_teardown`) is never read
    // again. Within that window the promise is the user's, as it is for any
    // file a program has open: a USD file rewritten in place *during* the
    // import can fault the process. Writers that replace a file by renaming
    // over it (openusd's own `Layer::save`) leave the mapped bytes intact.
    // The `crust mcp` session, whose authoring stage lives as long as the
    // session, opens it with its own plain builder and never maps.
    unsafe { DefaultResolver::new().map_files() }
}

/// Opens the stage with payloads loaded.
pub(in crate::scene) fn open_stage(path: &Path, path_str: &str) -> Result<Stage, crate::Error> {
    let started = Instant::now();
    let stage = stage_builder()
        .load(InitialLoadSet::LoadAll)
        .open(path_str)
        .map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
    debug!("Composed {} in {:?}", path.display(), started.elapsed());
    Ok(stage)
}

#[cfg(all(test, feature = "mmap"))]
mod tests {
    use super::*;
    use openusd::usd::PrimPredicate;

    /// An attribute's name, default value and time samples.
    type Read = (String, Option<sdf::Value>, Option<sdf::TimeSampleMap>);

    /// Every prim of `stage`, with each authored attribute read.
    fn contents(stage: &Stage) -> Vec<(String, Vec<Read>)> {
        let mut paths = Vec::new();
        stage
            .traverse(PrimPredicate::DEFAULT_PROXIES, |p| paths.push(p.clone()))
            .expect("traverses");
        paths
            .into_iter()
            .map(|path| {
                let prim = prim_at(stage, path.clone());
                let attrs = prim
                    .authored_attributes()
                    .expect("lists attributes")
                    .iter()
                    .map(|a| {
                        let value = a.get::<sdf::Value>().expect("reads");
                        (
                            a.name().to_string(),
                            value,
                            a.time_samples().expect("reads"),
                        )
                    })
                    .collect();
                (path.to_string(), attrs)
            })
            .collect()
    }

    /// A stage read through memory mappings (`CRUST_USD_MMAP`) holds exactly
    /// what one read into memory does — on a `.usdc`, whose values decode
    /// from the layer's bytes on demand, so from the mapping itself.
    #[test]
    fn a_mapped_usdc_reads_as_a_copied_one() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/PointInstancedMedCity.usd"
        );
        let registry = openusd_schemas::schema_registry;
        let mapped = Stage::builder()
            .schema_registry(registry())
            .resolver(mapping_resolver())
            .open(path)
            .expect("opens mapped");
        let copied = Stage::builder()
            .schema_registry(registry())
            .open(path)
            .expect("opens copied");
        let mapped = contents(&mapped);
        let read = |(_, v, t): &&Read| v.is_some() || t.is_some();
        let n = mapped.iter().flat_map(|(_, a)| a).filter(read).count();
        assert!(n > 50, "the sample reads {n} values");
        assert!(mapped == contents(&copied));
    }
}
