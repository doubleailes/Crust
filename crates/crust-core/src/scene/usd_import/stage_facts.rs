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

use crate::scene::hydra::index::host::{HostFacts, XformFacts};

/// The identifiers of every layer `stage` composed ([`crate::Scene::layers`]):
/// a file list, not scene description.
pub(in crate::scene) fn layer_identifiers(stage: &Stage) -> Vec<String> {
    stage.layer_identifiers().into_iter().collect()
}

/// The identifiers of `stage`'s root layer stack, strongest first
/// ([`crate::scene::Source::layer_stack`]): a file list, not scene description.
pub(in crate::scene) fn layer_stack(stage: &Stage) -> Vec<String> {
    stage.layer_stack()
}

/// The stage's `startTimeCode` and `endTimeCode`, from the absolute root's
/// `sceneGlobals` in `index`, when the stage authors both: whether a range is
/// authored is the host's fact (`UsdStage::HasAuthoredTimeCodeRange`), since
/// Hydra's scene globals give the values only, `0` when unauthored.
pub(in crate::scene) fn time_range(stage: &Stage, index: &dyn SceneIndex) -> Option<(f64, f64)> {
    use moho::tokens::scene_globals as globals;
    if !stage.has_authored_time_code_range() {
        return None;
    }
    let root = index.prim(&sdf::Path::abs_root()).data?;
    let field = |name: &str| -> Option<f64> {
        match locate(&root, &Locator::new([globals::SCENE_GLOBALS, name]))?.value()? {
            sdf::Value::Double(v) => Some(v),
            _ => None,
        }
    };
    Some((
        field(globals::START_TIME_CODE)?,
        field(globals::END_TIME_CODE)?,
    ))
}

/// The stage's authored `metersPerUnit` (C++ `UsdGeomGetStageMetersPerUnit`
/// reads the root and session layers), `None` when no layer authors it. A
/// host fact: Hydra carries no stage unit (design D4).
pub(in crate::scene) fn meters_per_unit(stage: &Stage) -> Option<f64> {
    if !stage
        .has_authored_stage_metadata("metersPerUnit")
        .unwrap_or(false)
    {
        return None;
    }
    match stage.stage_metadata("metersPerUnit").ok().flatten()? {
        sdf::Value::Double(m) => Some(m),
        sdf::Value::Float(m) => Some(f64::from(m)),
        _ => None,
    }
}

