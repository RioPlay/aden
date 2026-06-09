// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Community detection over the knowledge graph.
//!
//! Groups symbols into *functional communities* — clusters more densely
//! connected to each other (by calls/uses/references) than to the rest of the
//! graph — so `ask` and code-comprehension can talk about subsystems, not just
//! files. Communities are derived from the actual edge structure, independent of
//! the directory layout.
//!
//! Algorithm: multi-level modularity optimization (Louvain — Blondel, Guillaume,
//! Lambiotte & Lefebvre, 2008). This is the core that produces the clusters; the
//! Leiden refinement phase (Traag, Waltman & van Eck, 2019), which additionally
//! *guarantees* each community is internally connected, is a planned enhancement
//! on top of this.
//!
//! Determinism (aden's guarantee): nodes are processed in *anchor-sorted* order
//! (NOT raw `NodeIndex` order — graph construction inserts in HashMap-iteration
//! order, which differs per process), candidate communities are accumulated in a
//! `BTreeMap`, and ties are broken by lowest community id — so the partition is
//! identical run-to-run, no RNG.

use crate::graph::AdenGraph;
use crate::nodes::{AdenEdge, DocumentNode, GraphNode};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{BTreeMap, HashMap};

/// Detect communities and return them as groups of anchors, largest first
/// (anchors within a group sorted; groups sorted by size desc then first anchor,
/// so the output is fully deterministic). Singletons (a symbol in its own
/// community) are included; callers can filter by size.
/// `resolution` (γ ≥ 0) tunes cluster granularity: 1.0 is standard modularity;
/// higher values penalize large communities more, yielding more, smaller, finer
/// clusters (a principled mitigation of modularity's resolution limit).
pub fn detect_communities(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    resolution: f64,
) -> Vec<Vec<String>> {
    // Louvain is order-sensitive, and `node_indices()` order follows graph
    // construction (which inserts in HashMap-iteration order — non-deterministic
    // across process runs). Sort by anchor to give the algorithm a *canonical*
    // node order, so communities are reproducible run-to-run (aden's guarantee).
    let mut nodes: Vec<NodeIndex> = graph.graph.node_indices().collect();
    nodes.sort_by(|&a, &b| graph.graph[a].anchor().cmp(graph.graph[b].anchor()));
    let n = nodes.len();
    if n == 0 {
        return Vec::new();
    }
    let id_of: HashMap<NodeIndex, usize> = nodes.iter().enumerate().map(|(i, &ix)| (ix, i)).collect();

    // Undirected weighted adjacency: collapse edge direction, accumulate parallel
    // edges, drop self-loops. Edge weight is 1 per edge (any type counts as a
    // dependency tie). adj[a] maps neighbor -> weight.
    let mut adj: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for e in graph.graph.edge_references() {
        let a = id_of[&e.source()];
        let b = id_of[&e.target()];
        if a == b {
            continue;
        }
        *adj[a].entry(b).or_insert(0.0) += 1.0;
        *adj[b].entry(a).or_insert(0.0) += 1.0;
    }

    let labels = louvain(adj, resolution);

    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (i, &lab) in labels.iter().enumerate() {
        groups
            .entry(lab)
            .or_default()
            .push(graph.graph[nodes[i]].anchor().to_string());
    }
    let mut out: Vec<Vec<String>> = groups
        .into_values()
        .map(|mut v| {
            v.sort();
            v
        })
        .collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    out
}

/// Multi-level Louvain. Returns a community label per original node (0..n).
fn louvain(adj0: Vec<BTreeMap<usize, f64>>, resolution: f64) -> Vec<usize> {
    let n0 = adj0.len();
    // super_of[orig] = the current super-node an original node belongs to.
    let mut super_of: Vec<usize> = (0..n0).collect();
    let mut adj = adj0;

    loop {
        let comm = one_level(&adj, resolution);
        // Renumber the resulting communities to a dense 0..k.
        let mut renum: BTreeMap<usize, usize> = BTreeMap::new();
        let mut k = 0usize;
        let dense: Vec<usize> = comm
            .iter()
            .map(|&c| {
                *renum.entry(c).or_insert_with(|| {
                    let v = k;
                    k += 1;
                    v
                })
            })
            .collect();

        // Push the level's grouping down to original nodes.
        for s in super_of.iter_mut() {
            *s = dense[*s];
        }

        // Converged: this level merged nothing.
        if k == adj.len() {
            break;
        }
        adj = aggregate(&adj, &dense, k);
    }
    super_of
}

