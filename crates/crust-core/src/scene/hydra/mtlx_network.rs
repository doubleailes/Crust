//! `UsdShade` MaterialX networks → [`crust_mtlx::Doc`]s.
//!
//! A `.mtlx` referenced from a `Material` is read by crust-mtlx directly
//! (`materials::load_mtlx_material`). This is the other way MaterialX reaches
//! a stage: authored *inline*, as `Shader` prims whose `info:id` is a
//! MaterialX nodedef (`ND_open_pbr_surface_surfaceshader`,
//! `ND_anisotropic_vdf`, `ND_volume`) wired to the material's `surface` and
//! `volume` terminals — which is how UsdMtlx composes a document into a
//! stage, how Houdini and Maya export one, and how NVIDIA's Typhoon (the
//! OpenUSD reference renderer's hdEmbree) receives every material it renders.
//!
//! The network is read as the scene index gives it — the material's
//! `material` network ([`super::index::network`]), Hydra's material network
//! schema, its connections through `NodeGraph` outputs and interface inputs
//! already followed to a shader output or the value they forward — and
//! translated node for node into the document crust-mtlx's parser would have
//! produced from the equivalent XML: the category and type from the nodedef
//! name, each parameter's value formatted as the text a `value` attribute
//! holds and read back through the same [`crust_mtlx::parse_literal`], so a
//! network shades exactly as the same network in a `.mtlx` does.
//!
//! A shader id the import's MaterialX node library defines (a studio node
//! whose nodedef name breaks the `ND_` convention) is read through its
//! nodedef — category and type — before the name is parsed, and the
//! translated document is expanded against that library
//! ([`crust_mtlx::Doc::expand`]), as a `.mtlx` is.

use moho::openusd;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crust_mtlx::{Doc, Input, Library, Node, Source, Val};
use openusd::sdf;

use super::assets::asset_value_path;
use super::index::network::{self as shading, Network as Shading, Parameter};

/// A translated network: the document, and the node each terminal names.
pub(in crate::scene) struct Network {
    pub(in crate::scene) doc: Doc,
    pub(in crate::scene) surface: Option<String>,
    pub(in crate::scene) volume: Option<String>,
    /// What could not be translated, for one warning per material.
    pub(in crate::scene) reported: Vec<String>,
}

/// Whether a shader id names a MaterialX nodedef: a standard one (`ND_…`),
/// or one `library` defines.
pub(in crate::scene) fn is_mtlx_id(id: &str, library: &Library) -> bool {
    id.starts_with("ND_") || library.nodedef(id).is_some()
}

/// Translates the networks under the nodes `surface` and `volume` (either
/// may be absent; each a network and a node name in it, the networks of the
/// render contexts the terminals resolved in) into one document.
pub(in crate::scene) fn translate(
    stage_path: &Path,
    library: &Library,
    surface: Option<(&Shading, &str)>,
    volume: Option<(&Shading, &str)>,
) -> Network {
    let mut b = Builder {
        stage_path,
        library,
        nodes: Vec::new(),
        names: HashMap::new(),
        active: HashSet::new(),
        reported: Vec::new(),
    };
    let surface = surface.and_then(|(net, name)| b.node(net, name));
    let volume = volume.and_then(|(net, name)| b.node(net, name));
    let mut doc = Doc::from_nodes(b.nodes);
    doc.expand(library);
    Network {
        doc,
        surface,
        volume,
        reported: b.reported,
    }
}

struct Builder<'s> {
    stage_path: &'s Path,
    library: &'s Library,
    nodes: Vec<Node>,
    /// Shader prim path → its node's name (the path itself), once translated.
    names: HashMap<String, String>,
    /// Shader prims being translated, for cycle detection.
    active: HashSet<String>,
    reported: Vec<String>,
}

/// A translated value graph: its document, and each of its outputs as
/// `(name, node, node output)`.
pub(in crate::scene) struct ValueNetwork {
    pub(in crate::scene) doc: Doc,
    pub(in crate::scene) outputs: Vec<(String, String, Option<String>)>,
    /// What could not be translated, for one warning per graph.
    pub(in crate::scene) reported: Vec<String>,
}

