//! Multi-label shortest-path search over zone sets.
//!
//! ## Why not plain Dijkstra
//!
//! The tariff charges for the *set* of distinct zones a journey covers, each
//! counted once. That makes the cost of the remaining path depend on which zones
//! have already been paid for, so the label that looks cheapest on arrival at a
//! node is not always the one that extends best:
//!
//! Relaxing an edge unions in every zone that edge *traverses* (see `graph.rs`),
//! not just the arrival stop's zone.
//!
//! - `P1` reaches `v` with `{A,B,C}`, weight 3
//! - `P2` reaches `v` with `{A,D}`, weight 2
//!
//! Plain Dijkstra keeps `P2`. But if the rest of the route runs through B and C,
//! `P2` finishes at `{A,D,B,C}` = 4 while the discarded `P1` finishes at
//! `{A,B,C}` = 3. Collapsing to one label per node loses the optimum.
//!
//! So we keep a Pareto frontier per node and prune only genuine dominance:
//! `L1` dominates `L2` when `set(L1) ⊆ set(L2)`, since a subset can never cost
//! more now nor constrain the future more.

use crate::graph::Graph;
use crate::zoneset::{ZoneIndex, ZoneSet};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Maximum Pareto labels retained per station.
///
/// In the general case the minimum-label path problem is NP-hard and the
/// frontier can grow exponentially. On the ZVV network it does grow — the spec's
/// guess of one-to-three labels per station is not what this data does. Measured
/// over 460 origins and 1,269,600 station pairs (`validate --sample 400`):
///
/// | cap       | max labels | mean labels | fares differing from uncapped |
/// |-----------|-----------|-------------|-------------------------------|
/// | 16        | 16        | 10.73       | 15 of 1,269,600 (0.0012%)     |
/// | 32        | 32        | 18.61       | 0                             |
/// | 64 (ours) | 64        | 25.34       | 0                             |
/// | uncapped  | 131       | 27.17       | —                             |
///
/// So 16 is genuinely lossy — it overcharges a handful of pairs by one zone —
/// while 32 is already exact. We ship 64 for a 2x margin over the smallest
/// exact cap. Re-run `validate` after any graph change: a non-zero "priced
/// differently" count means fares have become upper bounds.
pub const LABEL_CAP: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    pub set: ZoneSet,
    pub hops: u32,
}

/// Heap entry ordered by `(zone_weight, hops)`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct QueueEntry {
    weight: u32,
    hops: u32,
    node: u32,
    set: ZoneSet,
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Only the key participates; node/set break ties arbitrarily but
        // deterministically so runs are reproducible.
        (self.weight, self.hops, self.node, self.set.0).cmp(&(
            other.weight,
            other.hops,
            other.node,
            other.set.0,
        ))
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Outcome of one origin's search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Minimum zone weight per station; `None` where unreachable.
    pub weights: Vec<Option<u32>>,
    /// The zone set achieving that minimum, for inspection and debugging.
    pub best_sets: Vec<Option<ZoneSet>>,
    /// Surviving label count per station, for the diagnostics in `validate`.
    pub label_counts: Vec<usize>,
    /// How many times `LABEL_CAP` forced a truncation during this run.
    pub cap_hits: u64,
}

impl SearchResult {
    pub fn max_labels(&self) -> usize {
        self.label_counts.iter().copied().max().unwrap_or(0)
    }

    pub fn mean_labels(&self) -> f64 {
        let reached: Vec<usize> = self
            .label_counts
            .iter()
            .copied()
            .filter(|&c| c > 0)
            .collect();
        if reached.is_empty() {
            return 0.0;
        }
        reached.iter().sum::<usize>() as f64 / reached.len() as f64
    }
}

/// Minimum zone weight from `source` to every station, using [`LABEL_CAP`].
pub fn search(graph: &Graph, zones: &ZoneIndex, source: u32) -> SearchResult {
    search_capped(graph, zones, source, LABEL_CAP)
}

