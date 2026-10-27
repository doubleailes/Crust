//! A material's shading networks as the scene index gives them: Moho's
//! `material` data source, Hydra's material network schema — per render
//! context, the shader nodes reachable from the terminals, each with its
//! identifier, its parameters (the values its authored inputs carry, a
//! connection to an interface input followed) and its input connections to
//! upstream nodes' outputs.
//!
//! The readers that build Crust's materials (`preview`, `mtlx_network`,
//! `materials`) walk these networks, and read a node's own attributes, where
//! the direct readers read the shader input itself rather than what it is
//! connected to, from the node prim's generic data ([`Network::prim`]).

use std::collections::{BTreeMap, HashSet};

use moho::data_source::{ContainerHandle, DataSource};
use moho::openusd::sdf;
use moho::tokens::material as t;

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
    /// For a value graph ([`graph`]) only: the graph's own interface input
    /// the parameter is connected to, a channel the host supplies per
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

    /// The node named `name`'s prim, for its own attributes, read by `at`
    /// (the walked index, through the bridge where the material is named
    /// inside an instance).
    pub(in crate::scene::usd_import) fn prim(
        &self,
        at: &dyn Fn(&sdf::Path) -> PrimData,
        name: &str,
    ) -> Option<PrimData> {
        Some(at(&self.path(name)?))
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

/// How many forwarding hops (graph outputs, interface inputs) one connection
/// of a value graph may take before the walk gives up on it: a cycle, in
/// practice.
const MAX_HOPS: usize = 32;

/// What a value graph's `outputs:<name>` reads.
#[derive(Clone, Debug)]
pub(in crate::scene::usd_import) enum GraphOutput {
    /// A shader node's output.
    Node(Connection),
    /// A value: the output's own, an interface input's, or (with
    /// [`Parameter::interface`]) the graph's interface channel.
    Value(Parameter),
}

/// A `NodeGraph` read as a value graph (a sample filter's): its shader nodes,
/// as a material [`Network`] holds them (named by stage path), and its
/// outputs.
#[derive(Clone, Debug, Default)]
pub(in crate::scene::usd_import) struct Graph {
    pub(in crate::scene::usd_import) network: Network,
    /// Each `outputs:<name>`, by attribute path: the name, its USD type and
    /// what it reads.
    pub(in crate::scene::usd_import) outputs: Vec<(String, String, GraphOutput)>,
    /// What could not be followed, for one warning per graph.
    pub(in crate::scene::usd_import) reported: Vec<String>,
}

/// The value graph at `graph`, read from the prims' generic data
/// (`__usdAttributes`): a sample filter's graph is no material, so the
/// scene index gives it no `material` network. Connections are followed as
/// a material network's are (`UsdShadeConnectableAPI`'s value-producing
/// attributes: through `NodeGraph` outputs and interface inputs to a shader
/// output or a value), except that one to the graph's own interface input
/// is that channel ([`Parameter::interface`]). A node is a prim whose USD
/// type is `Shader` (`__usdPrimInfo/typeName`).
pub(in crate::scene::usd_import) fn graph(
    at: &dyn Fn(&sdf::Path) -> PrimData,
    graph: &sdf::Path,
) -> Graph {
    let prim = at(graph);
    let mut walk = GraphWalk {
        at,
        graph: graph.clone(),
        active: HashSet::new(),
        out: Graph {
            network: Network {
                material: graph.clone(),
                ..Network::default()
            },
            ..Graph::default()
        },
    };
    let mut names: Vec<String> = prim
        .attribute_names()
        .into_iter()
        .filter(|n| n.starts_with("outputs:"))
        .collect();
    names.sort_by_key(|n| prim.attribute_path(n));
    for full in names {
        let Some(name) = full.strip_prefix("outputs:") else {
            continue;
        };
        let usd_type = prim.type_name(&full);
        match walk.resolve(&prim, &full, 0) {
            Some(output) => walk.out.outputs.push((name.to_owned(), usd_type, output)),
            None => walk
                .out
                .reported
                .push(format!("{graph}.outputs:{name}: connects to nothing")),
        }
    }
    walk.out
}

struct GraphWalk<'a> {
    at: &'a dyn Fn(&sdf::Path) -> PrimData,
    graph: sdf::Path,
    /// Shader prims being walked, for cycle detection.
    active: HashSet<String>,
    out: Graph,
}

