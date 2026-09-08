//! `isodapane-build` — offline build pipeline for isodapane.ch.
//!
//! Each subcommand is idempotent and caches aggressively: a rerun should not
//! re-hit the network. Full cold start:
//!
//! ```text
//! cargo run -p isodapane-build -- fetch-zones
//! cargo run -p isodapane-build -- fetch-gtfs
//! cargo run -p isodapane-build -- build-zones
//! cargo run -p isodapane-build -- build-graph
//! cargo run -p isodapane-build -- build-fares
//! cargo run -p isodapane-build -- validate
//! ```

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use isodapane_build::{fares, graph, national, search, stations, validate, wfs, zones, zoneset};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "isodapane-build", about = "Build the static data for isodapane.ch")]
struct Cli {
    /// Repository root; all other paths are relative to it.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download fare-zone polygons from the Kanton Zürich WFS.
    FetchZones {
        #[arg(long)]
        force: bool,
    },
    /// Download the Swiss GTFS static feed (~240 MB).
    FetchGtfs {
        #[arg(long)]
        force: bool,
    },
    /// Dissolve, simplify and emit web/data/zones.geojson.
    BuildZones,
    /// Build the station graph; emits stations.json and graph.json.
    BuildGraph,
    /// Emit web/data/fares.json from data/fares.toml.
    BuildFares,
    /// Phase 2: download the OSDM offline file (national fares).
    FetchNational {
        #[arg(long)]
        force: bool,
    },
    /// Phase 2: parse OSDM into data/national/.
    BuildNational,
    /// Phase 2: build the national rail-station graph from GTFS.
    BuildNationalGraph,
    /// Phase 2: fit tariff kilometres and report the price error.
    CalibrateNational,
    /// Phase 2: emit web/data/national.json.
    BuildNationalWeb,
    /// Phase 2: download SBB's Tarifverbundkarte.
    FetchFareUnions {
        #[arg(long)]
        force: bool,
    },
    /// Phase 2: extract the regional fare-union registry.
    BuildFareUnions,
    /// Check fare fixtures and report label-growth statistics.
    Validate {
        /// Origins sampled for the label-growth report.
        #[arg(long, default_value_t = 300)]
        sample: usize,
        /// Frontier cap to exercise; defaults to the shipped LABEL_CAP.
        #[arg(long)]
        cap: Option<usize>,
        /// Write per-origin results here so the JS port can be diffed against
        /// them (acceptance criterion 7).
        #[arg(long)]
        dump: Option<PathBuf>,
    },
}

struct Paths {
    raw_zones: PathBuf,
    gtfs: PathBuf,
    fares_toml: PathBuf,
    osdm: PathBuf,
    fare_unions: PathBuf,
    national_dir: PathBuf,
    web_zones: PathBuf,
    web_stations: PathBuf,
    web_graph: PathBuf,
    web_fares: PathBuf,
}