/// Translates a sample filter's graph, a `Material`'s network: the shaders
/// each of `outputs` (its name, the network its terminal is in, and the
/// terminal's connection) reads, node for node, as [`translate`] does,
/// except that a node input connected to one of the material's interface
/// inputs becomes a [`Source::Interface`] of that name — a channel the host
/// supplies per evaluation — carrying the interface input's value as its
/// default. An interface input with no value carries no type in the
/// network, so it is refused.
pub(in crate::scene) fn translate_value_graph(
    stage_path: &Path,
    graph: &sdf::Path,
    outputs: &[(String, &Shading, shading::Connection)],
) -> ValueNetwork {
    // A sample filter reads standard nodes only: no custom nodedef is
    // resolved, so the document needs no expansion.
    let library = Library::default();
    let mut b = Builder {
        stage_path,
        library: &library,
        nodes: Vec::new(),
        names: HashMap::new(),
        active: HashSet::new(),
        reported: Vec::new(),
    };
    let mut out = Vec::new();
    for (name, net, c) in outputs {
        let net = with_channels(net, &mut b.reported);
        match b.node(&net, &c.node) {
            Some(node) => out.push((
                name.clone(),
                node,
                (c.output != "out").then(|| c.output.clone()),
            )),
            None => b.reported.push(format!(
                "{graph}: output {name:?} reads no MaterialX shader"
            )),
        }
    }
    ValueNetwork {
        doc: Doc::from_nodes(b.nodes),
        outputs: out,
        reported: b.reported,
    }
}

/// `net` with each node input its interface feeds marked as that channel.
fn with_channels(net: &Shading, reported: &mut Vec<String>) -> Shading {
    let mut out = net.clone();
    for (channel, consumers) in &net.interface {
        for (node, input) in consumers {
            let at = net
                .path(node)
                .map(|p| p.to_string())
                .unwrap_or_else(|| node.clone());
            let Some(n) = out.nodes.get_mut(node) else {
                continue;
            };
            if n.connections.contains_key(input) {
                continue;
            }
            match n.parameters.get_mut(input) {
                Some(p) => p.interface = Some(channel.clone()),
                None => reported.push(format!(
                    "{at}.inputs:{input}: the interface input {channel:?} it reads authors no \
                     value, so it has no type — author its default"
                )),
            }
        }
    }
    out
}

