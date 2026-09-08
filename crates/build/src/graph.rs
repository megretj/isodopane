//! The station graph: adjacency over station indices, plus connectivity checks.
//!
//! Each edge carries the set of fare zones a vehicle *traverses* between two
//! consecutive stops — not merely the zone of the arrival stop.
//!
//! That distinction is the whole ballgame. ZVV tells passengers to "determine
//! the number of zones you will pass through", so a ticket must cover every zone
//! the vehicle crosses, including zones where it does not stop. Charging only
//! for the arrival stop's zone undercharges every express service: the non-stop
//! IC from Zürich HB to Winterthur would bill 110 + 120 = 4 zones (CHF 9.40)
//! when the real ticket also needs zones 121 and 122, making it 6 (CHF 13.60).
//!
//! So traversing `u -> v` unions in the edge's whole zone set. Whether that
//! costs anything depends on what the label already holds, which is why the cost
//! still lives in the label rather than in a scalar edge weight.
//!
//! Using network topology rather than polygon adjacency is deliberate: you can
//! only travel where lines actually run, so the tariff rule that unconnected
//! zones cannot be combined holds automatically, with no adjacency heuristics
//! and no Verbindungsliste blocklist.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Graph {
    /// Zone bit-index per node.
    zones: Vec<u8>,
    /// CSR adjacency: `adj[offsets[u]..offsets[u+1]]` are u's neighbours.
    offsets: Vec<u32>,
    adj: Vec<u32>,
    /// Zones traversed by each adjacency entry, parallel to `adj`.
    masks: Vec<u64>,
}

impl Graph {
    /// Build from an undirected edge list of `(u, v, traversed_zone_mask)`.
    ///
    /// Edges are deduplicated and self-loops dropped. Where the same stop pair
    /// appears more than once the masks are unioned, since different services
    /// over the same pair may take different alignments.
    pub fn from_edges(zones: Vec<u8>, edges: &[(u32, u32, u64)]) -> Self {
        let n = zones.len();
        let mut degree = vec![0u32; n];
        let mut seen: HashMap<(u32, u32), usize> = HashMap::with_capacity(edges.len() * 2);

        let mut unique: Vec<(u32, u32, u64)> = Vec::with_capacity(edges.len());
        for &(a, b, mask) in edges {
            if a == b {
                continue;
            }
            let key = if a < b { (a, b) } else { (b, a) };
            // Both endpoints are always part of what the edge covers.
            let mask = mask | (1u64 << zones[a as usize]) | (1u64 << zones[b as usize]);
            match seen.get(&key) {
                Some(&i) => unique[i].2 |= mask,
                None => {
                    seen.insert(key, unique.len());
                    unique.push((key.0, key.1, mask));
                    degree[a as usize] += 1;
                    degree[b as usize] += 1;
                }
            }
        }

        let mut offsets = vec![0u32; n + 1];
        for i in 0..n {
            offsets[i + 1] = offsets[i] + degree[i];
        }
        let mut cursor = offsets.clone();
        let mut adj = vec![0u32; offsets[n] as usize];
        let mut masks = vec![0u64; offsets[n] as usize];
        for (a, b, mask) in unique {
            adj[cursor[a as usize] as usize] = b;
            masks[cursor[a as usize] as usize] = mask;
            cursor[a as usize] += 1;
            adj[cursor[b as usize] as usize] = a;
            masks[cursor[b as usize] as usize] = mask;
            cursor[b as usize] += 1;
        }

        Self { zones, offsets, adj, masks }
    }

    pub fn node_count(&self) -> usize {
        self.zones.len()
    }

    pub fn edge_count(&self) -> usize {
        self.adj.len() / 2
    }

    #[inline]
    pub fn zone_of(&self, node: u32) -> u8 {
        self.zones[node as usize]
    }

    #[inline]
    pub fn neighbours(&self, node: u32) -> &[u32] {
        let s = self.offsets[node as usize] as usize;
        let e = self.offsets[node as usize + 1] as usize;
        &self.adj[s..e]
    }