/// One pass of local moving to modularity convergence on the current graph.
/// Returns a community id per current node.
fn one_level(adj: &[BTreeMap<usize, f64>], resolution: f64) -> Vec<usize> {
    let n = adj.len();
    // Degree includes self-loop weight twice (undirected convention); level-0 has
    // no self-loops, aggregated levels may.
    let degree: Vec<f64> = (0..n)
        .map(|i| {
            adj[i].iter().map(|(&j, &w)| if j == i { 2.0 * w } else { w }).sum()
        })
        .collect();
    let total_w: f64 = degree.iter().sum::<f64>() / 2.0; // m
    if total_w == 0.0 {
        return (0..n).collect(); // no edges: every node its own community
    }
    let two_m = 2.0 * total_w;

    let mut community: Vec<usize> = (0..n).collect();
    let mut comm_tot: Vec<f64> = degree.clone(); // Σtot per community (seeded singletons)

    let mut improved = true;
    while improved {
        improved = false;
        for i in 0..n {
            let ci = community[i];
            // Remove i from its community.
            comm_tot[ci] -= degree[i];
            community[i] = usize::MAX;

            // Weight from i to each candidate community (deterministic order).
            let mut k_i_in: BTreeMap<usize, f64> = BTreeMap::new();
            for (&j, &w) in &adj[i] {
                if j == i {
                    continue;
                }
                *k_i_in.entry(community[j]).or_insert(0.0) += w;
            }

            // Pick the community maximizing modularity gain; staying (ci) is a
            // candidate via k_i_in (0 if no tie). Tie-break: lowest community id,
            // with a bias to remain in ci to avoid churn.
            let mut best = ci;
            let mut best_gain = k_i_in.get(&ci).copied().unwrap_or(0.0)
                - resolution * comm_tot[ci] * degree[i] / two_m;
            for (&c, &kin) in &k_i_in {
                let gain = kin - resolution * comm_tot[c] * degree[i] / two_m;
                if gain > best_gain + 1e-12 {
                    best_gain = gain;
                    best = c;
                }
            }

            community[i] = best;
            comm_tot[best] += degree[i];
            if best != ci {
                improved = true;
            }
        }
    }
    community
}

/// Build the aggregated graph: one super-node per community, edge weights summed
/// (intra-community edges become a self-loop carrying the internal weight).
fn aggregate(adj: &[BTreeMap<usize, f64>], comm: &[usize], k: usize) -> Vec<BTreeMap<usize, f64>> {
    let mut out: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); k];
    for (i, row) in adj.iter().enumerate() {
        let ci = comm[i];
        for (&j, &w) in row {
            // Each undirected edge is stored twice (i->j and j->i); summing both
            // directions into the super-edge keeps the symmetric representation,
            // and an intra-community edge lands on the (ci,ci) self-loop.
            *out[ci].entry(comm[j]).or_insert(0.0) += w;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run louvain directly on a hand-built adjacency (anchors not needed).
    fn run(edges: &[(usize, usize)], n: usize) -> Vec<usize> {
        let mut adj: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
        for &(a, b) in edges {
            *adj[a].entry(b).or_insert(0.0) += 1.0;
            *adj[b].entry(a).or_insert(0.0) += 1.0;
        }
        louvain(adj, 1.0)
    }

    /// Group node ids by their community label.
    fn groups(labels: &[usize]) -> Vec<Vec<usize>> {
        let mut by: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, &l) in labels.iter().enumerate() {
            by.entry(l).or_default().push(i);
        }
        by.into_values().collect()
    }

    #[test]
    fn two_cliques_joined_by_one_edge_split_into_two() {
        // Clique A {0,1,2}, clique B {3,4,5}, one bridge edge 2-3.
        let edges = [
            (0, 1), (0, 2), (1, 2), // A
            (3, 4), (3, 5), (4, 5), // B
            (2, 3), // bridge
        ];
        let g = groups(&run(&edges, 6));
        assert_eq!(g.len(), 2, "expected two communities, got {g:?}");
        // Each community is one clique (order within may vary).
        assert!(g.iter().any(|c| c == &[0, 1, 2]));
        assert!(g.iter().any(|c| c == &[3, 4, 5]));
    }

    #[test]
    fn single_clique_is_one_community() {
        let edges = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
        let g = groups(&run(&edges, 4));
        assert_eq!(g.len(), 1);
        assert_eq!(g[0], vec![0, 1, 2, 3]);
    }

    #[test]
    fn isolated_nodes_are_their_own_communities() {
        let g = groups(&run(&[], 3));
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn deterministic_across_runs() {
        let edges = [(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3), (2, 3)];
        assert_eq!(run(&edges, 6), run(&edges, 6));
    }
}
