//! A material's shading networks as the scene index gives them: Moho's
//! `material` data source, Hydra's material network schema — per render
//! context, the shader nodes reachable from the terminals, each with its
//! identifier, its parameters (the values its authored inputs carry, a
//! connection to an interface input followed) and its input connections to
//! upstream nodes' outputs.
//!
//! The readers that build Crust's materials (`preview`, `mtlx_network`,
//! `materials`) read these networks only: a shader is no prim of the index.

use std::collections::BTreeMap;

use moho::data_source::{ContainerHandle, DataSource};
use moho::openusd::sdf;
use moho::tokens::material as t;

use super::super::attrs::{decode_bool, decode_f32, decode_vec3};
use super::data::PrimData;

/// A node's input connection: the upstream node's name and output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::scene::usd_import) struct Connection {
    pub(in crate::scene::usd_import) node: String,
    pub(in crate::scene::usd_import) output: String,
}

/// A node parameter: the value an authored input carries (an interface
/// input's, when connected to one), its type name and its value attribute's
/// own `colorSpace`.
#[derive(Clone, Debug, Default)]
pub(in crate::scene::usd_import) struct Parameter {
    pub(in crate::scene::usd_import) value: Option<sdf::Value>,
    #[allow(dead_code)]
    pub(in crate::scene::usd_import) type_name: Option<String>,
    pub(in crate::scene::usd_import) color_space: Option<String>,
    /// For a sample filter's network only: the material interface input the
    /// parameter is connected to, a channel the host supplies per
    /// evaluation, `value` its default.
    pub(in crate::scene::usd_import) interface: Option<String>,
}

/// One shader node.
#[derive(Clone, Debug, Default)]
pub(in crate::scene::usd_import) struct Node {
    /// `info:id`, empty when the shader authors none.
    pub(in crate::scene::usd_import) id: String,
    pub(in crate::scene::usd_import) parameters: BTreeMap<String, Parameter>,
    pub(in crate::scene::usd_import) connections: BTreeMap<String, Vec<Connection>>,
}

impl Node {
    /// Parameter `name`'s value.
    pub(in crate::scene::usd_import) fn value(&self, name: &str) -> Option<sdf::Value> {
        self.parameters.get(name)?.value.clone()
    }

    pub(in crate::scene::usd_import) fn f32(&self, name: &str) -> Option<f32> {
        self.value(name).and_then(decode_f32)
    }

    pub(in crate::scene::usd_import) fn bool(&self, name: &str) -> Option<bool> {
        self.value(name).and_then(decode_bool)
    }

    pub(in crate::scene::usd_import) fn vec3(&self, name: &str) -> Option<glam::Vec3A> {
        self.value(name).and_then(decode_vec3)
    }

    /// Parameter `name`'s colour space name, as Hydra consolidates it (the
    /// value attribute's own `colorSpace`, else its prim's or an ancestor's
    /// `colorSpace:name`); `None` when empty.
    pub(in crate::scene::usd_import) fn color_space(&self, name: &str) -> Option<&str> {
        self.parameters
            .get(name)?
            .color_space
            .as_deref()
            .filter(|n| !n.is_empty())
    }

    /// The first connection of input `name`.
    pub(in crate::scene::usd_import) fn connection(&self, name: &str) -> Option<&Connection> {
        self.connections.get(name)?.first()
    }
}

/// One render context's network.
#[derive(Clone, Debug, Default)]
pub(in crate::scene::usd_import) struct Network {
    /// The material prim the network belongs to.
    pub(in crate::scene::usd_import) material: sdf::Path,
    /// By node name: the node's path relative to the material, or its stage
    /// path when it lies outside the material.
    pub(in crate::scene::usd_import) nodes: BTreeMap<String, Node>,
    /// By terminal (`surface`, `displacement`, `volume`).
    pub(in crate::scene::usd_import) terminals: BTreeMap<String, Connection>,
    /// The material's interface: each input the network reads, with the
    /// node inputs it feeds (`interface/parameters/<name>/mappings`), as
    /// `(node, input)`.
    pub(in crate::scene::usd_import) interface: BTreeMap<String, Vec<(String, String)>>,
}

fn container(source: Option<DataSource>) -> Option<ContainerHandle> {
    source?.as_container().cloned()
}

fn token(source: Option<DataSource>) -> Option<String> {
    match source?.value()? {
        sdf::Value::Token(t) => Some(t.as_str().to_owned()),
        sdf::Value::String(s) => Some(s),
        _ => None,
    }
}

fn connection(source: &ContainerHandle) -> Option<Connection> {
    Some(Connection {
        node: token(source.get(&t::UPSTREAM_NODE_PATH.into()))?,
        output: token(source.get(&t::UPSTREAM_NODE_OUTPUT_NAME.into()))?,
    })
}

