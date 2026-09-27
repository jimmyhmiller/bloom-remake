//! Graph algorithms shared by the analyses and the planner (ARCHITECTURE §1.2, §3.9, §7.2).
//!
//! Every algorithm is iterative (no recursion depth limit), deterministic (results depend only on the graph and
//! the order in which it yields successors), and validates its input: a successor outside `0..node_count` is a
//! [`GraphError::NodeOutOfRange`], never a panic.
//!
//! - [`tarjan_scc`] — strongly connected components, numbered in reverse topological order;
//! - [`condensation`] — the DAG of components;
//! - [`topo_sort`] — the smallest-index-first topological order, or a witness cycle;
//! - [`shortest_cycle_through_edge`] — the BFS-shortest cycle through an edge (stratification witnesses, ANA-002);
//! - [`shortest_cycle_through_node`] — the BFS-shortest cycle through a node;
//! - [`max_bipartite_matching`] — Hopcroft–Karp;
//! - [`min_chain_cover`] — a minimum chain cover of a strict partial order (Dilworth), used for index selection.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

/// A directed graph over the nodes `0..node_count()`.
pub trait DirectedGraph {
    /// The number of nodes.
    fn node_count(&self) -> usize;

    /// The successors of `node`, in a fixed order. Multi-edges are allowed.
    fn successors(&self, node: usize) -> impl Iterator<Item = usize> + '_;
}

/// A graph stored as one successor list per node.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AdjacencyList {
    succ: Vec<Vec<usize>>,
}

impl AdjacencyList {
    /// A graph with `nodes` nodes and no edges.
    pub fn new(nodes: usize) -> AdjacencyList {
        AdjacencyList {
            succ: vec![Vec::new(); nodes],
        }
    }

    /// A graph with `nodes` nodes and the given edges.
    pub fn from_edges(
        nodes: usize,
        edges: impl IntoIterator<Item = (usize, usize)>,
    ) -> Result<AdjacencyList, GraphError> {
        let mut g = AdjacencyList::new(nodes);
        for (from, to) in edges {
            g.add_edge(from, to)?;
        }
        Ok(g)
    }

    /// Adds the edge `from → to`.
    pub fn add_edge(&mut self, from: usize, to: usize) -> Result<(), GraphError> {
        let count = self.succ.len();
        if to >= count {
            return Err(GraphError::NodeOutOfRange { node: to, count });
        }
        self.succ
            .get_mut(from)
            .ok_or(GraphError::NodeOutOfRange { node: from, count })?
            .push(to);
        Ok(())
    }

    /// The successors of `node` (empty for a node out of range).
    pub fn successor_slice(&self, node: usize) -> &[usize] {
        self.succ.get(node).map_or(&[], Vec::as_slice)
    }

    /// The number of edges.
    pub fn edge_count(&self) -> usize {
        self.succ.iter().map(Vec::len).sum()
    }

    /// Whether the edge `from → to` exists.
    pub fn has_edge(&self, from: usize, to: usize) -> bool {
        self.successor_slice(from).contains(&to)
    }
}

impl DirectedGraph for AdjacencyList {
    fn node_count(&self) -> usize {
        self.succ.len()
    }
    fn successors(&self, node: usize) -> impl Iterator<Item = usize> + '_ {
        self.successor_slice(node).iter().copied()
    }
}

/// Errors from the graph algorithms.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    /// A node index outside the graph.
    #[error("node {node} is out of range for a graph of {count} nodes")]
    NodeOutOfRange {
        /// The offending index.
        node: usize,
        /// The number of nodes.
        count: usize,
    },
    /// An edge that the graph does not have.
    #[error("the graph has no edge {from} -> {to}")]
    NoSuchEdge {
        /// Source.
        from: usize,
        /// Target.
        to: usize,
    },
    /// The graph is not acyclic; `cycle` lists the nodes of a witness cycle in order (the last has an edge back to
    /// the first). The function that reports it says which cycle it is.
    #[error("the graph has a cycle: {cycle:?}")]
    Cycle {
        /// The witness cycle.
        cycle: Vec<usize>,
    },
    /// A relation given as a strict partial order is not one.
    #[error("not a strict partial order: {reason}")]
    NotStrictPartialOrder {
        /// Which law fails, with a witness.
        reason: String,
    },
    /// A violated invariant of the algorithm itself (a bug; see [`bug!`](crate::bug)).
    #[error("{what}")]
    Internal {
        /// The internal error's message.
        what: String,
    },
    /// Adjacency lists and the declared side sizes of a bipartite graph disagree.
    #[error("the bipartite graph has {lists} adjacency lists for {left} left vertices")]
    BipartiteShape {
        /// Number of adjacency lists given.
        lists: usize,
        /// Declared number of left vertices.
        left: usize,
    },
}

