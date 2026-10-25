//! What the import reads from a stage that is not scene description (design
//! D7), kept in one place so every such read is visible: the layers a scene
//! composed, the files of its root layer stack, and the `invisible` overrides
//! its strongest layer authors. They describe the stage's files and layer
//! specs — a host's business, not a scene index's — so they are read from the
//! stage, never from Moho. A stage-wide fact Moho does carry is read there:
//! the time range's authored-ness, from the absolute root's
//! `__usdStageMetadata` ([`time_range`]).

use std::path::Path;

use moho::data_source::locate;
use moho::locator::Locator;
use moho::openusd::sdf;
use moho::openusd::usd::Stage;
use moho::scene_index::SceneIndex;
use moho::tokens::usd as generic;

/// The identifiers of every layer `stage` composed ([`crate::Scene::layers`]):
/// a file list, not scene description.
pub(in crate::scene::usd_import) fn layer_identifiers(stage: &Stage) -> Vec<String> {
    stage.layer_identifiers().into_iter().collect()
}

/// The identifiers of `stage`'s root layer stack, strongest first
/// ([`crate::scene::Source::layer_stack`]): a file list, not scene description.
pub(in crate::scene::usd_import) fn layer_stack(stage: &Stage) -> Vec<String> {
    stage.layer_stack()
}

/// The stage's `startTimeCode` and `endTimeCode`, from the absolute root's
/// `__usdStageMetadata` in `index`, when both are authored (Moho's M3: Hydra's
/// scene globals give values only).
pub(in crate::scene::usd_import) fn time_range(index: &dyn SceneIndex) -> Option<(f64, f64)> {
    let root = index.prim(&sdf::Path::abs_root()).data?;
    let field = |name: &str| -> Option<f64> {
        let at = |leaf: &str| locate(&root, &Locator::new([generic::STAGE_METADATA, name, leaf]));
        let authored = matches!(at(generic::AUTHORED)?.value()?, sdf::Value::Bool(true));
        match at(generic::VALUE)?.value()? {
            sdf::Value::Double(v) if authored => Some(v),
            _ => None,
        }
    };
    Some((field("startTimeCode")?, field("endTimeCode")?))
}

/// The files behind layer `identifiers`, sorted and without repeats: a layer
/// inside a package is the package file, a relative identifier is anchored
/// to the root layer `root`'s directory, file format arguments are dropped,
/// and an anonymous layer, which has no file, is left out.
pub(in crate::scene::usd_import) fn layer_files(
    identifiers: &std::collections::BTreeSet<String>,
    root: &Path,
) -> Vec<std::path::PathBuf> {
    let dir = root.parent().unwrap_or(Path::new(""));
    let files: std::collections::BTreeSet<std::path::PathBuf> = identifiers
        .iter()
        .filter(|id| !sdf::Layer::is_anonymous_identifier(id))
        .map(|id| {
            let id = id.split(":SDF_FORMAT_ARGS:").next().unwrap_or(id);
            let file = openusd::ar::split_package_relative_path_outer(id)
                .map(|(package, _)| package)
                .unwrap_or_else(|| id.to_owned());
            let file = Path::new(&file);
            if file.is_absolute() {
                file.to_path_buf()
            } else {
                dir.join(file)
            }
        })
        .collect();
    files.into_iter().collect()
}

/// The files of the root layer stack `identifiers`, in their order: each
/// mapped as [`layer_files`] maps it and made absolute against the working
/// directory (links not resolved), an anonymous layer left out, and a
/// package met twice kept once, where it is strongest.
pub(in crate::scene::usd_import) fn stack_files(
    identifiers: &[String],
    root: &Path,
) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for id in identifiers {
        let one = std::collections::BTreeSet::from([id.clone()]);
        for file in layer_files(&one, root) {
            // Absolute, so the same layer stamps the same path whatever the
            // working directory and however the stage path was typed.
            let file = std::path::absolute(&file).unwrap_or(file);
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    files
}

/// The prims the strongest layer of `stage`'s root layer stack — its session
/// layer when it has one, else its root layer — hides with an `over`: a
/// `visibility` spec on an `over` prim whose default or a time sample is
/// `invisible`. What a session or override layer hid of the scene beneath
/// it; a prim the layer itself defines invisible is the scene's own.
pub(in crate::scene::usd_import) fn invisible_overrides(stage: &Stage) -> u32 {
    let strongest = stage.session_layer().unwrap_or_else(|| stage.root_layer());
    let data = strongest.data();
    let invisible = |v: &sdf::Value| matches!(v, sdf::Value::Token(t) if t.as_str() == "invisible");
    let mut count = 0u32;
    for path in data.spec_paths() {
        if !path.is_property() || !path.as_str().ends_with(".visibility") {
            continue;
        }
        let prim = path.prim_path();
        let is_over = matches!(
            data.try_field(&prim, "specifier"),
            Ok(Some(v)) if matches!(&*v, sdf::Value::Specifier(sdf::Specifier::Over))
        );
        if !is_over {
            continue;
        }
        let default = matches!(data.try_field(&path, "default"), Ok(Some(v)) if invisible(&v));
        let sampled = matches!(
            data.try_field(&path, "timeSamples"),
            Ok(Some(v)) if matches!(&*v, sdf::Value::TimeSamples(samples)
                if samples.iter().any(|(_, v)| invisible(v)))
        );
        if default || sampled {
            count = count.saturating_add(1);
        }
    }
    count
}
