//! What the import reads from a stage that is not scene description (design
//! D7), kept in one place so every such read is visible: the layers a scene
//! composed, the files of its root layer stack, the `invisible` overrides
//! its strongest layer authors, a material's `.mtlx` reference, an arc
//! that composes nothing ([`mtlx_reference`]), and the specs a material
//! composes from elsewhere ([`foreign_spec_paths`]). They describe the stage's files and layer
//! specs — a host's business, not a scene index's — so they are read from the
//! stage, never from Moho. A stage-wide fact Moho does carry is read there:
//! the time range's authored-ness, from the absolute root's
//! `__usdStageMetadata` ([`time_range`]).

use moho::openusd;
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

/// Why the host's pass is not a `RenderPass`, for its error: no prim there, or
/// the USD type the prim has. The USD type name is what the user authored and
/// reads in the error, and the scene index carries Hydra types only (a
/// `Scope` or an `Xform` has none), so it is read from the stage.
pub(in crate::scene::usd_import) fn not_a_pass(stage: &Stage, pass: &sdf::Path) -> String {
    let prim = super::super::prim_at(stage, pass.clone());
    if !prim.is_valid().unwrap_or(false) {
        return "there is no prim at this path on the stage".to_string();
    }
    match prim.type_name().ok().flatten() {
        Some(t) if !t.as_str().is_empty() => format!("a {} is not a RenderPass", t.as_str()),
        _ => "a typeless prim is not a RenderPass".to_string(),
    }
}

/// The `.mtlx` file and material node a `Material` prim references, if any.
///
/// Read off the *layer specs* rather than the composed prim, because there is
/// nothing composed to read: without a MaterialX file-format plugin the
/// reference resolves to no layer at all, so the arc survives only as authored
/// metadata. The first `.mtlx` found wins.
///
/// Not scene description the index can give (design D7): the arc to a file no
/// file-format plugin reads composes nothing, so Moho has nothing to carry.
/// It moves to the composed shader prims when `mtlx-format-plugin` lands.
pub(in crate::scene::usd_import) fn mtlx_reference(
    stage: &Stage,
    mat_path: &sdf::Path,
) -> Option<(std::path::PathBuf, String)> {
    // The composed stage path is not where the reference is *authored*: a shot
    // layer referencing `teapot.usda` sees the material at
    // `/World/Teapot/Looks/TeapotCeramic`, while the arc is authored at
    // `/Teapot/Looks/TeapotCeramic` inside the asset layer. Only the prim's
    // composition graph knows both, so the candidate spec paths come from its
    // nodes rather than from the stage path.
    let mut paths = vec![mat_path.clone()];
    if let Ok(graph) = super::super::prim_at(stage, mat_path.clone())
        .prim_index()
        .graph()
    {
        for node in graph.all_nodes() {
            let p = node.path().clone();
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }

    for id in stage.layer_identifiers() {
        let Some(layer) = stage.layer(&id) else {
            continue;
        };
        // Every candidate path against every layer. The cross product is a
        // handful of lookups — a material's graph has a few nodes and a stage
        // a few layers — and it sidesteps having to map a node's `LayerId`
        // back to an identifier, which openusd does not expose.
        for path in &paths {
            let Ok(Some(spec)) = layer.prim(path.clone()) else {
                continue;
            };
            let Ok(Some(sdf::Value::ReferenceListOp(list))) = spec.field("references") else {
                continue;
            };
            // Every list-op bucket, because which one a reference lands in
            // depends on whether it was authored as `prepend`, `append` or a
            // bare assignment — and all three mean "this material is that
            // document".
            let items = list
                .explicit_items()
                .iter()
                .chain(list.prepended_items().iter())
                .chain(list.appended_items().iter())
                .chain(list.added_items().iter());
            for r in items {
                if !r.asset_path.to_ascii_lowercase().ends_with(".mtlx") {
                    continue;
                }
                // Anchored against the *authoring layer's* directory, the same
                // rule `asset_path` follows for textures and for the
                // same reason: `Looks/teapot_ceramic_ldX.mtlx` is relative to
                // `teapot.usda`, which need not be the stage root.
                let base = std::path::Path::new(&id).parent()?.to_path_buf();
                let file = base.join(&r.asset_path);
                let node = crate::materialx::material_node_of(r.prim_path.as_str())
                    .unwrap_or_default()
                    .to_string();
                return Some((file, node));
            }
        }
    }
    None
}

/// The paths of the specs the prims at `paths` compose from outside `root`:
/// an `inherits`, `specializes` or `references` arc's target, a referenced
/// layer's prim. Composition arcs are not scene description the index gives
/// (design D7): they say which layer specs an in-place edit of a material
/// would reach (`material_reads`, [`crate::live`]).
pub(in crate::scene::usd_import) fn foreign_spec_paths(
    stage: &Stage,
    paths: &[sdf::Path],
    root: &sdf::Path,
) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        let prim = super::super::prim_at(stage, path.clone());
        for site in prim.prim_stack().unwrap_or_default() {
            if !site.path.has_prefix(root) {
                out.push(site.path.to_string());
            }
        }
    }
    out
}

/// Why the collection `name` of the prim at `path` has no membership: the
/// error openusd's membership query gives, which the scene index (Hydra's
/// collections schema) does not carry. For the `light_link.unreadable_collection`
/// diagnostic only (design D7).
pub(in crate::scene::usd_import) fn collection_error(
    stage: &Stage,
    path: &sdf::Path,
    name: &str,
) -> Option<String> {
    let prim = super::super::prim_at(stage, path.clone());
    moho::openusd::usd::CollectionAPI::from_prim_unchecked(prim, name)
        .membership_query()
        .err()
        .map(|e| e.to_string())
}

/// Why the prim at `path` has no local transform at `time`: the error
/// openusd's `XformQuery` gives for its op stack, which the scene index
/// (Hydra's xform schema) does not carry. For the `xform.uncomposable`
/// diagnostic only (design D7).
pub(in crate::scene::usd_import) fn xform_error(
    stage: &Stage,
    path: &sdf::Path,
    time: Option<f64>,
) -> Option<String> {
    use moho::openusd_schemas::geom::XformQuery;
    let prim = super::super::prim_at(stage, path.clone());
    let time = time.map(moho::openusd::usd::TimeCode::new);
    match XformQuery::for_prim(&prim) {
        Err(e) => Some(e.to_string()),
        Ok(query) => query.local_transform(time).err().map(|e| e.to_string()),
    }
}

/// The paths of the prim specs `path` composes from (`prim_stack`), prim
/// sites only: which layers' specs an edit would reach, for provenance
/// (design D7) — not scene description.
pub(in crate::scene::usd_import) fn prim_stack_paths(
    stage: &Stage,
    path: &sdf::Path,
) -> Vec<String> {
    super::super::prim_at(stage, path.clone())
        .prim_stack()
        .unwrap_or_default()
        .into_iter()
        .filter(|site| site.path.is_prim())
        .map(|site| site.path.to_string())
        .collect()
}

/// Whether the prim at `path` is instanceable with a composition arc but has
/// no prototype: for `instancing.instanceable_without_prototype` only, a
/// state the scene index (which names an instance by its prototype) cannot
/// show (design D7).
pub(in crate::scene::usd_import) fn instance_without_prototype(
    stage: &Stage,
    path: &sdf::Path,
) -> bool {
    let prim = super::super::prim_at(stage, path.clone());
    prim.is_instance().unwrap_or(false) && !matches!(prim.prototype(), Ok(Some(_)))
}