fn out_of_range(node: usize, count: usize) -> GraphError {
    GraphError::NodeOutOfRange { node, count }
}

/// Strongly connected components.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sccs {
    /// The components, each sorted ascending, in **reverse topological order**: if an edge leads from component `a`
    /// to a different component `b`, then `b < a`.
    pub components: Vec<Vec<usize>>,
    /// The component of every node.
    pub component_of: Vec<usize>,
}

impl Sccs {
    /// The number of components.
    pub fn count(&self) -> usize {
        self.components.len()
    }

    /// Whether component `c` contains a cycle (more than one node, or a node with a self-loop).
    pub fn is_cyclic<G: DirectedGraph + ?Sized>(&self, graph: &G, c: usize) -> bool {
        match self.components.get(c).map(Vec::as_slice) {
            Some([single]) => graph.successors(*single).any(|s| s == *single),
            Some(nodes) => nodes.len() > 1,
            None => false,
        }
    }
}

#[derive(Clone, Copy)]
struct TarjanNode {
    index: usize,
    low: usize,
    on_stack: bool,
}

const UNVISITED: usize = usize::MAX;

/// Tarjan's strongly connected components, iteratively.
pub fn tarjan_scc<G: DirectedGraph + ?Sized>(graph: &G) -> Result<Sccs, GraphError> {
    let n = graph.node_count();
    let mut nodes = vec![
        TarjanNode {
            index: UNVISITED,
            low: 0,
            on_stack: false
        };
        n
    ];
    let mut component_of = vec![UNVISITED; n];
    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;

    let start = |nodes: &mut Vec<TarjanNode>, stack: &mut Vec<usize>, next_index: &mut usize, v: usize| {
        let node = nodes.get_mut(v).ok_or(out_of_range(v, n))?;
        *node = TarjanNode {
            index: *next_index,
            low: *next_index,
            on_stack: true,
        };
        *next_index += 1;
        stack.push(v);
        Ok::<(), GraphError>(())
    };

    for root in 0..n {
        if nodes.get(root).is_some_and(|r| r.index != UNVISITED) {
            continue;
        }
        start(&mut nodes, &mut stack, &mut next_index, root)?;
        let mut call = vec![(root, graph.successors(root))];
        while let Some((v, next)) = call.last_mut().map(|(v, succ)| (*v, succ.next())) {
            match next {
                Some(w) => {
                    let wn = *nodes.get(w).ok_or(out_of_range(w, n))?;
                    if wn.index == UNVISITED {
                        start(&mut nodes, &mut stack, &mut next_index, w)?;
                        call.push((w, graph.successors(w)));
                    } else if wn.on_stack {
                        let vn = nodes.get_mut(v).ok_or(out_of_range(v, n))?;
                        vn.low = vn.low.min(wn.index);
                    }
                }
                None => {
                    call.pop();
                    let vn = *nodes.get(v).ok_or(out_of_range(v, n))?;
                    if vn.low == vn.index {
                        let id = components.len();
                        let mut component = Vec::new();
                        while let Some(w) = stack.pop() {
                            if let Some(wn) = nodes.get_mut(w) {
                                wn.on_stack = false;
                            }
                            if let Some(c) = component_of.get_mut(w) {
                                *c = id;
                            }
                            component.push(w);
                            if w == v {
                                break;
                            }
                        }
                        component.sort_unstable();
                        components.push(component);
                    }
                    if let Some((u, _)) = call.last() {
                        let un = nodes.get_mut(*u).ok_or(out_of_range(*u, n))?;
                        un.low = un.low.min(vn.low);
                    }
                }
            }
        }
    }
    Ok(Sccs {
        components,
        component_of,
    })
}

/// The condensation of `graph`: one node per component of `sccs` and an edge between two components when some
/// edge crosses them. Successor lists are sorted and deduplicated.
pub fn condensation<G: DirectedGraph + ?Sized>(graph: &G, sccs: &Sccs) -> Result<AdjacencyList, GraphError> {
    let n = graph.node_count();
    let mut dag = AdjacencyList::new(sccs.count());
    for v in 0..n {
        let cv = *sccs.component_of.get(v).ok_or(out_of_range(v, n))?;
        for w in graph.successors(v) {
            let cw = *sccs.component_of.get(w).ok_or(out_of_range(w, n))?;
            if cv != cw {
                dag.add_edge(cv, cw)?;
            }
        }
    }
    for list in &mut dag.succ {
        list.sort_unstable();
        list.dedup();
    }
    Ok(dag)
}