impl Paths {
    fn new(root: &Path) -> Self {
        Self {
            raw_zones: root.join("data/raw/tarifzonen.geojson"),
            gtfs: root.join("data/raw/gtfs.zip"),
            fares_toml: root.join("data/fares.toml"),
            osdm: root.join("data/raw/osdm.zip"),
            fare_unions: root.join("data/raw/tarifverbundkarte.geojson"),
            national_dir: root.join("data/national"),
            web_zones: root.join("web/data/zones.geojson"),
            web_stations: root.join("web/data/stations.json"),
            web_graph: root.join("web/data/graph.json"),
            web_fares: root.join("web/data/fares.json"),
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let paths = Paths::new(&cli.root);

    match cli.command {
        Command::FetchZones { force } => wfs::fetch_zones(&paths.raw_zones, force)?,
        Command::FetchGtfs { force } => wfs::fetch_gtfs(&paths.gtfs, force)?,
        Command::BuildZones => build_zones(&paths)?,
        Command::BuildGraph => build_graph(&paths)?,
        Command::BuildFares => build_fares(&paths)?,
        Command::FetchNational { force } => wfs::fetch_osdm(&paths.osdm, force)?,
        Command::BuildNational => national::build(&paths.osdm, &paths.national_dir)?,
        Command::BuildNationalGraph => national::build_graph(&paths.gtfs, &paths.national_dir)?,
        Command::CalibrateNational => national::calibrate(&paths.national_dir)?,
        Command::FetchFareUnions { force } => {
            national::fetch_fare_unions(&paths.fare_unions, force)?
        }
        Command::BuildFareUnions => {
            national::build_fare_unions(&paths.fare_unions, &paths.national_dir)?
        }
        Command::BuildNationalWeb => {
            national::build_web(&paths.national_dir, &paths.web_stations.parent().unwrap())?
        }
        Command::Validate { sample, cap, dump } => validate::run(
            &paths.web_stations,
            &paths.web_graph,
            &paths.fares_toml,
            sample,
            cap.unwrap_or(search::LABEL_CAP),
            dump.as_deref(),
        )?,
    }
    Ok(())
}

fn build_zones(paths: &Paths) -> Result<()> {
    let mut zs = zones::load_and_dissolve(&paths.raw_zones)?;
    tracing::info!(zones = zs.len(), "dissolved fare zones");
    zones::simplify(&mut zs);
    zones::write_geojson(&zs, &paths.web_zones)?;
    let size = std::fs::metadata(&paths.web_zones)?.len();
    tracing::info!(path = %paths.web_zones.display(), size, "wrote zone geometry");
    Ok(())
}

fn build_graph(paths: &Paths) -> Result<()> {
    let zs = zones::load_and_dissolve(&paths.raw_zones)?;
    let zone_numbers: Vec<u32> = zs.iter().map(|z| z.number).collect();
    let zone_index = zoneset::ZoneIndex::new(zone_numbers)?;
    tracing::info!(zones = zone_index.len(), "fare zones loaded");

    let (raw_stops, index_of) = stations::read_stops(&paths.gtfs)?;

    // Point-in-polygon every stop; anything outside every zone is out of scope.
    let locator = stations::ZoneLocator::new(&zs);
    let mut zone_of_station: std::collections::HashMap<usize, u32> =
        std::collections::HashMap::new();
    for (i, s) in raw_stops.iter().enumerate() {
        if let Some(z) = locator.locate(s.lon, s.lat) {
            zone_of_station.insert(i, z);
        }
    }
    tracing::info!(
        inside = zone_of_station.len(),
        total = raw_stops.len(),
        "stops inside a ZVV fare zone"
    );
    anyhow::ensure!(!zone_of_station.is_empty(), "no stops fell inside any fare zone");

    let edges_raw = stations::stream_edges(&paths.gtfs, &index_of, &zone_of_station)?;

    // Compact to a dense index over the kept stations only.
    let mut kept: Vec<usize> = zone_of_station.keys().copied().collect();
    kept.sort_unstable();
    let dense: std::collections::HashMap<usize, u32> = kept
        .iter()
        .enumerate()
        .map(|(new, &old)| (old, new as u32))
        .collect();

    let stations_out: Vec<stations::Station> = kept
        .iter()
        .map(|&old| {
            let s = &raw_stops[old];
            stations::Station {
                id: s.id.clone(),
                name: s.name.clone(),
                lon: (s.lon * 1e6).round() / 1e6,
                lat: (s.lat * 1e6).round() / 1e6,
                zone_id: zone_of_station[&old],
            }
        })
        .collect();

    let node_zone_bits: Vec<u8> = stations_out
        .iter()
        .map(|s| {
            zone_index
                .index_of(s.zone_id)
                .context("station zone missing from the zone index")
        })
        .collect::<Result<_>>()?;

    // Each edge carries the zones its vehicle crosses, not just its endpoints'.
    let zone_bit = |z: u32| zone_index.index_of(z);
    let edges: Vec<(u32, u32, u64)> = edges_raw
        .iter()
        .filter_map(|&(a, b)| {
            let (u, v) = (*dense.get(&(a as usize))?, *dense.get(&(b as usize))?);
            let (su, sv) = (&stations_out[u as usize], &stations_out[v as usize]);
            let mask = stations::traversed_zones(
                &locator,
                &zone_bit,
                (su.lon, su.lat),
                (sv.lon, sv.lat),
            );
            Some((u, v, mask))
        })
        .collect();

    // Express edges whose straight-line zone estimate is unreliable, but which a
    // parallel local path already covers, are removed so the search follows the
    // local alignment and picks up the zones the track really crosses.
    let coords: Vec<(f64, f64)> = stations_out.iter().map(|s| (s.lon, s.lat)).collect();
    let (edges, shortcuts) = graph::drop_shortcut_edges(&coords, &edges, 1.6, 12);
    tracing::info!(shortcuts, remaining = edges.len(), "dropped express shortcut edges");

    let g = graph::Graph::from_edges(node_zone_bits, &edges);
    tracing::info!(nodes = g.node_count(), edges = g.edge_count(), "built station graph");

    report_connectivity(&g, &stations_out);

    std::fs::create_dir_all(paths.web_stations.parent().unwrap())?;
    std::fs::write(&paths.web_stations, serde_json::to_vec(&stations_out)?)?;
    let gj = graph::GraphJson::from_graph(&g, &zone_index);
    std::fs::write(&paths.web_graph, serde_json::to_vec(&gj)?)?;
    tracing::info!("wrote stations.json and graph.json");
    Ok(())
}

/// The graph should be connected. If it is not, log the components with their
/// sizes and largest member stations — that is a data problem worth seeing, not
/// something to silently paper over.
fn report_connectivity(g: &graph::Graph, stations: &[stations::Station]) {
    let comps = g.components();
    if comps.len() == 1 {
        tracing::info!("station graph is connected");
        return;
    }
    let largest = comps[0].len();
    tracing::warn!(
        components = comps.len(),
        largest,
        stranded = g.node_count() - largest,
        "station graph is NOT connected"
    );
    for (i, comp) in comps.iter().enumerate().take(15) {
        let names: Vec<&str> = comp
            .iter()
            .take(4)
            .map(|&n| stations[n as usize].name.as_str())
            .collect();
        tracing::warn!(component = i, size = comp.len(), members = ?names, "component");
    }
    if comps.len() > 15 {
        tracing::warn!(remaining = comps.len() - 15, "further components not listed");
    }
}

fn build_fares(paths: &Paths) -> Result<()> {
    let table = fares::FareTable::load(&paths.fares_toml)?;
    let zs = zones::load_and_dissolve(&paths.raw_zones)?;
    let zone_index = zoneset::ZoneIndex::new(zs.iter().map(|z| z.number).collect())?;

    let out = serde_json::json!({
        "valid_from": table.valid_from,
        "source": table.source,
        "stufen": table.stufen,
        "zone_numbers": zone_index.numbers(),
        "zone_weights": zone_index.numbers().iter()
            .map(|&n| zoneset::weight_for_zone(n)).collect::<Vec<_>>(),
        "double_weight_zones": zoneset::DOUBLE_WEIGHT_ZONES,
        // weight -> Tarifstufe, capped at the all-zones ticket.
        "max_stufe": 8,
    });
    std::fs::create_dir_all(paths.web_fares.parent().unwrap())?;
    std::fs::write(&paths.web_fares, serde_json::to_vec_pretty(&out)?)?;
    tracing::info!(path = %paths.web_fares.display(), "wrote fares.json");
    Ok(())
}
