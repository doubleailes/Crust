//! `UsdPreviewSurface` (+ `UsdUVTexture`) → [`OpenPBR`] / [`crate::PreviewSurface`],
//! read from the material's network in the scene index ([`super::index::network`]).

use crate::warning;
use std::sync::Arc;

use glam::Vec3A;
use moho::openusd::sdf;
use tracing::debug;

use crate::color::Space;
use crate::material::{Displacement, DisplacementValue, Material, OpenPBR};

use super::ImportCaches;
use super::assets::{asset_value_path, load_uv_texture};
use super::attrs::decode_float4;
use super::index::data::{PrimData, color_space};
use super::index::network::{self, Network};
use super::materials::{MatSource, material_ptex, named_space};

/// `UsdUVTexture`'s identifier and its inputs.
mod tk {
    pub(super) const USD_UV_TEXTURE: &str = "UsdUVTexture";
    pub(super) const USD_PRIMVAR_READER_FLOAT2: &str = "UsdPrimvarReader_float2";
    pub(super) const FILE: &str = "file";
    pub(super) const ST: &str = "st";
    pub(super) const VARNAME: &str = "varname";
    pub(super) const FALLBACK: &str = "fallback";
    pub(super) const SCALE: &str = "scale";
    pub(super) const BIAS: &str = "bias";
    pub(super) const WRAP_S: &str = "wrapS";
    pub(super) const WRAP_T: &str = "wrapT";
}

/// One `UsdPreviewSurface` input, as openusd-schemas' `read_preview_surface`
/// reads it: the `UsdUVTexture` feeding it (its first upstream node, when it
/// is one with a `file`), else the value the shader authors on the input
/// itself (an interface connection is not followed), else unset.
enum Channel<T> {
    Unset,
    Value(T),
    Texture,
}

impl<T> Channel<T> {
    fn value(&self) -> Option<&T> {
        match self {
            Channel::Value(v) => Some(v),
            _ => None,
        }
    }

    fn textured(&self) -> bool {
        matches!(self, Channel::Texture)
    }

    fn is_set(&self) -> bool {
        !matches!(self, Channel::Unset)
    }
}

/// A `UsdPreviewSurface` read off the network: its node and every channel.
struct PreviewRead {
    diffuse_color: Channel<Vec3A>,
    emissive_color: Channel<Vec3A>,
    specular_color: Channel<Vec3A>,
    metallic: Channel<f32>,
    roughness: Channel<f32>,
    clearcoat: Channel<f32>,
    clearcoat_roughness: Channel<f32>,
    opacity: Channel<f32>,
    opacity_threshold: Channel<f32>,
    ior: Channel<f32>,
    normal: Channel<Vec3A>,
    occlusion: Channel<f32>,
}

/// Whether input `name` of the node `node` is fed by a `UsdUVTexture` with a
/// `file` value (`connected_texture_file`).
fn connected_texture(network: &Network, node: &str, name: &str) -> bool {
    let Some(first) = network
        .nodes
        .get(node)
        .and_then(|n| n.connections.get(name))
        .and_then(|c| c.first())
    else {
        return false;
    };
    network.nodes.get(&first.node).is_some_and(|tex| {
        tex.id == tk::USD_UV_TEXTURE
            && tex
                .parameters
                .get(tk::FILE)
                .and_then(|p| p.value.as_ref())
                .and_then(sdf::Value::as_str)
                .is_some()
    })
}

fn color_channel(network: &Network, node: &str, prim: &PrimData, name: &str) -> Channel<Vec3A> {
    if connected_texture(network, node, name) {
        return Channel::Texture;
    }
    match prim.resolved(&format!("inputs:{name}")) {
        Some(sdf::Value::Vec3f(v)) => Channel::Value(Vec3A::new(v.x, v.y, v.z)),
        Some(sdf::Value::Vec3d(v)) => {
            Channel::Value(Vec3A::new(v.x as f32, v.y as f32, v.z as f32))
        }
        Some(sdf::Value::Vec3h(v)) => {
            Channel::Value(Vec3A::new(v.x.to_f32(), v.y.to_f32(), v.z.to_f32()))
        }
        _ => Channel::Unset,
    }
}