/// A topological order of `graph`, taking the smallest available node first (so the result is canonical). A cyclic
/// graph yields [`GraphError::Cycle`] with a shortest cycle through the smallest node of the first cyclic
/// component (in [`tarjan_scc`] order).
pub fn topo_sort<G: DirectedGraph + ?Sized>(graph: &G) -> Result<Vec<usize>, GraphError> {
    let n = graph.node_count();
    let mut indegree = vec![0usize; n];
    for v in 0..n {
        for w in graph.successors(v) {
            *indegree.get_mut(w).ok_or(out_of_range(w, n))? += 1;
        }
    }
    let mut ready: BinaryHeap<Reverse<usize>> = indegree
        .iter()
        .enumerate()
        .filter(|(_, d)| **d == 0)
        .map(|(v, _)| Reverse(v))
        .collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse(v)) = ready.pop() {
        order.push(v);
        for w in graph.successors(v) {
            let d = indegree.get_mut(w).ok_or(out_of_range(w, n))?;
            *d -= 1;
            if *d == 0 {
                ready.push(Reverse(w));
            }
        }
    }
    if order.len() == n {
        return Ok(order);
    }
    let sccs = tarjan_scc(graph)?;
    for (c, component) in sccs.components.iter().enumerate() {
        if !sccs.is_cyclic(graph, c) {
            continue;
        }
        let Some(&smallest) = component.first() else { continue };
        let within = |x: usize| sccs.component_of.get(x) == Some(&c);
        if let Some(cycle) = shortest_cycle_through_node(graph, smallest, within)? {
            return Err(GraphError::Cycle { cycle });
        }
    }
    // Kahn's algorithm left nodes unordered, so some component is cyclic; a cyclic component is strongly connected
    // (or a self-loop), so every one of its nodes lies on a cycle inside it and the loop above returned one.
    Err(GraphError::Internal {
        what: crate::internal_error!("topo_sort found no witness for a cyclic graph").to_string(),
    })
}

/// The shortest cycle through `node` whose other nodes all satisfy `within`. The result starts with `node`; its last
/// node has an edge back to `node`; a self-loop is `[node]`. Among several shortest cycles it is the first that a
/// breadth-first search from `node` finds, visiting successors in the graph's order. `Ok(None)` when `node` lies on
/// no such cycle.
pub fn shortest_cycle_through_node<G: DirectedGraph + ?Sized>(
    graph: &G,
    node: usize,
    within: impl Fn(usize) -> bool,
) -> Result<Option<Vec<usize>>, GraphError> {
    let n = graph.node_count();
    let mut parent = vec![UNVISITED; n];
    *parent.get_mut(node).ok_or(out_of_range(node, n))? = node;
    // Nodes leave the queue in order of their distance from `node`, so the first edge back to `node` closes a
    // shortest cycle.
    let mut queue = VecDeque::from([node]);
    while let Some(v) = queue.pop_front() {
        for w in graph.successors(v) {
            if w == node {
                let mut path = vec![v];
                let mut cur = v;
                while cur != node {
                    cur = *parent.get(cur).ok_or(out_of_range(cur, n))?;
                    path.push(cur);
                }
                path.reverse();
                return Ok(Some(path));
            }
            let seen = parent.get_mut(w).ok_or(out_of_range(w, n))?;
            if *seen == UNVISITED && within(w) {
                *seen = v;
                queue.push_back(w);
            }
        }
    }
    Ok(None)
}

/// The shortest cycle that uses the edge `from → to`, visiting only nodes for which `within` holds (besides `from`
/// and `to`). The result starts `[from, to, …]`; its last node has an edge back to `from`; a self-loop is
/// `[from]`. `Ok(None)` when no such cycle exists; an error when the edge itself is absent.
pub fn shortest_cycle_through_edge<G: DirectedGraph + ?Sized>(
    graph: &G,
    from: usize,
    to: usize,
    within: impl Fn(usize) -> bool,
) -> Result<Option<Vec<usize>>, GraphError> {
    let n = graph.node_count();
    for node in [from, to] {
        if node >= n {
            return Err(out_of_range(node, n));
        }
    }
    if !graph.successors(from).any(|w| w == to) {
        return Err(GraphError::NoSuchEdge { from, to });
    }
    if from == to {
        return Ok(Some(vec![from]));
    }
    // Breadth-first search from `to` back to `from`.
    let mut parent = vec![UNVISITED; n];
    if let Some(p) = parent.get_mut(to) {
        *p = to;
    }
    let mut queue = VecDeque::from([to]);
    while let Some(v) = queue.pop_front() {
        for w in graph.successors(v) {
            if w == from {
                let mut path = vec![v];
                let mut cur = v;
                while cur != to {
                    cur = *parent.get(cur).ok_or(out_of_range(cur, n))?;
                    path.push(cur);
                }
                path.push(from);
                path.reverse();
                return Ok(Some(path));
            }
            let seen = parent.get_mut(w).ok_or(out_of_range(w, n))?;
            if *seen == UNVISITED && within(w) {
                *seen = v;
                queue.push_back(w);
            }
        }
    }
    Ok(None)
}

