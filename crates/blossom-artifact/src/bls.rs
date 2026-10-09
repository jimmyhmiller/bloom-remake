//! A Blossom `.bls` program compiled for one deployment (LANGUAGE §6.10): the IR program every node runs (rules
//! placed at a role carry the role guard), the deployment's nodes and their roles.

use std::collections::BTreeMap;

use blossom_base::{FnId, RelId, RoleId, Span, Symbol};
use blossom_ir::ValidatedProgram;
use blossom_ir::core::{EventSource, RelClass};
use blossom_value::time::NodeId;

/// A compiled `.bls` program. The deployment's node `nodes[i]` is `NodeId(i)`; names are sorted (canonical directory
/// order, ARCHITECTURE §5.9).
#[derive(Clone)]
pub struct BlsArtifact {
    pub nodes: Vec<Symbol>,
    /// Each node's role (`None` in a role-free program).
    pub roles: Vec<Option<RoleId>>,
    pub program: ValidatedProgram,
    /// For every surface relation: the IR column of each declared column, in declaration order (a channel's
    /// destination is IR column 0, CR-14).
    pub surface: BTreeMap<RelId, Vec<usize>>,
    /// The built-in `halt(kill: bool)` output, if the program writes it.
    pub halt: Option<RelId>,
    /// The function of each method of a product lattice (LANGUAGE §11.8), with the span of the method's name: where
    /// the law harness reports a refuted class claim (BLS0704).
    pub methods: BTreeMap<FnId, Span>,
}

impl BlsArtifact {
    /// The deployment's keyed members (docs/design/KEYED.md): a node of a keyed role is the member its name keys.
    pub fn members(&self) -> Result<blossom_ir::members::Members, String> {
        let names: Vec<&str> = self.nodes.iter().map(|n| n.as_str()).collect();
        blossom_ir::members::of_deployment(self.program.get(), &names, &self.roles)
    }

    /// The node named `name`.
    pub fn node_id(&self, name: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .position(|n| n.as_str() == name)
            .and_then(|i| u32::try_from(i).ok())
            .map(NodeId)
    }

    /// The relation whose qualified name prints as `name` (`r`, or `a.r` inside an instance).
    pub fn rel_named(&self, name: &str) -> Option<RelId> {
        self.program
            .get()
            .rels
            .iter_enumerated()
            .find(|(_, r)| r.name.to_string() == name)
            .map(|(id, _)| id)
    }

    /// The IR columns of `rel`'s declared columns, in declaration order.
    pub fn surface_columns(&self, rel: RelId) -> Option<&[usize]> {
        self.surface.get(&rel).map(Vec::as_slice)
    }

    /// The `boot()` relation, if the program reads it.
    pub fn boot(&self) -> Option<RelId> {
        self.program
            .get()
            .rels
            .iter_enumerated()
            .find(|(_, r)| matches!(r.class, RelClass::Event(EventSource::Boot)))
            .map(|(id, _)| id)
    }
}