fn scalar_channel(network: &Network, node: &str, prim: &PrimData, name: &str) -> Channel<f32> {
    if connected_texture(network, node, name) {
        return Channel::Texture;
    }
    match prim.resolved(&format!("inputs:{name}")) {
        Some(sdf::Value::Float(f)) => Channel::Value(f),
        Some(sdf::Value::Double(d)) => Channel::Value(d as f32),
        Some(sdf::Value::Half(h)) => Channel::Value(h.to_f32()),
        Some(sdf::Value::Int(i)) => Channel::Value(i as f32),
        _ => Channel::Unset,
    }
}

/// The material's `UsdPreviewSurface`, as `read_preview_surface` finds it: the
/// universal surface terminal, else the authored contexts' in sorted order;
/// `None` unless its first source is a `UsdPreviewSurface` node.
fn read_preview(src: &MatSource) -> Option<PreviewRead> {
    let resolved = network::resolve(&src.networks, "surface", &[]).or_else(|| {
        let mut contexts: Vec<&str> = src
            .networks
            .iter()
            .filter(|(c, n)| !c.is_empty() && n.terminals.contains_key("surface"))
            .map(|(c, _)| c.as_str())
            .collect();
        contexts.sort();
        (!contexts.is_empty())
            .then(|| network::resolve(&src.networks, "surface", &contexts))
            .flatten()
    })?;
    let (network, Some((node, shader))) = resolved else {
        return None;
    };
    if shader.id != "UsdPreviewSurface" {
        return None;
    }
    let prim = src.node(network, node)?;
    let c = |name: &str| color_channel(network, node, &prim, name);
    let f = |name: &str| scalar_channel(network, node, &prim, name);
    Some(PreviewRead {
        diffuse_color: c("diffuseColor"),
        emissive_color: c("emissiveColor"),
        specular_color: c("specularColor"),
        metallic: f("metallic"),
        roughness: f("roughness"),
        clearcoat: f("clearcoat"),
        clearcoat_roughness: f("clearcoatRoughness"),
        opacity: f("opacity"),
        opacity_threshold: f("opacityThreshold"),
        ior: f("ior"),
        normal: c("normal"),
        occlusion: f("occlusion"),
    })
}