fn node(source: &ContainerHandle) -> Node {
    let mut node = Node {
        id: token(source.get(&t::NODE_IDENTIFIER.into())).unwrap_or_default(),
        ..Node::default()
    };
    if let Some(parameters) = container(source.get(&t::PARAMETERS.into())) {
        for name in parameters.names() {
            let Some(p) = container(parameters.get(&name)) else {
                continue;
            };
            node.parameters.insert(
                name.as_str().to_owned(),
                Parameter {
                    value: p.get(&t::VALUE.into()).and_then(|d| d.value()),
                    type_name: token(p.get(&t::TYPE_NAME.into())),
                    color_space: token(p.get(&t::COLOR_SPACE.into())),
                    interface: None,
                },
            );
        }
    }
    if let Some(connections) = container(source.get(&t::INPUT_CONNECTIONS.into())) {
        for name in connections.names() {
            let Some(links) = connections.get(&name).and_then(|d| d.as_vector().cloned()) else {
                continue;
            };
            let links: Vec<Connection> = (0..links.len())
                .filter_map(|i| container(links.get(i)))
                .filter_map(|c| connection(&c))
                .collect();
            node.connections.insert(name.as_str().to_owned(), links);
        }
    }
    node
}

/// The networks of the material `material`, by render context (`""` the
/// universal one); empty for a prim that is not a material.
pub(in crate::scene::usd_import) fn networks(material: &PrimData) -> BTreeMap<String, Network> {
    let mut out = BTreeMap::new();
    let Some(contexts) = container(material.locate([t::MATERIAL])) else {
        return out;
    };
    for context in contexts.names() {
        let Some(network) = container(contexts.get(&context)) else {
            continue;
        };
        let mut n = Network {
            material: material.path().clone(),
            ..Network::default()
        };
        if let Some(nodes) = container(network.get(&t::NODES.into())) {
            for name in nodes.names() {
                if let Some(source) = container(nodes.get(&name)) {
                    n.nodes.insert(name.as_str().to_owned(), node(&source));
                }
            }
        }
        if let Some(terminals) = container(network.get(&t::TERMINALS.into())) {
            for name in terminals.names() {
                if let Some(c) = container(terminals.get(&name)).and_then(|c| connection(&c)) {
                    n.terminals.insert(name.as_str().to_owned(), c);
                }
            }
        }
        if let Some(parameters) = container(network.get(&t::INTERFACE.into()))
            .and_then(|i| container(i.get(&t::PARAMETERS.into())))
        {
            for name in parameters.names() {
                let Some(mappings) = container(parameters.get(&name))
                    .and_then(|p| p.get(&t::MAPPINGS.into()))
                    .and_then(|d| d.as_vector().cloned())
                else {
                    continue;
                };
                let consumers: Vec<(String, String)> = (0..mappings.len())
                    .filter_map(|i| container(mappings.get(i)))
                    .filter_map(|m| {
                        Some((
                            token(m.get(&t::NODE_PATH.into()))?,
                            token(m.get(&t::INPUT_NAME.into()))?,
                        ))
                    })
                    .collect();
                n.interface.insert(name.as_str().to_owned(), consumers);
            }
        }
        out.insert(context.as_str().to_owned(), n);
    }
    out
}

impl Network {
    /// The stage path of the node named `name`.
    pub(in crate::scene::usd_import) fn path(&self, name: &str) -> Option<sdf::Path> {
        if name.starts_with('/') {
            sdf::Path::new(name).ok()
        } else {
            sdf::Path::new(format!("{}/{name}", self.material).as_str()).ok()
        }
    }

    /// The node a terminal reads, with its name.
    pub(in crate::scene::usd_import) fn terminal(&self, terminal: &str) -> Option<(&str, &Node)> {
        let c = self.terminals.get(terminal)?;
        self.nodes
            .get_key_value(&c.node)
            .map(|(k, v)| (k.as_str(), v))
    }
}

/// The terminal `terminal` resolved over `contexts` in order, the universal
/// context last when not listed (`UsdShadeMaterial::ComputeSurfaceSource`):
/// the first context with that terminal commits, and its node is `None`
/// when the terminal's source is no shader.
pub(in crate::scene::usd_import) fn resolve<'a>(
    networks: &'a BTreeMap<String, Network>,
    terminal: &str,
    contexts: &[&str],
) -> Option<(&'a Network, Option<(&'a str, &'a Node)>)> {
    let universal_listed = contexts.contains(&"");
    contexts
        .iter()
        .copied()
        .chain((!universal_listed).then_some(""))
        .find_map(|context| {
            let network = networks.get(context)?;
            network
                .terminals
                .contains_key(terminal)
                .then(|| (network, network.terminal(terminal)))
        })
}