impl Builder<'_> {
    /// The node for the shader node `name` of `net`, translating it (and
    /// everything upstream of it) on first use. `None` for a node that is not
    /// a MaterialX shader, or that the network does not hold (a prim that is
    /// no `Shader`).
    fn node(&mut self, net: &Shading, name: &str) -> Option<String> {
        let path = net.path(name)?;
        let key = path.as_str().to_string();
        if let Some(name) = self.names.get(&key) {
            return Some(name.clone());
        }
        let node = net.nodes.get(name)?;
        if !self.active.insert(key.clone()) {
            self.reported.push(format!("{key}: connection cycle — cut"));
            return None;
        }
        if !is_mtlx_id(&node.id, self.library) {
            self.reported.push(format!(
                "{key}: not a MaterialX shader ({}) — its input falls back to the default",
                if node.id.is_empty() {
                    "no info:id"
                } else {
                    &node.id
                }
            ));
            self.active.remove(&key);
            return None;
        }
        // A library nodedef is named exactly, version included, so the
        // expansion instantiates that one and not its category's default.
        let (category, type_name, version) = match self.library.nodedef(&node.id) {
            Some(def) => (
                def.node.clone(),
                def.instance_type().to_string(),
                def.version.clone(),
            ),
            None => {
                // The network carries no output's type (Hydra has no Sdr
                // here), so the nodedef name alone says it.
                let (category, nd_type) = nodedef_category(&node.id);
                let type_name = node_type(&category, nd_type, None);
                (category, type_name, None)
            }
        };

        let names: BTreeSet<&String> = node
            .parameters
            .keys()
            .chain(node.connections.keys())
            .collect();
        let mut inputs = Vec::new();
        for input in names {
            let at = format!("{key}.inputs:{input}");
            // As UsdImaging's graph walk: a connection wins over a value.
            let made = match node.connections.get(input).and_then(|c| c.first()) {
                Some(c) => self.node(net, &c.node).map(|upstream| {
                    let type_name = self
                        .nodes
                        .iter()
                        .find(|n| n.name == upstream)
                        .map(|n| n.type_name.clone())
                        .unwrap_or_else(|| "float".to_string());
                    Input {
                        name: input.clone(),
                        type_name,
                        source: Source::Node {
                            name: upstream,
                            output: (c.output != "out").then(|| c.output.clone()),
                        },
                        colorspace: None,
                        text: None,
                        interface: None,
                    }
                }),
                None => match node.parameters.get(input) {
                    Some(p) => self.parameter_input(input, &at, "", p),
                    None => None,
                },
            };
            if let Some(made) = made {
                inputs.push(made);
            }
        }
        self.active.remove(&key);
        self.names.insert(key.clone(), key.clone());
        self.nodes.push(Node {
            category,
            name: key.clone(),
            type_name,
            inputs,
            graph: None,
            version,
            colorspace: None,
        });
        Some(key)
    }

    /// The input `name` taking the parameter `p` (its USD type `usd_type`
    /// when the parameter carries none): a literal, or the value graph's
    /// interface channel.
    fn parameter_input(
        &mut self,
        name: &str,
        at: &str,
        usd_type: &str,
        p: &Parameter,
    ) -> Option<Input> {
        let ty = p.type_name.as_deref().unwrap_or(usd_type);
        let mtype = mtlx_type(ty);
        if let Some(channel) = &p.interface {
            let default = p
                .value
                .as_ref()
                .and_then(|v| self.literal_text(v, at, mtype))
                .and_then(|text| literal_value(&text, mtype));
            return Some(Input {
                name: name.to_string(),
                type_name: mtype.to_string(),
                source: Source::Interface {
                    name: channel.clone(),
                    default,
                },
                colorspace: p.color_space.clone(),
                text: None,
                interface: None,
            });
        }
        let text = self.literal_text(p.value.as_ref()?, at, mtype)?;
        let Some(value) = literal_value(&text, mtype) else {
            // Left unauthored, so it takes its nodedef default.
            self.reported
                .push(format!("{at}: {text:?} is not a {mtype} value"));
            return None;
        };
        Some(Input {
            name: name.to_string(),
            type_name: mtype.to_string(),
            source: Source::Value(value),
            colorspace: p.color_space.clone(),
            text: Some(text),
            interface: None,
        })
    }

    /// A value as the text a MaterialX `value` attribute would hold.
    /// `filename`s are written absolute, resolved as every USD asset path is.
    fn literal_text(&mut self, value: &sdf::Value, at: &str, mtype: &str) -> Option<String> {
        let join = |v: &[f64]| {
            v.iter()
                .map(|x| format!("{}", *x as f32))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Some(match value {
            sdf::Value::Bool(b) => b.to_string(),
            sdf::Value::Int(i) => i.to_string(),
            sdf::Value::Float(x) => format!("{x}"),
            sdf::Value::Double(x) => format!("{}", *x as f32),
            sdf::Value::Vec2f(v) => join(&[v.x as f64, v.y as f64]),
            sdf::Value::Vec2d(v) => join(&[v.x, v.y]),
            sdf::Value::Vec3f(v) => join(&[v.x as f64, v.y as f64, v.z as f64]),
            sdf::Value::Vec3d(v) => join(&[v.x, v.y, v.z]),
            sdf::Value::Vec4f(v) => join(&[v.x as f64, v.y as f64, v.z as f64, v.w as f64]),
            sdf::Value::Vec4d(v) => join(&[v.x, v.y, v.z, v.w]),
            // Row-major, the translation in the last row: USD's `Gf`
            // convention and MaterialX's `matrix44` value order alike.
            sdf::Value::Matrix4d(m) => join(&m.0),
            sdf::Value::Matrix3d(m) => join(&m.0),
            sdf::Value::String(s) => s.clone(),
            sdf::Value::Token(t) => t.as_str().to_string(),
            sdf::Value::AssetPath(_) if mtype == "filename" => {
                asset_value_path(value, self.stage_path, &|| sdf::Path::new(at).ok())?
                    .to_string_lossy()
                    .into_owned()
            }
            sdf::Value::AssetPath(p) => p.as_str().to_string(),
            other => {
                self.reported.push(format!(
                    "{at}: value of type {:?} not translated",
                    std::mem::discriminant(other)
                ));
                return None;
            }
        })
    }
}

/// The MaterialX type tokens a nodedef name spells its signature with.
const TYPES: [&str; 18] = [
    "float",
    "integer",
    "boolean",
    "string",
    "filename",
    "color3",
    "color4",
    "vector2",
    "vector3",
    "vector4",
    "matrix33",
    "matrix44",
    "surfaceshader",
    "volumeshader",
    "displacementshader",
    "lightshader",
    "material",
    "multioutput",
];

/// The closure types, which a nodedef name spells as a signature only on the
/// combinators (`ND_mix_vdf`, `ND_multiply_bsdfC`); on a leaf
/// (`ND_dielectric_bsdf`, `ND_anisotropic_vdf`) it is part of the category.
const CLOSURES: [&str; 3] = ["bsdf", "edf", "vdf"];
const COMBINATORS: [&str; 4] = ["mix", "add", "multiply", "layer"];