    /// Neighbours of `node` paired with the zones each edge traverses.
    #[inline]
    pub fn edges_from(&self, node: u32) -> impl Iterator<Item = (u32, u64)> + '_ {
        let s = self.offsets[node as usize] as usize;
        let e = self.offsets[node as usize + 1] as usize;
        self.adj[s..e].iter().copied().zip(self.masks[s..e].iter().copied())
    }

    /// Connected components as node-index lists, largest first.
    pub fn components(&self) -> Vec<Vec<u32>> {
        let n = self.node_count();
        let mut seen = vec![false; n];
        let mut out = Vec::new();
        for start in 0..n as u32 {
            if seen[start as usize] {
                continue;
            }
            let mut stack = vec![start];
            let mut comp = Vec::new();
            seen[start as usize] = true;
            while let Some(u) = stack.pop() {
                comp.push(u);
                for &v in self.neighbours(u) {
                    if !seen[v as usize] {
                        seen[v as usize] = true;
                        stack.push(v);
                    }
                }
            }
            out.push(comp);
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.len()));
        out
    }
}

/// Great-circle-ish distance in metres. Equirectangular is plenty over the few
/// kilometres between two consecutive stops.
pub fn chord_metres(a: (f64, f64), b: (f64, f64)) -> f64 {
    let mid_lat = ((a.1 + b.1) / 2.0).to_radians();
    let dx = (b.0 - a.0).to_radians() * 6_371_000.0 * mid_lat.cos();
    let dy = (b.1 - a.1).to_radians() * 6_371_000.0;
    (dx * dx + dy * dy).sqrt()
}

/// Drop express edges that a path of shorter edges already parallels.
///
/// An express train runs on the same tracks as the stopping service beside it,
/// so it crosses the same fare zones — but its `stop_times` entry jumps straight
/// from Zürich Stadelhofen to Uster, and the straight line between those two
/// stops misses zone 130 that the track actually runs through. Left in, such an
/// edge silently undercharges.
///
/// Where a path of shorter edges already joins the same pair without a big
/// detour, that path *is* the local service on the same alignment, so the
/// express edge adds nothing but its bad zone set. Removing it makes the search
/// take the local path and pick up the right zones. Edges with no such parallel
/// (rural buses with widely spaced stops) are genuinely load-bearing and stay,
/// keeping their straight-line zone estimate.
///
/// Returns the surviving edges and the number dropped.
pub fn drop_shortcut_edges(
    coords: &[(f64, f64)],
    edges: &[(u32, u32, u64)],
    detour: f64,
    max_hops: u32,
) -> (Vec<(u32, u32, u64)>, usize) {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let lens: Vec<f64> = edges
        .iter()
        .map(|&(u, v, _)| chord_metres(coords[u as usize], coords[v as usize]))
        .collect();

    // Longest first: a shortcut is judged against edges shorter than itself, so
    // removing a long one never invalidates a decision already made.
    let mut order: Vec<usize> = (0..edges.len()).collect();
    order.sort_by(|&a, &b| lens[b].partial_cmp(&lens[a]).unwrap());

    let n = coords.len();
    let mut adj: Vec<Vec<(u32, f64, usize)>> = vec![Vec::new(); n];
    for (i, &(u, v, _)) in edges.iter().enumerate() {
        adj[u as usize].push((v, lens[i], i));
        adj[v as usize].push((u, lens[i], i));
    }

    let mut dropped = vec![false; edges.len()];
    let mut best = vec![f64::INFINITY; n];
    let mut stamp = vec![0u32; n];
    let mut epoch = 0u32;

    for &e in &order {
        let (u, v, _) = edges[e];
        let limit = lens[e] * detour;
        epoch += 1;

        let mut heap = BinaryHeap::new();
        heap.push((Reverse(ordered(0.0)), 0u32, u));
        best[u as usize] = 0.0;
        stamp[u as usize] = epoch;
        let mut found = false;

        while let Some((Reverse(d), hops, x)) = heap.pop() {
            let d = d.0;
            if x == v {
                found = true;
                break;
            }
            if hops >= max_hops || d > limit {
                continue;
            }
            for &(y, w, ei) in &adj[x as usize] {
                // Only strictly shorter, still-present edges count as "local".
                if ei == e || dropped[ei] || w >= lens[e] {
                    continue;
                }
                let nd = d + w;
                if nd > limit {
                    continue;
                }
                if stamp[y as usize] != epoch || nd < best[y as usize] {
                    stamp[y as usize] = epoch;
                    best[y as usize] = nd;
                    heap.push((Reverse(ordered(nd)), hops + 1, y));
                }
            }
        }

        if found {
            dropped[e] = true;
        }
    }

    let count = dropped.iter().filter(|&&d| d).count();
    let kept = edges
        .iter()
        .enumerate()
        .filter(|(i, _)| !dropped[*i])
        .map(|(_, &e)| e)
        .collect();
    (kept, count)
}