/// A `UsdPreviewSurface` material: its constants as an [`OpenPBR`], wrapped in
/// a [`crate::PreviewSurface`] when any input is driven by a `UsdUVTexture`.
///
/// An untextured surface — and one whose every texture connection turned out
/// unusable — stays the plain `OpenPBR` it always was, so it neither builds a
/// UV table nor pays a per-hit evaluation.
///
/// `shader` is the surface node the material resolves to (in `network`), whose
/// inputs are followed to textures and whose colours name their space.
pub(super) fn preview_surface_material(
    src: &MatSource,
    network: &Network,
    shader: &str,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {
    use crate::material::preview_surface::Target;

    let mat_path = &src.path;
    let shader_prim = src.node(network, shader);
    let ps = read_preview(src);
    let mut base = match (&ps, &shader_prim) {
        (Some(ps), Some(prim)) => preview_surface_openpbr(ps, prim, caches.working),
        _ => OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)),
    };
    base.base_color_ptex = material_ptex(src, caches);
    base.luma = caches.luma;
    let Some(ps) = ps else {
        return Arc::new(base);
    };

    // `read_preview_surface` reports `Texture` exactly when an input resolves
    // to a `UsdUVTexture` output, which is the question; the walk below then
    // reads the whole node rather than just its file.
    let textured = [
        (Target::DiffuseColor, ps.diffuse_color.textured()),
        (Target::EmissiveColor, ps.emissive_color.textured()),
        (Target::Metallic, ps.metallic.textured()),
        (Target::Roughness, ps.roughness.textured()),
        // Translucency only: a textured cutout mask is read apart, below.
        (
            Target::Opacity,
            ps.opacity.textured() && opacity_transmission(&ps),
        ),
        (Target::Ior, ps.ior.textured()),
        (Target::Clearcoat, ps.clearcoat.textured()),
        (
            Target::ClearcoatRoughness,
            ps.clearcoat_roughness.textured(),
        ),
    ];
    let mut inputs = Vec::new();
    let mut varnames: Vec<String> = Vec::new();
    let mut note = |v: Option<String>| {
        if let Some(v) = v
            && !varnames.contains(&v)
        {
            varnames.push(v);
        }
    };
    for (target, is_textured) in textured {
        if is_textured
            && let Some((input, varname)) = preview_uv_input(
                src,
                network,
                shader,
                target.input_name(),
                if target.is_colour() {
                    TexRole::Colour
                } else {
                    TexRole::Value
                },
                caches,
            )
        {
            note(varname);
            inputs.push((target, input));
        }
    }
    let normal = if ps.normal.textured() {
        preview_uv_input(src, network, shader, "normal", TexRole::Value, caches).map(
            |(input, varname)| {
                note(varname);
                input
            },
        )
    } else {
        None
    };
    // `opacityThreshold > 0` with a textured opacity: the mask is sampled
    // per hit. (A constant one is already thresholded into
    // `geometry_opacity`.)
    let cutout = if !opacity_transmission(&ps) && ps.opacity.textured() {
        preview_uv_input(src, network, shader, "opacity", TexRole::Value, caches).map(
            |(input, varname)| {
                note(varname);
                let threshold = ps.opacity_threshold.value().copied().unwrap_or(0.0);
                (input, threshold)
            },
        )
    } else {
        None
    };

    // One chart per mesh: a network whose readers name two primvars shades
    // every texture from the first.
    if varnames.len() > 1 {
        warning!(
            PreviewMultiplePrimvars,
            at = mat_path,
            "UsdPreviewSurface at {mat_path}: textures read primvars {varnames:?}; crust \
             carries one chart per mesh and reads '{}' for all of them",
            varnames[0]
        );
    }
    for (name, set) in [
        ("occlusion", ps.occlusion.is_set()),
        ("specularColor", ps.specular_color.is_set()),
    ] {
        if set {
            debug!("UsdPreviewSurface at {mat_path}: {name} is not read");
        }
    }
    if inputs.is_empty() && normal.is_none() && cutout.is_none() {
        return Arc::new(base);
    }
    let mut m = crate::PreviewSurface::new(mat_path.to_string(), base, inputs, normal)
        .with_uv_primvar(varnames.into_iter().next());
    if let Some((input, threshold)) = cutout {
        m = m.with_cutout(input, threshold);
    }
    debug!("Material {mat_path}: {m:?}");
    Arc::new(m)
}

/// What a UsdUVTexture feeding a surface input holds, which decides how it
/// is decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TexRole {
    /// A colour (`diffuseColor`, `emissiveColor`): its file's own
    /// `colorSpace` metadatum, else `sourceColorSpace`, converted into the
    /// working space, primaries included.
    Colour,
    /// A value that is not a colour — normal, roughness, metallic, opacity,
    /// IOR, clearcoat: `sourceColorSpace`'s curve alone, `auto` decided by the
    /// file-format rule as UsdUVTexture specifies.
    Value,
    /// A displacement height: as [`TexRole::Value`], but `auto` is raw, so an
    /// 8-bit RGB height map is not decoded as sRGB. An authored `sRGB` still
    /// wins.
    Data,
}

