//! Two jobs: check the model against real ZVV prices, and measure how the
//! Pareto frontier actually grows on this network.
//!
//! A fixture mismatch is a bug in the graph or the zone assignment. Never adjust
//! the price table to make a test pass.

use crate::fares::{Class, FareTable, Reduction};
use crate::graph::{Graph, GraphJson};
use crate::search::{self, LABEL_CAP};
use crate::stations::Station;
use crate::zoneset::ZoneIndex;
use anyhow::{Context, Result};
use std::path::Path;

/// A real ZVV fare, looked up on zvv.ch.
pub struct Fixture {
    pub from: &'static str,
    pub to: &'static str,
    /// Expected number of billed zones (the tariff's zone count).
    pub zones: u32,
    /// Expected adult 2nd-class single-ticket price in CHF.
    pub chf: f64,
    pub note: &'static str,
}

/// Station names are matched against GTFS `stop_name`.
///
/// Expected values are derived from the official ZVV Tarifzonenplan (the same
/// zone polygons this build uses) together with ZVV's published rule that a
/// ticket must cover every zone the journey *passes through*, then priced from
/// the table in `data/fares.toml`. Each fixture records the zone chain so the
/// derivation is auditable rather than taken on trust.
///
/// These are deliberately derived independently of the search, not read back
/// out of it. Spot-check them in the ZVV app before shipping.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        from: "Zürich HB",
        to: "Zürich Oerlikon",
        zones: 2,
        chf: 4.70,
        note: "{110}: both stops inside the city zone, which counts double — \
               Tarifstufe 2, not 1",
    },
    Fixture {
        from: "Zürich HB",
        to: "Winterthur",
        zones: 6,
        chf: 13.60,
        note: "{110 x2, 121, 122, 120 x2}: the non-stop IC still crosses \
               Wallisellen/Effretikon territory and must pay for it",
    },
    Fixture {
        from: "Zürich HB",
        to: "Uster",
        zones: 5,
        chf: 11.40,
        note: "{110 x2, 121, 130, 131}: the line runs through zone 130 at \
               Nänikon-Greifensee even on trains that do not stop there",
    },
    Fixture {
        from: "Zürich HB",
        to: "Dietikon",
        zones: 3,
        chf: 7.20,
        note: "{110 x2, 154}: Altstetten is still 110 and Schlieren already 154, \
               so nothing lies between",
    },
    Fixture {
        from: "Winterthur",
        to: "Uster",
        zones: 6,
        chf: 13.60,
        note: "{120 x2, 122, 121, 130, 131} via Effretikon, or {120 x2, 122, \
               135, 132, 131} via Pfäffikon — both count 6",
    },
    Fixture {
        from: "Wetzikon ZH",
        to: "Rapperswil SG",
        zones: 4,
        chf: 9.40,
        note: "{132, 133, 134, 180}: zone 133 at Bubikon is crossed even by \
               trains that run through it",
    },
];

struct Loaded {
    stations: Vec<Station>,
    graph: Graph,
    zones: ZoneIndex,
}

fn load(stations_path: &Path, graph_path: &Path) -> Result<Loaded> {
    let stations: Vec<Station> = serde_json::from_slice(
        &std::fs::read(stations_path)
            .with_context(|| format!("reading {} — run build-graph first", stations_path.display()))?,
    )?;
    let gj: GraphJson = serde_json::from_slice(&std::fs::read(graph_path)?)?;

    let zones = ZoneIndex::new(gj.zone_numbers.clone())?;
    let mut edges = Vec::new();
    for (u, ns) in gj.adjacency.iter().enumerate() {
        for (i, &v) in ns.iter().enumerate() {
            if (u as u32) < v {
                let mask =
                    gj.edge_mask_lo[u][i] as u64 | ((gj.edge_mask_hi[u][i] as u64) << 32);
                edges.push((u as u32, v, mask));
            }
        }
    }
    let graph = Graph::from_edges(gj.zone_bits.clone(), &edges);
    Ok(Loaded { stations, graph, zones })
}

/// Find a station by exact name, else by unique prefix.
fn find_station(stations: &[Station], name: &str) -> Result<u32> {
    if let Some(i) = stations.iter().position(|s| s.name == name) {
        return Ok(i as u32);
    }
    let matches: Vec<usize> = stations
        .iter()
        .enumerate()
        .filter(|(_, s)| s.name.starts_with(name))
        .map(|(i, _)| i)
        .collect();
    match matches.len() {
        0 => anyhow::bail!("no station named {name:?}"),
        1 => Ok(matches[0] as u32),
        _ => {
            let names: Vec<&str> = matches
                .iter()
                .take(6)
                .map(|&i| stations[i].name.as_str())
                .collect();
            anyhow::bail!("station {name:?} is ambiguous: {names:?}")
        }
    }
}

pub fn run(
    stations_path: &Path,
    graph_path: &Path,
    fares_path: &Path,
    sample: usize,
    cap: usize,
    dump: Option<&Path>,
) -> Result<()> {
    let l = load(stations_path, graph_path)?;
    let table = FareTable::load(fares_path)?;
    tracing::info!(
        stations = l.stations.len(),
        edges = l.graph.edge_count(),
        zones = l.zones.len(),
        "loaded model"
    );

    let ok = check_fixtures(&l, &table)?;
    report_label_growth(&l, sample, cap);
    if let Some(path) = dump {
        write_dump(&l, sample, path)?;
    }

    anyhow::ensure!(ok, "fare fixtures failed — the graph or zone assignment is wrong");
    Ok(())
}