/// The files behind layer `identifiers`, sorted and without repeats: a layer
/// inside a package is the package file, a relative identifier is anchored
/// to the root layer `root`'s directory, file format arguments are dropped,
/// and an anonymous layer, which has no file, is left out.
pub(in crate::scene) fn layer_files(
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
pub(in crate::scene) fn stack_files(
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
pub(in crate::scene) fn invisible_overrides(stage: &Stage) -> u32 {
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
pub(in crate::scene) fn not_a_pass(stage: &Stage, pass: &sdf::Path) -> String {
    let prim = super::prim_at(stage, pass.clone());
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
pub(in crate::scene) fn mtlx_reference(
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
    if let Ok(graph) = super::prim_at(stage, mat_path.clone()).prim_index().graph() {
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
pub(in crate::scene) fn foreign_spec_paths(
    stage: &Stage,
    paths: &[sdf::Path],
    root: &sdf::Path,
) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        let prim = super::prim_at(stage, path.clone());
        for site in prim.prim_stack().unwrap_or_default() {
            if !site.path.has_prefix(root) {
                out.push(site.path.to_string());
            }
        }
    }
    out
}

/// The [`XformFacts`] of the prim at `path` on `stage`, at time code `time`
/// (`None`: time code 0).
pub(in crate::scene) fn xform_facts(
    stage: &Stage,
    path: &sdf::Path,
    time: Option<f64>,
) -> XformFacts {
    use moho::openusd::tf::Token;
    let prim = super::prim_at(stage, path.clone());
    let registry = moho::openusd_schemas::schema_registry();
    let ty = prim
        .type_name()
        .ok()
        .flatten()
        .map(|t| t.as_str().to_owned())
        .unwrap_or_default();
    let xformable =
        !ty.is_empty() && registry.is_a(&Token::from(ty.as_str()), &Token::from("Xformable"));
    let ops: Vec<String> = match prim.attribute("xformOpOrder").get::<sdf::Value>() {
        Ok(Some(sdf::Value::TokenVec(v))) => v.iter().map(|t| t.as_str().to_owned()).collect(),
        _ => Vec::new(),
    };
    let unknown_type =
        (!ty.is_empty() && !registry.is_concrete_type(&Token::from(ty.as_str()))).then_some(ty);
    let error = if xformable && !ops.is_empty() {
        xform_error(stage, path, time)
    } else {
        None
    };
    XformFacts {
        xformable,
        ops,
        unknown_type,
        error,
    }
}

/// Why the prim at `path` has no local transform at `time`: the error
/// openusd's `XformQuery` gives for its op stack, which the scene index
/// (Hydra's xform schema) does not carry. For the `xform.uncomposable`
/// diagnostic only (design D7).
pub(in crate::scene) fn xform_error(
    stage: &Stage,
    path: &sdf::Path,
    time: Option<f64>,
) -> Option<String> {
    use moho::openusd_schemas::geom::XformQuery;
    let prim = super::prim_at(stage, path.clone());
    // Without a time code, the index's own: time code 0.
    let time = Some(moho::openusd::usd::TimeCode::new(time.unwrap_or(0.0)));
    match XformQuery::for_prim(&prim) {
        Err(e) => Some(e.to_string()),
        Ok(query) => query.local_transform(time).err().map(|e| e.to_string()),
    }
}

/// Whether the prim at `path` is instanceable with a composition arc but has
/// no prototype: for `instancing.instanceable_without_prototype` only, a
/// state the scene index (which names an instance by its prototype) cannot
/// show (design D7).
pub(in crate::scene) fn instance_without_prototype(stage: &Stage, path: &sdf::Path) -> bool {
    let prim = super::prim_at(stage, path.clone());
    prim.is_instance().unwrap_or(false) && !matches!(prim.prototype(), Ok(Some(_)))
}

/// The directory of the layer that authors the value of the attribute at
/// `attr` (following its connections to an interface input that supplies
/// it): where a relative asset path the value holds is anchored, as USD
/// anchors an asset path on its layer. Which layer authors an opinion is a
/// fact of the layer stack, not scene description (design D7). `None` when
/// the stage holds no such opinion.
pub(in crate::scene) fn authoring_layer_dir(
    stage: &Stage,
    attr: &sdf::Path,
) -> Option<std::path::PathBuf> {
    let mut at = attr.clone();
    // An interface chain is short; the bound keeps a cycle from looping.
    for _ in 0..16 {
        let (prim, name) = at.split_property()?;
        let attribute = super::prim_at(stage, prim).attribute(name);
        if attribute.has_authored_value().unwrap_or(false) {
            let site = attribute.property_stack().ok()?.into_iter().next()?;
            let dir = Path::new(&site.layer).parent()?.to_path_buf();
            return (!dir.as_os_str().is_empty()).then_some(dir);
        }
        at = attribute.connections().ok()?.into_iter().next()?;
    }
    None
}

/// Geometry and volume renderer attributes, by the name after `crust:`, that
/// are constant primvars now (design D5).
const GEOMETRY_ATTRIBUTES: &[&str] = &[
    "rayMask",
    "subdivisionLevel",
    "subdivisionEdgeLength",
    "displacementBound",
    "motion:translate",
];

/// Path settings, a relationship before and a `string` (`string[]` for the
/// filter lists) now (design D5).
const PATH_SETTINGS: &[(&str, &str)] = &[
    ("crust:dicingCamera", "string"),
    ("crust:sampleFilters", "string[]"),
    ("crust:displayFilters", "string[]"),
    ("crust:filter", "string"),
];

/// `crust check`'s walk of the composed stage, for what a Hydra delegate
/// cannot see (design D5, D11): `crust:*` authoring Crust no longer reads
/// (`attribute.legacy_crust`), a string path setting on a prim that comes
/// through a reference or payload (`settings.path_across_arc`), and an
/// attribute authoring both a default and time samples
/// (`time.default_and_samples`). Composition arcs, legacy properties and
/// which opinions a layer authors are facts of the stage, not scene
/// description. A render never runs it.
pub(in crate::scene) fn stage_checks(stage: &Stage) {
    let mut paths = Vec::new();
    if stage
        .traverse(moho::openusd::usd::PrimPredicate::DEFAULT, |p| {
            paths.push(p.clone())
        })
        .is_err()
    {
        return;
    }
    for path in paths {
        let prim = super::prim_at(stage, path.clone());
        let ty = prim
            .type_name()
            .ok()
            .flatten()
            .map(|t| t.as_str().to_owned())
            .unwrap_or_default();
        let render_prim = matches!(
            ty.as_str(),
            "RenderSettings" | "RenderProduct" | "RenderVar" | "RenderPass"
        );
        let light = prim.has_api_schema("LightAPI").unwrap_or(false)
            || prim.is_a("BoundableLightBase").unwrap_or(false)
            || prim.is_a("NonboundableLightBase").unwrap_or(false);
        for attribute in prim.attributes().unwrap_or_default() {
            let name = attribute.name().to_owned();
            legacy_attribute(&path, &name, render_prim, light, &attribute);
            default_and_samples(stage, &attribute);
            if render_prim
                && PATH_SETTINGS.iter().any(|(n, _)| *n == name)
                && attribute.has_authored_value().unwrap_or(false)
            {
                path_across_arc(&prim, &path, &name);
            }
        }
        for relationship in prim.relationships().unwrap_or_default() {
            let name = relationship.name();
            if let Some((_, form)) = PATH_SETTINGS.iter().find(|(n, _)| *n == name) {
                crate::warning!(
                    AttributeLegacyCrust,
                    at = &path,
                    "{path}: rel {name} is not read — author `{form} {name}` holding the \
                     absolute prim path"
                );
            }
        }
    }
}

/// `attribute.legacy_crust` for attribute `name` of the prim at `path`.
fn legacy_attribute(
    path: &sdf::Path,
    name: &str,
    render_prim: bool,
    light: bool,
    attribute: &moho::openusd::usd::Attribute,
) {
    if !attribute.has_authored_value().unwrap_or(false) {
        return;
    }
    if light {
        if name == "primvars:crust:rayMask" {
            crate::warning!(
                AttributeLegacyCrust,
                at = path,
                "{path}: {name} on a light is not read — author `crust:rayMask` on the light"
            );
        }
        return;
    }
    if render_prim {
        return;
    }
    let Some(rest) = name.strip_prefix("crust:") else {
        return;
    };
    if GEOMETRY_ATTRIBUTES.contains(&rest) || rest.starts_with("volume:") {
        crate::warning!(
            AttributeLegacyCrust,
            at = path,
            "{path}: {name} is not read — author the constant primvar `primvars:{name}`"
        );
    }
}

/// `time.default_and_samples` for `attribute`: time samples, and a layer
/// that authors a default for it.
fn default_and_samples(stage: &Stage, attribute: &moho::openusd::usd::Attribute) {
    if attribute
        .time_sample_times()
        .map(|t| t.is_empty())
        .unwrap_or(true)
    {
        return;
    }
    let with_default = attribute
        .property_stack()
        .unwrap_or_default()
        .iter()
        .any(|site| {
            stage
                .layer(&site.layer)
                .and_then(|layer| {
                    layer
                        .attribute(site.path.clone())
                        .ok()
                        .flatten()
                        .and_then(|spec| spec.default())
                })
                .is_some_and(|v| !matches!(v, sdf::Value::ValueBlock))
        });
    if with_default {
        let at = attribute.path();
        crate::warning!(
            TimeDefaultAndSamples,
            at = &at.prim_path(),
            "{at} authors both a default and time samples — without -f its sample at time \
             code 0 is read, not its default"
        );
    }
}

/// `settings.path_across_arc` for the path setting `name` of the prim at
/// `path`, when the prim comes through a reference or payload.
fn path_across_arc(prim: &moho::openusd::usd::Prim, path: &sdf::Path, name: &str) {
    use moho::openusd::pcp::ArcType;
    let Ok(graph) = prim.prim_index().graph() else {
        return;
    };
    let Some((kind, from)) = graph.all_nodes().find_map(|node| match node.arc() {
        ArcType::Reference => Some(("reference", node.path().clone())),
        ArcType::Payload => Some(("payload", node.path().clone())),
        _ => None,
    }) else {
        return;
    };
    crate::warning!(
        SettingsPathAcrossArc,
        at = path,
        "{path}: {name} holds a prim path, and {path} comes through a {kind} (of {from}), \
         which does not remap it as it would a relationship target"
    );
}

/// The batch host's [`HostFacts`]: read off the stage it opened, at the time
/// it set on its stage scene index (whose `standard` chain it also reads the
/// stage's time range from).
pub struct StageFacts {
    stage: Stage,
    time: Option<f64>,
    standard: Option<moho::scene_index::SceneIndexHandle>,
    /// The stage's file, which relative layer identifiers resolve against.
    path: std::path::PathBuf,
}

impl StageFacts {
    /// The facts of `stage`, opened from the file at `path`, as its host
    /// reads it at `time` through `standard` (Moho's standard chain over its
    /// stage scene index).
    pub fn new(
        stage: &Stage,
        path: &Path,
        time: Option<f64>,
        standard: Option<moho::scene_index::SceneIndexHandle>,
    ) -> Self {
        StageFacts {
            stage: stage.clone(),
            time,
            standard,
            path: path.to_path_buf(),
        }
    }

    /// The facts of `stage` alone, at the default time: for a reader that
    /// asks no time-dependent fact.
    #[cfg(test)]
    pub(in crate::scene) fn of(stage: &Stage) -> Self {
        StageFacts::new(stage, Path::new(""), None, None)
    }

    /// The host moved its stage scene index's time to `time`.
    pub fn set_time(&mut self, time: Option<f64>) {
        self.time = time;
    }
}

impl HostFacts for StageFacts {
    fn time(&self) -> Option<f64> {
        self.time
    }
    fn time_range(&self) -> Option<(f64, f64)> {
        time_range(&self.stage, self.standard.as_ref()?.as_ref())
    }
    fn layers(&self) -> Vec<std::path::PathBuf> {
        layer_files(
            &layer_identifiers(&self.stage).into_iter().collect(),
            &self.path,
        )
    }
    fn layer_stack(&self) -> Vec<std::path::PathBuf> {
        stack_files(&layer_stack(&self.stage), &self.path)
    }
    fn invisible_overrides(&self) -> Option<u32> {
        Some(invisible_overrides(&self.stage))
    }
    fn meters_per_unit(&self) -> Option<f64> {
        meters_per_unit(&self.stage)
    }
    fn not_a_pass(&self, pass: &sdf::Path) -> String {
        not_a_pass(&self.stage, pass)
    }
    fn mtlx_reference(&self, material: &sdf::Path) -> Option<(std::path::PathBuf, String)> {
        mtlx_reference(&self.stage, material)
    }
    fn foreign_spec_paths(&self, paths: &[sdf::Path], root: &sdf::Path) -> Vec<String> {
        foreign_spec_paths(&self.stage, paths, root)
    }
    fn xform_facts(&self, path: &sdf::Path, time: Option<f64>) -> XformFacts {
        xform_facts(&self.stage, path, time)
    }
    fn instance_without_prototype(&self, path: &sdf::Path) -> bool {
        instance_without_prototype(&self.stage, path)
    }
    fn authoring_layer_dir(&self, attr: &sdf::Path) -> Option<std::path::PathBuf> {
        authoring_layer_dir(&self.stage, attr)
    }
}