/// Follows one `UsdPreviewSurface` input to the `UsdUVTexture` producing it
/// and reads that node whole: file, colour space, wrap modes, scale, bias,
/// fallback, and the primvar reader behind its `st`.
///
/// Every value is read through `value_producing_attributes(Any)`, so an input
/// wired to the Material's interface — how a published look exposes its
/// file paths — resolves the same as one authored on the node. `None` when the
/// input does not resolve to a texture output crust can use; the surface then
/// keeps that input's constant.
///
/// `role` says what the texture holds, which decides its decode (see
/// [`TexRole`]).
fn preview_uv_input(
    src: &MatSource,
    network: &Network,
    shader: &str,
    name: &str,
    role: TexRole,
    caches: &mut ImportCaches<'_>,
) -> Option<(crate::material::preview_surface::UvInput, Option<String>)> {
    use crate::material::preview_surface::{TexOutput, UvInput, Wrap};

    let mat_path = &src.path;
    let source = network.nodes.get(shader)?.connections.get(name)?.first()?;
    let Some(output) = TexOutput::from_name(&source.output) else {
        let at = network
            .path(&source.node)
            .map_or_else(|| source.node.clone(), |p| p.to_string());
        warning!(
            PreviewUnsupportedConnection,
            at = mat_path,
            "UsdPreviewSurface at {mat_path}: {name} connects to {at}.outputs:{}, not a \
             UsdUVTexture r/g/b/a/rgb output — using its constant",
            source.output
        );
        return None;
    };
    let tex = network.nodes.get(&source.node)?;
    if tex.id != tk::USD_UV_TEXTURE {
        return None;
    }
    let tex_path = network.path(&source.node)?;

    // The value an input carries, an interface connection followed: the
    // node's parameter.
    let value = |input: &str| tex.parameters.get(input).and_then(|p| p.value.clone());
    let token = |input: &str| value(input).and_then(|v| v.as_str().map(str::to_owned));
    let float4 = |input: &str| value(input).and_then(decode_float4);

    let file = value(tk::FILE).and_then(|v| {
        asset_value_path(&v, caches.stage_path, &|| {
            src.layer_dir(&tex_path, "inputs:file")
        })
    });
    let Some(file) = file else {
        warning!(
            PreviewTextureWithoutFile,
            at = tex_path,
            "UsdUVTexture {tex_path}: no inputs:file — {name} keeps its constant"
        );
        return None;
    };
    // Hydra consolidates the file's colour space onto `inputs:file`
    // (`UsdImagingDataSourceAttributeColorSpace`): `sourceColorSpace` when it
    // holds a non-empty token, else the attribute's computed `colorSpace`.
    // The UsdUVTexture keywords (`sRGB`, `raw`, `auto`) keep their meaning; any
    // other name is a colour space, which a colour's file is read in.
    //
    // A texture that is not a colour is never moved to other primaries,
    // whatever the file or the working space: a change of primaries mixes a
    // normal map's channels and shifts a roughness read from `.r`. It takes
    // the keyword alone, into Rec.709 linear — the primaries of every curve
    // a keyword can name, so the curve alone, as before colour management.
    let file_space = tex
        .parameters
        .get(tk::FILE)
        .and_then(|p| p.color_space.clone())
        .filter(|n| !n.is_empty());
    let keyword = file_space
        .as_deref()
        .filter(|n| ["srgb", "raw", "auto"].contains(&n.to_ascii_lowercase().as_str()));
    let source = crate::ColorSpace::from_usd(keyword, caches.working);
    let space = if role != TexRole::Colour {
        match source.resolved() {
            None if role == TexRole::Data => crate::ColorSpace::RAW,
            _ => source.into_working(Space::LIN_REC709),
        }
    } else {
        let named = file_space
            .as_deref()
            .filter(|_| keyword.is_none())
            .and_then(|n| named_space(&format!("{tex_path}.inputs:{}", tk::FILE), n));
        match named {
            Some(space) => crate::ColorSpace::new(space, caches.working),
            None => source,
        }
    };

    // Which chart the texture reads. crust carries one per mesh (see
    // `mesh_uvs`), so a reader naming another primvar is approximated by it.
    let mut varname = None;
    if let Some(reader) = tex.connections.get(tk::ST).and_then(|c| c.first()) {
        let node = network.nodes.get(&reader.node);
        let id = node.map(|n| n.id.clone()).filter(|id| !id.is_empty());
        match (node, id.as_deref()) {
            (Some(reader), Some(tk::USD_PRIMVAR_READER_FLOAT2)) => {
                varname = reader
                    .parameters
                    .get(tk::VARNAME)
                    .and_then(|p| p.value.as_ref())
                    .and_then(|v| v.as_str().map(str::to_owned));
            }
            _ => warning!(
                PreviewUnreadSt,
                at = tex_path,
                "UsdUVTexture {tex_path}: st is driven by {id:?}, which is not read — using the \
                 mesh chart unchanged"
            ),
        }
    }

    let file_name = file.to_string_lossy();
    let tiled = file_name.contains("<UDIM>") || file_name.contains("<UVTILE>");
    // The node's own fallback, else the surface input's authored constant (so
    // a declined texture renders on the surface's constants), else that
    // input's `UsdPreviewSurface` schema default. The node set's own default,
    // opaque black, is the last resort only for an input the schema does not
    // know: a published look routinely connects an input without authoring a
    // constant, and black there is not neutral — ALab's wrench references a
    // roughness map the dataset does not ship, and roughness 0 turned it into
    // a mirror where the schema's 0.5 is an ordinary surface.
    let fallback = float4(tk::FALLBACK)
        .or_else(|| {
            src.node(network, shader)?
                .value(&format!("inputs:{name}"))
                .and_then(decode_float4)
        })
        .or_else(|| preview_surface_default(name))
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let loaded = load_uv_texture(&file, space, mat_path, caches).map(crate::TextureRef);
    if loaded.is_none() {
        // DEBUG: the host has already reported *why* at its own level (a
        // missing file is an ERROR there), and `CRUST_TEX=0` declines every
        // texture on purpose — a WARN here would repeat per texture.
        debug!(
            "UsdPreviewSurface at {mat_path}: {name} texture {} not loaded — shading with \
             {fallback:?}",
            file.display()
        );
    }
    let input = UvInput {
        tex: loaded,
        output,
        scale: float4(tk::SCALE).unwrap_or([1.0; 4]),
        bias: float4(tk::BIAS).unwrap_or([0.0; 4]),
        fallback,
        wrap: [
            Wrap::from_token(token(tk::WRAP_S).as_deref()),
            Wrap::from_token(token(tk::WRAP_T).as_deref()),
        ],
        tiled,
    };
    Some((input, varname))
}