/// The signature token `tok` spells, without the variant letters MaterialX
/// appends (`vdfC`, `color3FA`, `floatB`), if it is one.
fn type_token(tok: &str) -> Option<&str> {
    let base = tok.trim_end_matches(|c: char| c.is_ascii_uppercase());
    (TYPES.contains(&base) || CLOSURES.contains(&base)).then_some(base)
}

/// Splits a nodedef name into the node's category and the first type its
/// signature names: `ND_open_pbr_surface_surfaceshader` →
/// (`open_pbr_surface`, `surfaceshader`), `ND_mix_vdf` → (`mix`, `vdf`),
/// `ND_anisotropic_vdf` → (`anisotropic_vdf`, none),
/// `ND_standard_surface_surfaceshader_100` → (`standard_surface`,
/// `surfaceshader`). MaterialX names every standard nodedef this way.
fn nodedef_category(id: &str) -> (String, Option<&str>) {
    let rest = id.strip_prefix("ND_").unwrap_or(id);
    let mut toks: Vec<&str> = rest.split('_').collect();
    // A versioned nodedef: `_100`.
    if toks.len() > 1
        && toks
            .last()
            .is_some_and(|t| t.chars().all(|c| c.is_ascii_digit()))
    {
        toks.pop();
    }
    let mut first = None;
    while toks.len() > 1 {
        let Some(t) = type_token(toks[toks.len() - 1]) else {
            break;
        };
        if CLOSURES.contains(&t)
            && !COMBINATORS.contains(&toks[..toks.len() - 1].join("_").as_str())
        {
            break;
        }
        toks.pop();
        first = Some(t);
    }
    (toks.join("_"), first)
}

/// The MaterialX type of a node: a closure or shader type when its category
/// or signature says so, else its `outputs:out`'s USD type, else the
/// signature's.
fn node_type(category: &str, signature: Option<&str>, out: Option<&str>) -> String {
    let closure = |t: &str| match t {
        "bsdf" => Some("BSDF"),
        "edf" => Some("EDF"),
        "vdf" => Some("VDF"),
        _ => None,
    };
    if let Some(t) = CLOSURES
        .iter()
        .find(|c| category.ends_with(&format!("_{c}")))
        .and_then(|c| closure(c))
    {
        return t.to_string();
    }
    match category {
        "surface" | "surface_unlit" => return "surfaceshader".into(),
        "volume" => return "volumeshader".into(),
        "surfacematerial" | "volumematerial" => return "material".into(),
        _ => {}
    }
    if let Some(sig) = signature {
        if let Some(c) = closure(sig) {
            return c.to_string();
        }
        if sig.ends_with("shader") || sig == "material" {
            return sig.to_string();
        }
    }
    match out.map(mtlx_type) {
        Some(t) if t != "string" => t.to_string(),
        _ => signature.unwrap_or("float").to_string(),
    }
}

/// The MaterialX spelling of a USD value type.
fn mtlx_type(usd: &str) -> &'static str {
    match usd {
        "float" | "double" | "half" => "float",
        "int" | "uint" | "int64" => "integer",
        "bool" => "boolean",
        "color3f" | "color3d" | "color3h" => "color3",
        "color4f" | "color4d" | "color4h" => "color4",
        "float2" | "double2" | "half2" | "texCoord2f" | "texCoord2d" => "vector2",
        "float3" | "double3" | "half3" | "vector3f" | "vector3d" | "normal3f" | "normal3d"
        | "point3f" | "point3d" => "vector3",
        "float4" | "double4" | "half4" => "vector4",
        "matrix3d" => "matrix33",
        "matrix4d" => "matrix44",
        "asset" => "filename",
        _ => "string",
    }
}