impl GraphWalk<'_> {
    /// Where attribute `name` of `prim` leads: an upstream shader output, a
    /// value, or the graph's interface channel. `None` when it leads nowhere
    /// (an unconnected, unauthored input; a cycle).
    fn resolve(&mut self, prim: &PrimData, name: &str, hops: usize) -> Option<GraphOutput> {
        let Some(target) = prim.connections(name).into_iter().next() else {
            // An input or output with no connection forwards its own value,
            // if it has one.
            return prim.value(name).map(|value| {
                GraphOutput::Value(Parameter {
                    value: Some(value),
                    type_name: Some(prim.type_name(name)).filter(|t| !t.is_empty()),
                    color_space: color_space_of(prim, name),
                    interface: None,
                })
            });
        };
        if hops >= MAX_HOPS {
            self.out.reported.push(format!(
                "{}: connection chain too long — cut",
                prim.attribute_path(name)
            ));
            return None;
        }
        let (prim_path, prop) = target.split_property()?;
        let prop = prop.to_owned();
        let upstream = (self.at)(&prim_path);
        if prim_path == self.graph
            && let Some(channel) = prop.strip_prefix("inputs:")
        {
            return Some(GraphOutput::Value(Parameter {
                value: upstream.value(&prop),
                type_name: Some(upstream.type_name(&prop)).filter(|t| !t.is_empty()),
                color_space: color_space_of(&upstream, &prop),
                interface: Some(channel.to_owned()),
            }));
        }
        match prop.strip_prefix("outputs:") {
            Some(output) if upstream.usd_type() == "Shader" => {
                let node = self.node(&upstream)?;
                Some(GraphOutput::Node(Connection {
                    node,
                    output: output.to_owned(),
                }))
            }
            // A container's output or interface input forwards whatever it
            // is connected to, or its own value.
            _ => self.resolve(&upstream, &prop, hops + 1),
        }
    }

    /// The node for the shader prim `prim`, walking it (and everything
    /// upstream of it) on first use; its name is its stage path.
    fn node(&mut self, prim: &PrimData) -> Option<String> {
        let name = prim.path().to_string();
        if self.out.network.nodes.contains_key(&name) {
            return Some(name);
        }
        if !self.active.insert(name.clone()) {
            self.out
                .reported
                .push(format!("{name}: connection cycle — cut"));
            return None;
        }
        let mut node = Node {
            id: prim.custom_token("info:id").unwrap_or_default(),
            ..Node::default()
        };
        for full in prim.attribute_names() {
            let Some(input) = full.strip_prefix("inputs:") else {
                continue;
            };
            match self.resolve(prim, &full, 0) {
                Some(GraphOutput::Node(c)) => {
                    node.connections.insert(input.to_owned(), vec![c]);
                }
                Some(GraphOutput::Value(p)) => {
                    node.parameters.insert(input.to_owned(), p);
                }
                None => {}
            }
        }
        self.active.remove(&name);
        self.out.network.nodes.insert(name.clone(), node);
        Some(name)
    }
}

/// Attribute `name`'s own authored `colorSpace`.
fn color_space_of(prim: &PrimData, name: &str) -> Option<String> {
    match prim.metadata(name, moho::tokens::usd::COLOR_SPACE)? {
        sdf::Value::Token(t) => Some(t.as_str().to_owned()),
        sdf::Value::String(s) => Some(s),
        _ => None,
    }
}