/// `UsdPreviewSurface.inputs:displacement`, as a [`Displacement`].
///
/// Read by name: `ReadPreviewSurface` carries no displacement field. Either a
/// value — a constant offset, `None` at the schema default of 0 — or a
/// `UsdUVTexture`, read through [`preview_uv_input`] exactly as a shading input
/// is (channel, `scale`, `bias`, wrap), its file decoded raw unless
/// `sourceColorSpace` says otherwise. A texture the host cannot load falls
/// back to its fallback, and a connection that is not a usable texture (no
/// `inputs:file`, not a `UsdUVTexture`) to the input's own authored value, as
/// a shading input does. The texture's primvar reader names the chart the
/// mesh reads when the surface itself reads none.
pub(super) fn preview_displacement(
    src: &MatSource,
    network: &Network,
    shader: &str,
    caches: &mut ImportCaches<'_>,
) -> Option<Displacement> {
    let node = network.nodes.get(shader)?;
    let textured = node
        .connections
        .get("displacement")
        .is_some_and(|c| !c.is_empty());
    let constant = |c: f32| {
        (c != 0.0 && c.is_finite()).then(|| Displacement::new(DisplacementValue::Constant(c)))
    };
    if textured {
        let Some((uv, varname)) =
            preview_uv_input(src, network, shader, "displacement", TexRole::Data, caches)
        else {
            // Already warned about; the input keeps its own constant.
            let own = src
                .node(network, shader)?
                .value("inputs:displacement")
                .and_then(decode_float4)?;
            return constant(own[0]);
        };
        if uv.tex.is_none() {
            return constant(uv.scalar(uv.fallback));
        }
        return Some(Displacement::new(DisplacementValue::Uv(uv)).with_uv_primvar(varname));
    }
    let value = node
        .parameters
        .get("displacement")
        .and_then(|p| p.value.clone())
        .and_then(decode_float4)?;
    let c = value[0];
    (c != 0.0 && c.is_finite()).then(|| Displacement::new(DisplacementValue::Constant(c)))
}