/// The numeric value of a literal of MaterialX type `mtype`. A `filename` or
/// `string` rides in the input's `text`, beside a zero no operator reading
/// those types looks at — the XML parser's convention; any other type whose
/// text is not a number has no value (`None`), rather than a zero that would
/// shade as if authored.
fn literal_value(text: &str, mtype: &str) -> Option<Val> {
    crust_mtlx::parse_literal(text, mtype)
        .or_else(|| matches!(mtype, "string" | "filename").then_some(Val::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodedef_names_split_into_category_and_signature() {
        let cases = [
            (
                "ND_open_pbr_surface_surfaceshader",
                "open_pbr_surface",
                Some("surfaceshader"),
            ),
            (
                "ND_standard_surface_surfaceshader_100",
                "standard_surface",
                Some("surfaceshader"),
            ),
            ("ND_anisotropic_vdf", "anisotropic_vdf", None),
            ("ND_dielectric_bsdf", "dielectric_bsdf", None),
            ("ND_mix_vdf", "mix", Some("vdf")),
            ("ND_multiply_vdfC", "multiply", Some("vdf")),
            ("ND_layer_vdf", "layer", Some("vdf")),
            ("ND_mix_volumeshader", "mix", Some("volumeshader")),
            ("ND_volume", "volume", None),
            ("ND_surface", "surface", None),
            ("ND_surface_unlit", "surface_unlit", None),
            ("ND_surfacematerial", "surfacematerial", None),
            ("ND_image_color3", "image", Some("color3")),
            ("ND_convert_float_color3", "convert", Some("float")),
            ("ND_multiply_color3FA", "multiply", Some("color3")),
            ("ND_artistic_ior", "artistic_ior", None),
            (
                "ND_UsdPreviewSurface_surfaceshader",
                "UsdPreviewSurface",
                Some("surfaceshader"),
            ),
        ];
        for (id, category, sig) in cases {
            let (c, s) = nodedef_category(id);
            assert_eq!((c.as_str(), s), (category, sig), "{id}");
        }
    }

    #[test]
    fn usd_types_map_to_materialx_spellings() {
        assert_eq!(mtlx_type("color3f"), "color3");
        assert_eq!(mtlx_type("normal3f"), "vector3");
        assert_eq!(mtlx_type("texCoord2f"), "vector2");
        assert_eq!(mtlx_type("asset"), "filename");
        assert_eq!(mtlx_type("token"), "string");
    }

    #[test]
    fn a_numeric_literal_that_is_not_a_number_has_no_value() {
        assert_eq!(literal_value("0.5", "float").map(|v| v.x()), Some(0.5));
        assert_eq!(literal_value("true", "boolean").map(|v| v.x()), Some(1.0));
        // Not a zero that shades as if authored: unset, so the default holds.
        assert!(literal_value("fast", "float").is_none());
        assert!(literal_value("", "color3").is_none());
        // Text-valued types keep the XML parser's zero beside their text.
        assert_eq!(literal_value("linear", "string").map(|v| v.x()), Some(0.0));
        assert_eq!(literal_value("a.png", "filename").map(|v| v.x()), Some(0.0));
    }

    /// A shader whose `info:id` names a library nodedef expands that nodedef,
    /// version included — not its category's default version.
    #[test]
    fn a_shader_naming_a_versioned_nodedef_expands_that_version() {
        let mut library = Library::default();
        library
            .merge_text(
                r#"<materialx>
                     <nodedef name="ND_k_v1" node="k" version="1" isdefaultversion="true"><output name="out" type="float" /></nodedef>
                     <nodegraph name="NG_k_v1" nodedef="ND_k_v1">
                       <constant name="c" type="float"><input name="value" type="float" value="1" /></constant>
                       <output name="out" type="float" nodename="c" />
                     </nodegraph>
                     <nodedef name="ND_k_v2" node="k" version="2"><output name="out" type="float" /></nodedef>
                     <nodegraph name="NG_k_v2" nodedef="ND_k_v2">
                       <constant name="c" type="float"><input name="value" type="float" value="2" /></constant>
                       <output name="out" type="float" nodename="c" />
                     </nodegraph>
                   </materialx>"#,
            )
            .expect("library parses");
        let dir = std::env::temp_dir().join("crust_mtlx_network_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("versioned.usda");
        std::fs::write(
            &path,
            "#usda 1.0\ndef Shader \"S\"\n{\n    uniform token info:id = \"ND_k_v2\"\n    float outputs:out\n}\n",
        )
        .expect("write stage");
        let network = super::shading::Network {
            material: openusd::sdf::path("/").unwrap(),
            nodes: [(
                "/S".to_string(),
                super::shading::Node {
                    id: "ND_k_v2".into(),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        };
        let net = translate(&path, &library, Some((&network, "/S")), None);
        assert!(net.doc.reported.is_empty(), "{:?}", net.doc.reported);
        let c = net
            .doc
            .find("", "/S/c")
            .expect("the version 2 graph's node");
        let Some(Source::Value(v)) = c.input("value").map(|i| &i.source) else {
            panic!("{:?}", c.inputs)
        };
        assert_eq!(v.x(), 2.0);
    }
}