/// A matching of a bipartite graph.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Matching {
    /// The right vertex matched to each left vertex.
    pub left_to_right: Vec<Option<usize>>,
    /// The left vertex matched to each right vertex.
    pub right_to_left: Vec<Option<usize>>,
}

impl Matching {
    /// The number of matched pairs.
    pub fn size(&self) -> usize {
        self.left_to_right.iter().filter(|m| m.is_some()).count()
    }

    /// The matched `(left, right)` pairs, by left vertex.
    pub fn pairs(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.left_to_right
            .iter()
            .enumerate()
            .filter_map(|(l, r)| r.map(|r| (l, r)))
    }
}

/// A maximum matching of the bipartite graph with `left` and `right` vertices where `adjacency[l]` lists the right
/// neighbours of left vertex `l` (Hopcroft–Karp, O(E·√V)).
pub fn max_bipartite_matching(left: usize, right: usize, adjacency: &[Vec<usize>]) -> Result<Matching, GraphError> {
    if adjacency.len() != left {
        return Err(GraphError::BipartiteShape {
            lists: adjacency.len(),
            left,
        });
    }
    for neighbours in adjacency {
        if let Some(&bad) = neighbours.iter().find(|&&r| r >= right) {
            return Err(out_of_range(bad, right));
        }
    }
    let mut m = Matching {
        left_to_right: vec![None; left],
        right_to_left: vec![None; right],
    };
    let mut dist = vec![UNVISITED; left];
    // Each phase augments along a maximal set of vertex-disjoint *shortest* augmenting paths; O(√V) phases suffice.
    while let Some(limit) = bfs_layers(adjacency, &m, &mut dist) {
        for l in 0..left {
            if m.left_to_right.get(l).is_some_and(Option::is_none) {
                augment_from(l, adjacency, &mut m, &mut dist, limit);
            }
        }
    }
    Ok(m)
}