/// A `UsdPreviewSurface` input's schema default, widened to four channels the
/// way [`decode_float4`] widens an authored value — what the input reads when a
/// texture drives it, the texture fails, and nothing else was authored.
/// Values from the UsdPreviewSurface specification.
fn preview_surface_default(input: &str) -> Option<[f32; 4]> {
    let v = |x: f32| [x; 4];
    let c = |r: f32, g: f32, b: f32| [r, g, b, 1.0];
    Some(match input {
        "diffuseColor" => c(0.18, 0.18, 0.18),
        "emissiveColor" => c(0.0, 0.0, 0.0),
        "specularColor" => c(0.0, 0.0, 0.0),
        "normal" => c(0.0, 0.0, 1.0),
        "metallic" => v(0.0),
        "roughness" => v(0.5),
        "clearcoat" => v(0.0),
        "clearcoatRoughness" => v(0.01),
        "opacity" => v(1.0),
        "ior" => v(1.5),
        "occlusion" => v(1.0),
        "displacement" => v(0.0),
        _ => return None,
    })
}

/// A `UsdPreviewSurface`'s constant inputs as an [`OpenPBR`]. A
/// texture-connected input leaves its field at the default here;
/// [`preview_surface_material`] is what drives it.
///
/// The two colours are in the working space unless their `colorSpace`
/// metadatum names another ([`in_working`]).
fn preview_surface_openpbr(ps: &PreviewRead, shader: &PrimData, working: Space) -> OpenPBR {
    let mut o = OpenPBR::default();
    // `attrs::in_working`, the space from the shader's own attribute.
    let color = |name: &str, rgb: Vec3A| match color_space(shader, name) {
        Some(space) => crate::color::convert(rgb, space, working),
        None => rgb,
    };

    if let Some(rgb) = ps.diffuse_color.value() {
        o.base_color = color("inputs:diffuseColor", *rgb);
    }
    if let Some(m) = ps.metallic.value() {
        o.base_metalness = *m;
    }
    if let Some(r) = ps.roughness.value() {
        o.specular_roughness = *r;
    }
    if let Some(op) = ps.opacity.value() {
        if opacity_transmission(ps) {
            o.transmission_weight = 1.0 - op.clamp(0.0, 1.0);
        } else {
            // The spec's cutout: kept whole at or above the threshold.
            let threshold = ps.opacity_threshold.value().copied().unwrap_or(0.0);
            o.geometry_opacity = if *op >= threshold { 1.0 } else { 0.0 };
        }
    }
    if let Some(rgb) = ps.emissive_color.value() {
        o.emission_color = color("inputs:emissiveColor", *rgb);
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        if max > 0.0 {
            o.emission_luminance = 1.0;
        }
    }
    if let Some(ior) = ps.ior.value() {
        o.specular_ior = *ior;
    }
    if let Some(c) = ps.clearcoat.value() {
        o.coat_weight = *c;
    }
    if let Some(cr) = ps.clearcoat_roughness.value() {
        o.coat_roughness = *cr;
    }

    o
}

/// Whether a `UsdPreviewSurface`'s `opacity` means translucency, which crust
/// maps to refraction (`transmission_weight = 1 − opacity` at `ior`), rather
/// than a cutout mask.
///
/// The spec gives `opacity` two modes. Under the default `opacityThreshold`
/// of 0 a surface below 1 is translucent, and its `ior` is "the index of
/// refraction to be used for translucent objects" — a dielectric, which is how
/// ALab authors all of its glass (opacity ≈ 0, ior ≈ 1.49). Above 0 it is a
/// mask that keeps or discards each point whole, which is the host cutout
/// `geometry_opacity` names and does not refract at all.
fn opacity_transmission(ps: &PreviewRead) -> bool {
    ps.opacity_threshold.value().is_none_or(|t| *t <= 0.0)
}