/// As [`search`], with an explicit frontier cap. `usize::MAX` runs the search
/// uncapped, which is how `validate` measures whether the cap distorts results.
pub fn search_capped(
    graph: &Graph,
    zones: &ZoneIndex,
    source: u32,
    cap: usize,
) -> SearchResult {
    let n = graph.node_count();
    let mut labels: Vec<Vec<Label>> = vec![Vec::new(); n];
    let mut cap_hits = 0u64;

    let start_set = ZoneSet::single(graph.zone_of(source));
    let start = Label { set: start_set, hops: 1 };
    labels[source as usize].push(start);

    let mut heap = BinaryHeap::new();
    heap.push(Reverse(QueueEntry {
        weight: zones.weight(start_set),
        hops: 1,
        node: source,
        set: start_set,
    }));

    while let Some(Reverse(entry)) = heap.pop() {
        // Skip labels that were pruned from the frontier after being queued.
        if !labels[entry.node as usize]
            .iter()
            .any(|l| l.set == entry.set && l.hops == entry.hops)
        {
            continue;
        }

        for (v, traversed) in graph.edges_from(entry.node) {
            let cand = Label {
                set: entry.set.union(ZoneSet(traversed)),
                hops: entry.hops + 1,
            };

            let frontier = &mut labels[v as usize];

            // Dominated by something already there? Subset ⇒ no worse future.
            if frontier.iter().any(|l| l.set.is_subset_of(cand.set)) {
                continue;
            }
            // Drop whatever `cand` now dominates.
            frontier.retain(|l| !cand.set.is_subset_of(l.set));
            frontier.push(cand);

            if frontier.len() > cap {
                cap_hits += 1;
                frontier.sort_by_key(|l| (zones.weight(l.set), l.hops));
                frontier.truncate(cap);
                // If `cand` was the one truncated away, don't expand it.
                if !frontier.iter().any(|l| *l == cand) {
                    continue;
                }
            }

            heap.push(Reverse(QueueEntry {
                weight: zones.weight(cand.set),
                hops: cand.hops,
                node: v,
                set: cand.set,
            }));
        }
    }

    let best: Vec<Option<(u32, ZoneSet)>> = labels
        .iter()
        .map(|ls| ls.iter().map(|l| (zones.weight(l.set), l.set)).min_by_key(|&(w, _)| w))
        .collect();
    let weights = best.iter().map(|b| b.map(|(w, _)| w)).collect();
    let best_sets = best.iter().map(|b| b.map(|(_, s)| s)).collect();
    let label_counts = labels.iter().map(|ls| ls.len()).collect();

    SearchResult { weights, best_sets, label_counts, cap_hits }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;

    /// Build a test graph. Edges carry no extra traversed zones, so each edge
    /// covers exactly its two endpoints' zones — the behaviour the old
    /// stop-zone-only model had, which keeps these cases easy to reason about.
    fn build(zone_numbers: &[u32], edges: &[(u32, u32)]) -> (Graph, ZoneIndex) {
        let mut uniq: Vec<u32> = zone_numbers.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        let zi = ZoneIndex::new(uniq).unwrap();
        let node_zones: Vec<u8> = zone_numbers
            .iter()
            .map(|n| zi.index_of(*n).unwrap())
            .collect();
        let edges: Vec<(u32, u32, u64)> = edges.iter().map(|&(a, b)| (a, b, 0)).collect();
        (Graph::from_edges(node_zones, &edges), zi)
    }

    #[test]
    fn single_zone_trip_costs_one() {
        let (g, zi) = build(&[154, 154], &[(0, 1)]);
        let r = search(&g, &zi, 0);
        assert_eq!(r.weights[1], Some(1));
    }

    #[test]
    fn zurich_city_bills_as_two_zones() {
        // Both stations inside zone 110 — acceptance criterion 6.
        let (g, zi) = build(&[110, 110], &[(0, 1)]);
        let r = search(&g, &zi, 0);
        assert_eq!(r.weights[1], Some(2), "zone 110 must count double");
    }

    #[test]
    fn revisiting_a_zone_is_free() {
        // 154 -> 155 -> 154: the set is {154,155}, weight 2, not 3.
        let (g, zi) = build(&[154, 155, 154], &[(0, 1), (1, 2)]);
        let r = search(&g, &zi, 0);
        assert_eq!(r.weights[2], Some(2));
    }

    #[test]
    fn unreachable_nodes_have_no_weight() {
        let (g, zi) = build(&[154, 155], &[]);
        let r = search(&g, &zi, 0);
        assert_eq!(r.weights[1], None);
    }

    /// The case that breaks plain Dijkstra, from the module docs.
    ///
    /// Node 0 in A. Two routes to node 3: one through B,C, one through D.
    /// Then 3 -> 4 -> 5 passes through B and C. The path that looked worse at
    /// node 3 (already holding B and C) wins at node 5.
    #[test]
    fn locally_worse_label_can_win_globally() {
        //            1(B) - 2(C)
        //          /             \
        //  0(A)                    3(A) - 4(B) - 5(C)
        //          \             /
        //            6(D) ------
        let (g, zi) = build(
            &[100, 101, 102, 100, 101, 102, 103],
            &[(0, 1), (1, 2), (2, 3), (0, 6), (6, 3), (3, 4), (4, 5)],
        );
        let r = search(&g, &zi, 0);
        // Via B,C: set {A,B,C} = 3. Via D: {A,D} then +B +C = {A,D,B,C} = 4.
        assert_eq!(
            r.weights[5],
            Some(3),
            "must keep the locally-worse label that extends better"
        );
        // And node 3 really does hold both labels on its frontier.
        assert!(
            r.label_counts[3] >= 2,
            "expected a Pareto frontier at the junction, got {}",
            r.label_counts[3]
        );
    }

    /// An edge that crosses a zone without stopping in it must still be billed.
    #[test]
    fn traversed_zones_are_charged() {
        let zi = ZoneIndex::new(vec![100, 101, 102]).unwrap();
        let (a, mid, b) = (
            zi.index_of(100).unwrap(),
            zi.index_of(101).unwrap(),
            zi.index_of(102).unwrap(),
        );
        // One non-stop edge from a zone-100 stop to a zone-102 stop that
        // physically crosses zone 101 — the Zürich HB -> Winterthur case.
        let g = Graph::from_edges(vec![a, b], &[(0, 1, 1u64 << mid)]);
        let r = search(&g, &zi, 0);
        assert_eq!(r.weights[1], Some(3), "the crossed zone must be paid for");
    }

    #[test]
    fn cycles_terminate() {
        let (g, zi) = build(&[100, 101, 102], &[(0, 1), (1, 2), (2, 0)]);
        let r = search(&g, &zi, 0);
        assert_eq!(r.weights[1], Some(2));
        assert_eq!(r.weights[2], Some(2));
        assert_eq!(r.cap_hits, 0);
    }
}