/// f64 wrapper so distances can live in a `BinaryHeap`.
#[derive(PartialEq, PartialOrd)]
struct Ordf(f64);
impl Eq for Ordf {}
impl Ord for Ordf {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap_or(std::cmp::Ordering::Equal)
    }
}
fn ordered(v: f64) -> Ordf {
    Ordf(v)
}

/// Serialised form written to `web/data/graph.json`.
///
/// Edge masks are split into two 32-bit halves because JavaScript's bitwise
/// operators coerce to int32: a 45-bit mask survives a JSON round trip as a
/// float, but `|` on it would silently truncate. Two halves keep the browser
/// port using plain fast integer ops instead of BigInt.
#[derive(Debug, Serialize, Deserialize)]
pub struct GraphJson {
    /// `adjacency[i]` lists neighbours of station `i` as station indices.
    pub adjacency: Vec<Vec<u32>>,
    /// Low 32 bits of each edge's traversed-zone mask, parallel to `adjacency`.
    pub edge_mask_lo: Vec<Vec<u32>>,
    /// High 32 bits of each edge's traversed-zone mask, parallel to `adjacency`.
    pub edge_mask_hi: Vec<Vec<u32>>,
    /// Zone bit-index per station, parallel to `stations.json`.
    pub zone_bits: Vec<u8>,
    /// Zone numbers ordered by bit index; bit `i` means `zone_numbers[i]`.
    pub zone_numbers: Vec<u32>,
    /// Tariff weight per bit index (2 for zones 110 and 120, else 1).
    pub zone_weights: Vec<u32>,
}

impl GraphJson {
    pub fn from_graph(graph: &Graph, zones: &crate::zoneset::ZoneIndex) -> Self {
        Self {
            adjacency: (0..graph.node_count() as u32)
                .map(|u| graph.neighbours(u).to_vec())
                .collect(),
            edge_mask_lo: (0..graph.node_count() as u32)
                .map(|u| graph.edges_from(u).map(|(_, m)| m as u32).collect())
                .collect(),
            edge_mask_hi: (0..graph.node_count() as u32)
                .map(|u| graph.edges_from(u).map(|(_, m)| (m >> 32) as u32).collect())
                .collect(),
            zone_bits: (0..graph.node_count() as u32).map(|u| graph.zone_of(u)).collect(),
            zone_numbers: zones.numbers().to_vec(),
            zone_weights: zones
                .numbers()
                .iter()
                .map(|&n| crate::zoneset::weight_for_zone(n))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_are_undirected_and_deduplicated() {
        let g = Graph::from_edges(vec![0, 0, 0], &[(0, 1, 0), (1, 0, 0), (0, 1, 0), (1, 2, 0)]);
        assert_eq!(g.edge_count(), 2);
        assert_eq!(g.neighbours(1).len(), 2);
        assert!(g.neighbours(0).contains(&1));
        assert!(g.neighbours(1).contains(&0));
    }

    #[test]
    fn self_loops_are_dropped() {
        let g = Graph::from_edges(vec![0, 0], &[(0, 0, 0), (0, 1, 0)]);
        assert_eq!(g.edge_count(), 1);
    }

    #[test]
    fn shortcut_edges_are_dropped_but_load_bearing_ones_survive() {
        // Four stops in a line, 1 km apart, plus an express 0 -> 3.
        let coords = vec![
            (8.0, 47.0),
            (8.0, 47.009),
            (8.0, 47.018),
            (8.0, 47.027),
            // An isolated stop reachable only by one long edge.
            (8.5, 47.5),
        ];
        let edges = vec![
            (0, 1, 0u64),
            (1, 2, 0),
            (2, 3, 0),
            (0, 3, 0), // express, paralleled by the locals
            (3, 4, 0), // long, but the only way to reach node 4
        ];
        let (kept, dropped) = drop_shortcut_edges(&coords, &edges, 1.6, 12);
        assert_eq!(dropped, 1, "only the express should go");
        assert!(kept.contains(&(3, 4, 0)), "load-bearing long edge must survive");
        assert!(!kept.contains(&(0, 3, 0)), "express must be dropped");
    }

    #[test]
    fn components_are_found_largest_first() {
        // {0,1,2} and {3,4}
        let g = Graph::from_edges(vec![0; 5], &[(0, 1, 0), (1, 2, 0), (3, 4, 0)]);
        let comps = g.components();
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].len(), 3);
        assert_eq!(comps[1].len(), 2);
    }

    #[test]
    fn isolated_nodes_are_their_own_component() {
        let g = Graph::from_edges(vec![0; 3], &[(0, 1, 0)]);
        assert_eq!(g.components().len(), 2);
    }
}