fn check_fixtures(l: &Loaded, table: &FareTable) -> Result<bool> {
    println!("\n=== Fare fixtures (adult, 2nd class) ===");
    let mut all_ok = true;

    for f in FIXTURES {
        let from = match find_station(&l.stations, f.from) {
            Ok(i) => i,
            Err(e) => {
                println!("  SKIP {} -> {}: {e}", f.from, f.to);
                all_ok = false;
                continue;
            }
        };
        let to = match find_station(&l.stations, f.to) {
            Ok(i) => i,
            Err(e) => {
                println!("  SKIP {} -> {}: {e}", f.from, f.to);
                all_ok = false;
                continue;
            }
        };

        let res = search::search(&l.graph, &l.zones, from);
        match res.weights[to as usize] {
            None => {
                println!("  FAIL {} -> {}: unreachable", f.from, f.to);
                all_ok = false;
            }
            Some(w) => {
                let price = table
                    .price(w, Class::Second, Reduction::Full)
                    .context("no price for that zone weight")?;
                let good = w == f.zones && (price - f.chf).abs() < 1e-9;
                if !good {
                    all_ok = false;
                }
                let set = res.best_sets[to as usize].expect("reachable label has a set");
                let listed: Vec<u32> = set.iter().map(|b| l.zones.number_at(b)).collect();
                println!(
                    "  {} {} -> {}: {} zones / CHF {:.2}   (expected {} / CHF {:.2})  via {:?}",
                    if good { "PASS" } else { "FAIL" },
                    f.from,
                    f.to,
                    w,
                    price,
                    f.zones,
                    f.chf,
                    listed
                );
                if !good {
                    println!("        note: {}", f.note);
                }
            }
        }
    }
    Ok(all_ok)
}

fn report_label_growth(l: &Loaded, sample: usize, cap: usize) {
    let n = l.graph.node_count();
    let step = (n / sample.max(1)).max(1);
    let origins: Vec<u32> = (0..n as u32).step_by(step).collect();

    println!("\n=== Label growth over {} origins ===", origins.len());
    let mut max_labels = 0usize;
    let mut sum = 0f64;
    let mut runs = 0f64;
    let mut cap_hits = 0u64;
    let mut worst_origin = 0u32;

    // Uncapped statistics, and whether the cap ever changes an answer.
    let mut max_uncapped = 0usize;
    let mut sum_uncapped = 0f64;
    let mut differing_origins = 0u64;
    let mut differing_pairs = 0u64;
    let mut worst_overcharge = 0u32;

    for &o in &origins {
        let r = search::search_capped(&l.graph, &l.zones, o, cap);
        if r.max_labels() > max_labels {
            max_labels = r.max_labels();
            worst_origin = o;
        }
        sum += r.mean_labels();
        cap_hits += r.cap_hits;

        let exact = search::search_capped(&l.graph, &l.zones, o, usize::MAX);
        max_uncapped = max_uncapped.max(exact.max_labels());
        sum_uncapped += exact.mean_labels();

        let mut differs = false;
        for (capped, truth) in r.weights.iter().zip(exact.weights.iter()) {
            if capped != truth {
                differs = true;
                differing_pairs += 1;
                if let (Some(c), Some(t)) = (capped, truth) {
                    worst_overcharge = worst_overcharge.max(c.saturating_sub(*t));
                }
            }
        }
        if differs {
            differing_origins += 1;
        }
        runs += 1.0;
    }

    let total_pairs = runs * n as f64;
    println!("  cap under test        = {cap} (shipped LABEL_CAP = {LABEL_CAP})");
    println!(
        "  capped:   max {max_labels} / mean {:.2} labels per station (worst origin: {})",
        sum / runs,
        l.stations[worst_origin as usize].name
    );
    println!("  uncapped: max {max_uncapped} / mean {:.2} labels per station", sum_uncapped / runs);
    println!("  cap hits              = {cap_hits}");
    println!(
        "  origins where the cap changed an answer = {differing_origins} of {}",
        origins.len()
    );
    println!(
        "  station pairs priced differently        = {differing_pairs} of {:.0} ({:.4}%)",
        total_pairs,
        100.0 * differing_pairs as f64 / total_pairs
    );
    if differing_pairs == 0 {
        println!("  => the cap never changes a fare: results are EXACT on this network.");
    } else {
        println!("  => worst overcharge = {worst_overcharge} zone(s). Results are upper bounds.");
    }
}

/// Dump `origin -> [min zone weight per station]` so the JS port can be diffed
/// against the Rust results over every station pair, not just the fixtures.
fn write_dump(l: &Loaded, sample: usize, path: &Path) -> Result<()> {
    let n = l.graph.node_count();
    let step = (n / sample.max(1)).max(1);
    let mut out = serde_json::Map::new();
    for o in (0..n as u32).step_by(step) {
        let r = search::search(&l.graph, &l.zones, o);
        let ws: Vec<i64> = r.weights.iter().map(|w| w.map_or(-1, |x| x as i64)).collect();
        out.insert(o.to_string(), serde_json::to_value(ws)?);
    }
    std::fs::write(path, serde_json::to_vec(&out)?)?;
    println!("\n  wrote Rust reference for {} origins to {}", out.len(), path.display());
    Ok(())
}
