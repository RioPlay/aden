// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Personalized PageRank over the knowledge graph.
//!
//! Ranks nodes by graph-topology salience relative to a query: the
//! personalization (restart) vector is the per-anchor query relevance the search
//! layer already computes, and the walk spreads that relevance to structurally
//! central neighbors — surfacing context connected to the query region even when
//! its own text doesn't match the query (the "multi-hop in one hop" effect).
//! Feeds the `relevance` tie-breaker in `aden-asm`'s frontier ordering.
//!
//! Determinism (aden's guarantee): power iteration with fixed damping and a fixed
//! iteration cap, over nodes processed in *anchor-sorted* order (NOT raw
//! `NodeIndex` order, which follows non-deterministic HashMap-insertion — see
//! `community::detect_communities`) — so scores are reproducible run-to-run, no
//! RNG. No embeddings: the seed is supplied by the caller, so PPR itself works in
//! the lean (BM25-only) build.

use crate::graph::AdenGraph;
use crate::nodes::{GraphEdge, GraphNode};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

/// Probability of following an edge each step; `1 - DAMPING` teleports back to the
/// personalization seed. 0.85 is the PageRank standard.
const DAMPING: f32 = 0.85;
/// Power-iteration stop: first iteration whose total L1 change is below this.
const EPS: f32 = 1e-6;
/// Hard cap on iterations (convergence is geometric; ~50 suffices in practice).
const MAX_ITERS: usize = 100;

/// Personalized PageRank seeded by `seed` (anchor → relevance weight).
///
/// Returns anchor → score (scores sum to ~1). `seed` need not be normalized;
/// non-positive weights and anchors absent from the graph are ignored. An empty
/// or all-zero seed degrades to uniform PageRank (pure topology). A caller that
/// feeds this into frontier ordering degrades to the prior structural order
/// wherever PPR is flat.
pub fn personalized_pagerank<N: GraphNode, E: GraphEdge>(
    graph: &AdenGraph<N, E>,
    seed: &HashMap<String, f32>,
) -> HashMap<String, f32> {
    // Canonical (anchor-sorted) node order so accumulation is reproducible
    // run-to-run — `node_indices()` follows HashMap-insertion order.
    let mut nodes: Vec<NodeIndex> = graph.graph.node_indices().collect();
    nodes.sort_by(|&a, &b| graph.graph[a].anchor().cmp(graph.graph[b].anchor()));
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }
    let id_of: HashMap<NodeIndex, usize> =
        nodes.iter().enumerate().map(|(i, &ix)| (ix, i)).collect();

    // Out-adjacency in canonical id space; self-loops dropped, parallel edges kept
    // (a doubled tie carries proportionally more flow).
    let mut out_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in graph.graph.edge_references() {
        let (a, b) = (id_of[&e.source()], id_of[&e.target()]);
        if a != b {
            out_adj[a].push(b);
        }
    }

    // Personalization vector aligned to the canonical order.
    let pers: Vec<f32> = nodes
        .iter()
        .map(|&ix| {
            seed.get(graph.graph[ix].anchor())
                .copied()
                .filter(|w| *w > 0.0)
                .unwrap_or(0.0)
        })
        .collect();

    let ranks = pagerank(&out_adj, &pers);

    nodes
        .iter()
        .enumerate()
        .map(|(i, &ix)| (graph.graph[ix].anchor().to_string(), ranks[i]))
        .collect()
}

/// Deterministic power-iteration PageRank on integer out-adjacency, restarting to
/// `personalization` (normalized here; all-zero ⇒ uniform). Split out from the
/// graph wrapper so it is unit-testable on hand-built adjacency.
fn pagerank(out_adj: &[Vec<usize>], personalization: &[f32]) -> Vec<f32> {
    let n = out_adj.len();
    if n == 0 {
        return Vec::new();
    }

    let mut p = personalization.to_vec();
    let sum: f32 = p.iter().sum();
    if sum > 0.0 {
        p.iter_mut().for_each(|x| *x /= sum);
    } else {
        p.iter_mut().for_each(|x| *x = 1.0 / n as f32);
    }

    let outdeg: Vec<usize> = out_adj.iter().map(Vec::len).collect();
    let mut rank = p.clone();
    let mut next = vec![0f32; n];

    for _ in 0..MAX_ITERS {
        // Rank stranded on dangling (no-out) nodes is redistributed via teleport,
        // so total mass is conserved at 1 each step.
        let dangling: f32 = (0..n).filter(|&i| outdeg[i] == 0).map(|i| rank[i]).sum();
        next.iter_mut().for_each(|x| *x = 0.0);
        for i in 0..n {
            if outdeg[i] == 0 {
                continue;
            }
            let share = rank[i] / outdeg[i] as f32;
            for &j in &out_adj[i] {
                next[j] += share;
            }
        }
        let mut delta = 0f32;
        for i in 0..n {
            let v = (1.0 - DAMPING) * p[i] + DAMPING * (next[i] + dangling * p[i]);
            delta += (v - rank[i]).abs();
            next[i] = v;
        }
        std::mem::swap(&mut rank, &mut next);
        if delta < EPS {
            break;
        }
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn symmetric_ring_uniform_seed_is_equal_mass() {
        // 0->1->2->0; empty seed ⇒ uniform teleport.
        let adj = vec![vec![1], vec![2], vec![0]];
        let r = pagerank(&adj, &[0.0, 0.0, 0.0]);
        assert!(
            approx(r.iter().sum::<f32>(), 1.0),
            "sum={}",
            r.iter().sum::<f32>()
        );
        assert!(r.iter().all(|&x| approx(x, 1.0 / 3.0)), "{r:?}");
    }

    #[test]
    fn seed_on_source_ranks_downstream_descending() {
        // chain 0->1->2, all restart mass on node 0.
        let adj = vec![vec![1], vec![2], vec![]];
        let r = pagerank(&adj, &[1.0, 0.0, 0.0]);
        assert!(r[0] > r[1] && r[1] > r[2], "expected 0>1>2, got {r:?}");
    }

    #[test]
    fn seed_in_middle_excludes_upstream() {
        // chain 0->1->2, restart on node 1: node 1 and its downstream 2 score; the
        // upstream 0 (no path from 1, no restart) gets ~nothing.
        let adj = vec![vec![1], vec![2], vec![]];
        let r = pagerank(&adj, &[0.0, 1.0, 0.0]);
        assert!(r[1] > r[2], "1>2, got {r:?}");
        assert!(r[2] > r[0], "2>0, got {r:?}");
        assert!(approx(r[0], 0.0), "upstream should be ~0, got {}", r[0]);
    }

    #[test]
    fn deterministic_across_calls() {
        let adj = vec![vec![1, 2], vec![2], vec![0], vec![]];
        let p = [0.5, 0.0, 0.5, 0.0];
        assert_eq!(pagerank(&adj, &p), pagerank(&adj, &p));
    }

    #[test]
    fn all_zero_seed_is_finite_and_normalized() {
        let adj = vec![vec![1], vec![], vec![1]];
        let r = pagerank(&adj, &[0.0, 0.0, 0.0]);
        assert!(r.iter().all(|x| x.is_finite()), "{r:?}");
        assert!(approx(r.iter().sum::<f32>(), 1.0));
    }
}