/// Layers the left vertices by alternating BFS from the free ones (layer 0). Returns the layer of the left vertices
/// adjacent to a free right vertex — the length, in left vertices, of a shortest augmenting path — or `None` when
/// there is no augmenting path. Layers beyond it are not expanded.
fn bfs_layers(adjacency: &[Vec<usize>], m: &Matching, dist: &mut [usize]) -> Option<usize> {
    let mut queue = VecDeque::new();
    for (l, d) in dist.iter_mut().enumerate() {
        if m.left_to_right.get(l).is_some_and(Option::is_none) {
            *d = 0;
            queue.push_back(l);
        } else {
            *d = UNVISITED;
        }
    }
    let mut limit: Option<usize> = None;
    // The queue yields vertices by nondecreasing layer, so the first free right vertex found fixes the limit.
    while let Some(l) = queue.pop_front() {
        let dl = dist.get(l).copied().unwrap_or(UNVISITED);
        if limit.is_some_and(|lim| dl > lim) {
            continue;
        }
        for &r in adjacency.get(l).map_or(&[][..], Vec::as_slice) {
            match m.right_to_left.get(r).copied().flatten() {
                None => {
                    limit.get_or_insert(dl);
                }
                Some(next) => {
                    if let Some(dn) = dist.get_mut(next)
                        && *dn == UNVISITED
                    {
                        *dn = dl + 1;
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    limit
}

/// Searches for a shortest augmenting path from the free left vertex `start` along the BFS layers (iterative DFS):
/// a free right vertex is accepted only from layer `limit`, and a matched one is followed only to the next layer.
/// Applies the path if found. Dead ends are removed from the layering.
fn augment_from(start: usize, adjacency: &[Vec<usize>], m: &mut Matching, dist: &mut [usize], limit: usize) -> bool {
    // Each frame: (left vertex, index of the next neighbour to try, the right vertex we descended through).
    let mut frames: Vec<(usize, usize, usize)> = vec![(start, 0, UNVISITED)];
    while let Some(&mut (l, ref mut next, _)) = frames.last_mut() {
        let neighbours = adjacency.get(l).map_or(&[][..], Vec::as_slice);
        let Some(&r) = neighbours.get(*next) else {
            // Exhausted: `l` cannot reach a free right vertex in this phase.
            if let Some(d) = dist.get_mut(l) {
                *d = UNVISITED;
            }
            frames.pop();
            continue;
        };
        *next += 1;
        let dl = dist.get(l).copied().unwrap_or(UNVISITED);
        match m.right_to_left.get(r).copied().flatten() {
            None if dl == limit => {
                // Augment: `r` is free. Walk the frames back, matching each left vertex with the right vertex it
                // was entered through by its child frame.
                let mut right = r;
                while let Some((left, _, via)) = frames.pop() {
                    if let Some(slot) = m.left_to_right.get_mut(left) {
                        *slot = Some(right);
                    }
                    if let Some(slot) = m.right_to_left.get_mut(right) {
                        *slot = Some(left);
                    }
                    right = via;
                }
                return true;
            }
            Some(owner) if dl < limit && dist.get(owner).copied() == Some(dl + 1) => {
                frames.push((owner, 0, r));
            }
            // A free vertex below the limit layer (a longer path), or a matched one off the layering.
            None | Some(_) => {}
        }
    }
    false
}

/// A minimum chain cover of the strict partial order `less` on `0..n` (Dilworth's theorem via bipartite matching):
/// the fewest chains, each listed in ascending order of `less`, that together contain every element exactly once.
///
/// `less` must be irreflexive, asymmetric and transitive; this is checked (O(n³)), and a violation is
/// [`GraphError::NotStrictPartialOrder`] with a witness. Chains are listed in ascending order of their first
/// element, which is the chain's least element under `less` (not necessarily its smallest index).
pub fn min_chain_cover(n: usize, less: impl Fn(usize, usize) -> bool) -> Result<Vec<Vec<usize>>, GraphError> {
    let mut relation = vec![vec![false; n]; n];
    for (i, row) in relation.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = less(i, j);
        }
    }
    let lt = |i: usize, j: usize| relation.get(i).and_then(|row| row.get(j)).copied().unwrap_or(false);
    for i in 0..n {
        if lt(i, i) {
            return Err(GraphError::NotStrictPartialOrder {
                reason: format!("{i} < {i} (not irreflexive)"),
            });
        }
        for j in 0..n {
            if lt(i, j) && lt(j, i) {
                return Err(GraphError::NotStrictPartialOrder {
                    reason: format!("{i} < {j} and {j} < {i}"),
                });
            }
            if lt(i, j) {
                for k in 0..n {
                    if lt(j, k) && !lt(i, k) {
                        return Err(GraphError::NotStrictPartialOrder {
                            reason: format!("{i} < {j} and {j} < {k} but not {i} < {k} (not transitive)"),
                        });
                    }
                }
            }
        }
    }
    let adjacency: Vec<Vec<usize>> = (0..n).map(|i| (0..n).filter(|&j| lt(i, j)).collect()).collect();
    let matching = max_bipartite_matching(n, n, &adjacency)?;
    let mut chains = Vec::with_capacity(n - matching.size());
    for head in 0..n {
        // A chain starts at every element that is nobody's successor in the matching.
        if matching.right_to_left.get(head).copied().flatten().is_some() {
            continue;
        }
        let mut chain = vec![head];
        let mut cur = head;
        while let Some(next) = matching.left_to_right.get(cur).copied().flatten() {
            chain.push(next);
            cur = next;
        }
        chains.push(chain);
    }
    Ok(chains)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn graph(n: usize, edges: &[(usize, usize)]) -> AdjacencyList {
        AdjacencyList::from_edges(n, edges.iter().copied()).unwrap()
    }

    fn reachable(g: &AdjacencyList, from: usize) -> Vec<bool> {
        let mut seen = vec![false; g.node_count()];
        let mut todo = vec![from];
        while let Some(v) = todo.pop() {
            for w in g.successors(v) {
                if !seen[w] {
                    seen[w] = true;
                    todo.push(w);
                }
            }
        }
        seen
    }

    fn arb_graph(max_nodes: usize) -> impl Strategy<Value = AdjacencyList> {
        (1..=max_nodes).prop_flat_map(|n| {
            proptest::collection::vec((0..n, 0..n), 0..n * 3).prop_map(move |edges| graph(n, &edges))
        })
    }

    #[test]
    fn tarjan_known_graph() {
        // 0 -> 1 -> 2 -> 0 is a cycle; 2 -> 3; 3 -> 4 -> 3; 5 is isolated with a self-loop.
        let g = graph(6, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 3), (5, 5)]);
        let sccs = tarjan_scc(&g).unwrap();
        assert_eq!(sccs.components, vec![vec![3, 4], vec![0, 1, 2], vec![5]]);
        assert_eq!(sccs.component_of, vec![1, 1, 1, 0, 0, 2]);
        assert!((0..3).all(|c| sccs.is_cyclic(&g, c)));
        let dag = condensation(&g, &sccs).unwrap();
        assert_eq!(dag.successor_slice(1), &[0]);
        assert_eq!(dag.edge_count(), 1);
    }

    #[test]
    fn tarjan_deep_chain_does_not_recurse() {
        let n = 200_000;
        let edges: Vec<(usize, usize)> = (0..n - 1).map(|i| (i, i + 1)).chain([(n - 1, 0)]).collect();
        let sccs = tarjan_scc(&graph(n, &edges)).unwrap();
        assert_eq!(sccs.count(), 1);
        assert_eq!(sccs.components[0].len(), n);
    }

    #[test]
    fn tarjan_rejects_bad_successor() {
        struct Bad;
        impl DirectedGraph for Bad {
            fn node_count(&self) -> usize {
                2
            }
            fn successors(&self, _: usize) -> impl Iterator<Item = usize> + '_ {
                std::iter::once(7)
            }
        }
        assert_eq!(tarjan_scc(&Bad), Err(GraphError::NodeOutOfRange { node: 7, count: 2 }));
        assert_eq!(topo_sort(&Bad), Err(GraphError::NodeOutOfRange { node: 7, count: 2 }));
    }

    proptest! {
        #[test]
        fn tarjan_matches_mutual_reachability(g in arb_graph(9)) {
            let sccs = tarjan_scc(&g).unwrap();
            let n = g.node_count();
            let reach: Vec<Vec<bool>> = (0..n).map(|v| reachable(&g, v)).collect();
            for (u, from_u) in reach.iter().enumerate() {
                for (v, from_v) in reach.iter().enumerate() {
                    let same = u == v || (from_u[v] && from_v[u]);
                    prop_assert_eq!(sccs.component_of[u] == sccs.component_of[v], same, "{} {}", u, v);
                }
            }
            // Reverse topological numbering: every cross-component edge goes to a smaller component.
            for v in 0..n {
                for w in g.successors(v) {
                    let (cv, cw) = (sccs.component_of[v], sccs.component_of[w]);
                    prop_assert!(cv == cw || cw < cv);
                }
            }
            let covered: usize = sccs.components.iter().map(Vec::len).sum();
            prop_assert_eq!(covered, n);
        }

        #[test]
        fn topo_sort_orders_dags_and_witnesses_cycles(g in arb_graph(9)) {
            match topo_sort(&g) {
                Ok(order) => {
                    let mut pos = vec![usize::MAX; g.node_count()];
                    for (i, v) in order.iter().enumerate() {
                        pos[*v] = i;
                    }
                    prop_assert!(pos.iter().all(|p| *p != usize::MAX));
                    for v in 0..g.node_count() {
                        for w in g.successors(v) {
                            prop_assert!(pos[v] < pos[w]);
                        }
                    }
                }
                Err(GraphError::Cycle { cycle }) => {
                    prop_assert!(!cycle.is_empty());
                    for (i, v) in cycle.iter().enumerate() {
                        let next = cycle[(i + 1) % cycle.len()];
                        prop_assert!(g.has_edge(*v, next), "{:?}", cycle);
                    }
                    // The witness runs through the smallest node of the first cyclic component and is a shortest
                    // cycle through it inside that component.
                    let sccs = tarjan_scc(&g).unwrap();
                    let c = (0..sccs.count()).find(|&c| sccs.is_cyclic(&g, c)).unwrap();
                    let smallest = sccs.components[c][0];
                    prop_assert_eq!(cycle[0], smallest);
                    prop_assert!(cycle.iter().all(|v| sccs.component_of[*v] == c));
                    let shortest = shortest_cycle_len_brute_force(&g, smallest, |v| sccs.component_of[v] == c);
                    prop_assert_eq!(Some(cycle.len()), shortest);
                }
                Err(other) => prop_assert!(false, "unexpected error {other:?}"),
            }
        }

        #[test]
        fn shortest_cycle_through_node_matches_brute_force(g in arb_graph(8), node in 0usize..8) {
            let node = node % g.node_count().max(1);
            if g.node_count() > 0 {
                let found = shortest_cycle_through_node(&g, node, |_| true).unwrap();
                prop_assert_eq!(found.as_ref().map(Vec::len), shortest_cycle_len_brute_force(&g, node, |_| true));
                if let Some(cycle) = found {
                    prop_assert_eq!(cycle[0], node);
                    for (i, v) in cycle.iter().enumerate() {
                        prop_assert!(g.has_edge(*v, cycle[(i + 1) % cycle.len()]), "{:?}", cycle);
                    }
                }
            }
        }
    }

    /// The length of a shortest cycle through `node` whose nodes all satisfy `within`, by exhaustive search over
    /// simple paths.
    fn shortest_cycle_len_brute_force(g: &AdjacencyList, node: usize, within: impl Fn(usize) -> bool) -> Option<usize> {
        fn walk(
            g: &AdjacencyList,
            start: usize,
            v: usize,
            len: usize,
            on_path: &mut Vec<bool>,
            within: &dyn Fn(usize) -> bool,
            best: &mut Option<usize>,
        ) {
            for w in g.successors(v) {
                if w == start {
                    *best = Some(best.map_or(len, |b| b.min(len)));
                } else if !on_path[w] && within(w) {
                    on_path[w] = true;
                    walk(g, start, w, len + 1, on_path, within, best);
                    on_path[w] = false;
                }
            }
        }
        let mut best = None;
        let mut on_path = vec![false; g.node_count()];
        on_path[node] = true;
        walk(g, node, node, 1, &mut on_path, &within, &mut best);
        best
    }

    #[test]
    fn topo_sort_smallest_first() {
        let g = graph(5, &[(3, 1), (4, 0), (2, 0)]);
        assert_eq!(topo_sort(&g).unwrap(), vec![2, 3, 1, 4, 0]);
        let cyclic = graph(4, &[(0, 1), (1, 2), (2, 3), (3, 1)]);
        assert_eq!(topo_sort(&cyclic), Err(GraphError::Cycle { cycle: vec![1, 2, 3] }));
        let self_loop = graph(2, &[(0, 1), (1, 1)]);
        assert_eq!(topo_sort(&self_loop), Err(GraphError::Cycle { cycle: vec![1] }));
    }

    #[test]
    fn topo_sort_witness_is_shortest_cycle_through_smallest_node() {
        // Both cycles run through 0, the smallest node of the only cyclic component: 0 1 3 4 0 and the shorter 0 2 0.
        // The witness is the shorter one, although 0 -> 1 is the smallest edge out of 0.
        let g = graph(5, &[(0, 1), (1, 3), (3, 4), (4, 0), (0, 2), (2, 0)]);
        assert_eq!(topo_sort(&g), Err(GraphError::Cycle { cycle: vec![0, 2] }));
        assert_eq!(
            shortest_cycle_through_node(&g, 3, |_| true).unwrap(),
            Some(vec![3, 4, 0, 1])
        );
        assert_eq!(shortest_cycle_through_node(&g, 3, |v| v != 1).unwrap(), None);
        assert_eq!(
            shortest_cycle_through_node(&g, 5, |_| true),
            Err(GraphError::NodeOutOfRange { node: 5, count: 5 })
        );
    }

    #[test]
    fn topo_sort_shortest_cycle_through_edge() {
        // Two cycles through 0 -> 1: 0 1 2 3 0 (long) and 0 1 4 0 (short).
        let g = graph(5, &[(0, 1), (1, 2), (2, 3), (3, 0), (1, 4), (4, 0)]);
        assert_eq!(
            shortest_cycle_through_edge(&g, 0, 1, |_| true).unwrap(),
            Some(vec![0, 1, 4])
        );
        assert_eq!(
            shortest_cycle_through_edge(&g, 0, 1, |v| v != 4).unwrap(),
            Some(vec![0, 1, 2, 3])
        );
        assert_eq!(shortest_cycle_through_edge(&g, 0, 1, |v| v == 0).unwrap(), None);
        assert_eq!(
            shortest_cycle_through_edge(&g, 0, 2, |_| true),
            Err(GraphError::NoSuchEdge { from: 0, to: 2 })
        );
    }

    fn brute_force_matching(left: usize, adjacency: &[Vec<usize>], l: usize, used: &mut Vec<bool>) -> usize {
        if l == left {
            return 0;
        }
        let mut best = brute_force_matching(left, adjacency, l + 1, used);
        for &r in &adjacency[l] {
            if !used[r] {
                used[r] = true;
                best = best.max(1 + brute_force_matching(left, adjacency, l + 1, used));
                used[r] = false;
            }
        }
        best
    }

    proptest! {
        #[test]
        fn hopcroft_karp_matches_brute_force(
            left in 0usize..7,
            right in 1usize..7,
            seed in proptest::collection::vec(proptest::collection::vec(0usize..16, 0..6), 7),
        ) {
            let adjacency: Vec<Vec<usize>> =
                seed.iter().take(left).map(|ns| ns.iter().map(|r| r % right).collect()).collect();
            let m = max_bipartite_matching(left, right, &adjacency).unwrap();
            // A valid matching over existing edges, consistent in both directions.
            for (l, r) in m.pairs() {
                prop_assert!(adjacency[l].contains(&r));
                prop_assert_eq!(m.right_to_left[r], Some(l));
            }
            prop_assert_eq!(m.right_to_left.iter().filter(|x| x.is_some()).count(), m.size());
            let best = brute_force_matching(left, &adjacency, 0, &mut vec![false; right]);
            prop_assert_eq!(m.size(), best);
        }
    }

    #[test]
    fn hopcroft_karp_long_augmenting_paths() {
        // Left i lists right i+1 before right i. A greedy first pass matches i -> i+1 and leaves left n-1 free
        // behind an augmenting path through every vertex; the maximum matching is perfect (i -> i).
        let n = 2000;
        let adjacency: Vec<Vec<usize>> = (0..n)
            .map(|i| if i + 1 < n { vec![i + 1, i] } else { vec![i] })
            .collect();
        let m = max_bipartite_matching(n, n, &adjacency).unwrap();
        assert_eq!(m.size(), n);
        for (l, r) in m.pairs() {
            assert!(adjacency[l].contains(&r));
            assert_eq!(m.right_to_left[r], Some(l));
        }
    }

    #[test]
    fn hopcroft_karp_shape_errors() {
        assert_eq!(
            max_bipartite_matching(2, 2, &[vec![0]]),
            Err(GraphError::BipartiteShape { lists: 1, left: 2 })
        );
        assert_eq!(
            max_bipartite_matching(1, 2, &[vec![5]]),
            Err(GraphError::NodeOutOfRange { node: 5, count: 2 })
        );
    }

    /// The largest antichain of a poset on `0..n` by exhaustive search (n ≤ 10).
    fn max_antichain(n: usize, lt: &dyn Fn(usize, usize) -> bool) -> usize {
        (0u32..(1 << n))
            .filter(|mask| {
                let members: Vec<usize> = (0..n).filter(|i| mask & (1 << i) != 0).collect();
                members.iter().all(|&a| members.iter().all(|&b| !lt(a, b)))
            })
            .map(u32::count_ones)
            .max()
            .unwrap_or(0) as usize
    }

    proptest! {
        #[test]
        fn chain_cover_is_minimum(n in 1usize..9, edges in proptest::collection::vec((0usize..9, 0usize..9), 0..20)) {
            // Random DAG (edges from lower to higher index), then its transitive closure: a strict partial order.
            let mut lt = vec![vec![false; n]; n];
            for (a, b) in edges {
                let (a, b) = (a % n, b % n);
                if a < b {
                    lt[a][b] = true;
                }
            }
            for k in 0..n {
                for i in 0..n {
                    for j in 0..n {
                        if lt[i][k] && lt[k][j] {
                            lt[i][j] = true;
                        }
                    }
                }
            }
            let less = |a: usize, b: usize| lt[a][b];
            let chains = min_chain_cover(n, less).unwrap();
            let mut seen = vec![false; n];
            for chain in &chains {
                for pair in chain.windows(2) {
                    prop_assert!(less(pair[0], pair[1]));
                }
                for &v in chain {
                    prop_assert!(!seen[v]);
                    seen[v] = true;
                }
            }
            prop_assert!(seen.iter().all(|s| *s));
            prop_assert_eq!(chains.len(), max_antichain(n, &less));
        }
    }

    #[test]
    fn chain_cover_rejects_non_orders() {
        let not_transitive = |a: usize, b: usize| (a, b) == (0, 1) || (a, b) == (1, 2);
        assert!(matches!(
            min_chain_cover(3, not_transitive),
            Err(GraphError::NotStrictPartialOrder { .. })
        ));
        assert!(matches!(
            min_chain_cover(2, |a, b| a == b),
            Err(GraphError::NotStrictPartialOrder { .. })
        ));
        assert!(matches!(
            min_chain_cover(2, |a, b| a != b),
            Err(GraphError::NotStrictPartialOrder { .. })
        ));
        assert_eq!(min_chain_cover(0, |_, _| false).unwrap(), Vec::<Vec<usize>>::new());
        // A total order is one chain; an antichain is n singleton chains.
        assert_eq!(min_chain_cover(4, |a, b| a < b).unwrap(), vec![vec![0, 1, 2, 3]]);
        assert_eq!(
            min_chain_cover(3, |_, _| false).unwrap(),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn chain_cover_lists_chains_by_first_element() {
        // 3 < 0 is the only relation: the chain [3, 0] starts at its least element 3, so it comes after [1] and [2]
        // although it holds the smallest index.
        assert_eq!(
            min_chain_cover(4, |a, b| (a, b) == (3, 0)).unwrap(),
            vec![vec![1], vec![2], vec![3, 0]]
        );
    }
}
