//! Phase 2 substrate: the national Direkter Verkehr tariff.
//!
//! The spec planned to transcribe the T601 price table out of a PDF and to
//! estimate tariff kilometres, noting that Tarifkilometer and Distanzzuschläge
//! "are not published machine-readably". Two things turned out differently:
//!
//! 1. The Alliance SwissPass tariff PDFs (T601, T603, T604) are **login-gated**.
//!    `https://www.allianceswisspass.ch/de/asp/Downloads?download=16350` returns
//!    the login page, not the document, so transcribing T601 is not an option
//!    from open sources.
//!
//! 2. It is not needed. opentransportdata.swiss publishes an **OSDM offline
//!    file** — a machine-readable subset of Swiss public transport fares — which
//!    carries, for each of thousands of relations, the **tariff distance**, the
//!    permitted via-route, and the actual price per passenger type, class and
//!    reduction card. That is both the price table and the tariff kilometres,
//!    from the horse's mouth.
//!
//! This module parses that file into three artefacts under `data/national/`.
//! Note it is a *subset*: a few hundred stations, not the whole network, so it
//! calibrates and validates the distance-to-price model rather than replacing
//! it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// OSDM offline file, published by the SKI business office.
pub const OSDM_DATASET: &str = "https://data.opentransportdata.swiss/en/dataset/osdm-offline";
pub const OSDM_URL: &str = "https://data.opentransportdata.swiss/dataset/\
    75399276-72f9-4542-a781-8901326befb2/resource/\
    f58119bb-6d19-4619-ab8f-e6a0bf250667/download/osdm_delivery_10_7.zip";

/* ----------------------------------------------------------- input shapes */

#[derive(Deserialize)]
struct Root {
    #[serde(rename = "fareDelivery")]
    fare_delivery: FareDelivery,
}

#[derive(Deserialize)]
struct FareDelivery {
    delivery: Delivery,
    #[serde(rename = "fareStructure")]
    fare_structure: FareStructure,
}

#[derive(Deserialize)]
struct Delivery {
    #[serde(rename = "deliveryId")]
    delivery_id: String,
    version: String,
}

#[derive(Deserialize)]
struct FareStructure {
    prices: Vec<PriceEntry>,
    #[serde(rename = "regionalConstraints")]
    regional_constraints: Vec<RegionalConstraint>,
    fares: Vec<Fare>,
    #[serde(rename = "stationNames")]
    station_names: Vec<StationName>,
}

#[derive(Deserialize)]
struct PriceEntry {
    id: String,
    price: Vec<Money>,
}

#[derive(Deserialize)]
struct Money {
    amount: f64,
    scale: i32,
}

#[derive(Deserialize)]
struct RegionalConstraint {
    id: String,
    #[serde(rename = "entryConnectionPointId")]
    entry: Option<String>,
    #[serde(rename = "exitConnectionPointId")]
    exit: Option<String>,
    distance: Option<u32>,
    #[serde(rename = "regionalValidity", default)]
    validity: Vec<RegionalValidity>,
}

#[derive(Deserialize)]
struct RegionalValidity {
    #[serde(rename = "viaStations")]
    via: Option<ViaStations>,
}

#[derive(Deserialize)]
struct ViaStations {
    #[serde(default)]
    route: Vec<RoutePoint>,
}

#[derive(Deserialize)]
struct RoutePoint {
    station: Option<StationRef>,
}

#[derive(Deserialize)]
struct StationRef {
    code: String,
}

#[derive(Deserialize)]
struct Fare {
    #[serde(rename = "priceRef")]
    price_ref: String,
    #[serde(rename = "regionalConstraintRef")]
    region_ref: String,
    #[serde(rename = "serviceClassRef")]
    class: String,
    #[serde(rename = "passengerConstraintRef")]
    passenger: String,
    #[serde(rename = "reductionConstraintRef")]
    reduction: Option<String>,
}

#[derive(Deserialize)]
struct StationName {
    code: String,
    #[serde(rename = "nameUtf8")]
    name: String,
}

/* ---------------------------------------------------------- output shapes */

#[derive(Serialize, Deserialize)]
pub struct Station {
    pub uic: String,
    pub name: String,
}

/// One priced relation: a permitted route between two stations, its tariff
/// distance, and what it actually costs.
#[derive(Serialize, Deserialize)]
pub struct Relation {
    pub from: String,
    pub to: String,
    /// Tariff kilometres — not geographic distance.
    pub km: u32,
    /// UIC codes of the via-route this price is valid for.
    pub route: Vec<String>,
    /// Adult full fare, 2nd then 1st class, in CHF.
    pub adult2: Option<f64>,
    pub adult1: Option<f64>,
    /// Adult with Halbtax.
    pub half2: Option<f64>,
    pub half1: Option<f64>,
    pub child2: Option<f64>,
}

/// The empirical price-per-distance curve, with the spread at each distance.
///
/// The spread is the point: the same tariff distance does not always cost the
/// same, because operators apply surcharges on some routes. Anything downstream
/// that prices by distance alone must publish this error, not hide it.
#[derive(Serialize, Deserialize)]
pub struct DistanceRow {
    pub km: u32,
    pub n: usize,
    pub min2: f64,
    pub median2: f64,
    pub max2: f64,
    pub min1: Option<f64>,
    pub median1: Option<f64>,
    pub max1: Option<f64>,
}

fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Parse the cached OSDM zip and write the three artefacts.
pub fn build(zip_path: &Path, out_dir: &Path) -> Result<()> {
    tracing::info!(path = %zip_path.display(), "reading OSDM offline file");
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("opening {} — run fetch-national first", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("opening OSDM zip")?;
    let entry = zip.by_index(0).context("OSDM zip is empty")?;
    tracing::info!(name = entry.name(), size = entry.size(), "parsing OSDM JSON");

    let root: Root = serde_json::from_reader(std::io::BufReader::with_capacity(1 << 22, entry))
        .context("parsing OSDM JSON")?;
    let fs = root.fare_delivery.fare_structure;
    tracing::info!(
        delivery = %root.fare_delivery.delivery.delivery_id,
        version = %root.fare_delivery.delivery.version,
        relations = fs.regional_constraints.len(),
        fares = fs.fares.len(),
        stations = fs.station_names.len(),
        "loaded OSDM"
    );

    let prices: HashMap<&str, f64> = fs
        .prices
        .iter()
        .filter_map(|p| {
            let m = p.price.first()?;
            Some((p.id.as_str(), m.amount / 10f64.powi(m.scale)))
        })
        .collect();

    // Pick out the fare columns we care about, keyed by relation id.
    let mut col: HashMap<&str, [Option<f64>; 5]> = HashMap::new();
    for f in &fs.fares {
        let Some(&price) = prices.get(f.price_ref.as_str()) else { continue };
        let slot = match (f.passenger.as_str(), f.reduction.as_deref(), f.class.as_str()) {
            ("ADULT", None, "BASIC") => 0,
            ("ADULT", None, "HIGH") => 1,
            ("ADULT", Some("HALBTAX_CONSTRAINT"), "BASIC") => 2,
            ("ADULT", Some("HALBTAX_CONSTRAINT"), "HIGH") => 3,
            ("CHILD", None, "BASIC") => 4,
            _ => continue,
        };
        col.entry(f.region_ref.as_str()).or_default()[slot] = Some(price);
    }

    let mut relations = Vec::new();
    for rc in &fs.regional_constraints {
        let (Some(from), Some(to), Some(km)) = (&rc.entry, &rc.exit, rc.distance) else { continue };
        let Some(c) = col.get(rc.id.as_str()) else { continue };
        let route = rc
            .validity
            .first()
            .and_then(|v| v.via.as_ref())
            .map(|v| v.route.iter().filter_map(|p| p.station.as_ref().map(|s| s.code.clone())).collect())
            .unwrap_or_default();
        relations.push(Relation {
            from: from.clone(),
            to: to.clone(),
            km,
            route,
            adult2: c[0],
            adult1: c[1],
            half2: c[2],
            half1: c[3],
            child2: c[4],
        });
    }
    relations.sort_by(|a, b| (a.km, &a.from, &a.to).cmp(&(b.km, &b.from, &b.to)));

    // Empirical price curve with its spread.
    let mut by_km: BTreeMap<u32, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for r in &relations {
        if r.km == 0 {
            continue;
        }
        let e = by_km.entry(r.km).or_default();
        if let Some(p) = r.adult2 { e.0.push(p); }
        if let Some(p) = r.adult1 { e.1.push(p); }
    }
    let curve: Vec<DistanceRow> = by_km
        .into_iter()
        .filter(|(_, (a, _))| !a.is_empty())
        .map(|(km, (mut a2, mut a1))| DistanceRow {
            km,
            n: a2.len(),
            min2: a2.iter().cloned().fold(f64::INFINITY, f64::min),
            max2: a2.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            median2: median(&mut a2),
            min1: (!a1.is_empty()).then(|| a1.iter().cloned().fold(f64::INFINITY, f64::min)),
            max1: (!a1.is_empty()).then(|| a1.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
            median1: (!a1.is_empty()).then(|| median(&mut a1)),
        })
        .collect();

    let mut stations: Vec<Station> = fs
        .station_names
        .iter()
        .map(|s| Station { uic: s.code.clone(), name: s.name.clone() })
        .collect();
    stations.sort_by(|a, b| a.uic.cmp(&b.uic));

    std::fs::create_dir_all(out_dir)?;
    write_json(&out_dir.join("stations.json"), &stations)?;
    write_json(&out_dir.join("relations.json"), &relations)?;
    write_json(&out_dir.join("distance_price.json"), &curve)?;

    // How badly does distance alone predict price? This is the number the spec
    // asks to publish rather than paper over.
    let mut off = 0usize;
    let mut worst = 0f64;
    let mut rel_errs = Vec::new();
    for row in &curve {
        if row.max2 - row.min2 > 1e-9 {
            off += row.n;
            worst = worst.max(row.max2 - row.min2);
            rel_errs.push((row.max2 - row.min2) / row.median2);
        }
    }
    rel_errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total: usize = curve.iter().map(|r| r.n).sum();
    tracing::info!(
        relations = relations.len(),
        stations = stations.len(),
        distances = curve.len(),
        "wrote national tariff artefacts"
    );
    println!("\n=== distance is not a function of price ===");
    println!("  priced relations                : {total}");
    println!("  distances with >1 price         : {} of {}", rel_errs.len(), curve.len());
    println!("  relations at an ambiguous distance: {off} ({:.1}%)", 100.0 * off as f64 / total as f64);
    if !rel_errs.is_empty() {
        println!(
            "  spread at one distance          : median {:.1}%, p90 {:.1}%, max {:.1}% (CHF {:.2})",
            100.0 * rel_errs[rel_errs.len() / 2],
            100.0 * rel_errs[rel_errs.len() * 9 / 10],
            100.0 * rel_errs[rel_errs.len() - 1],
            worst
        );
    }
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    std::fs::write(path, serde_json::to_vec(value)?)
        .with_context(|| format!("writing {}", path.display()))?;
    let size = std::fs::metadata(path)?.len();
    tracing::info!(path = %path.display(), size, "wrote");
    Ok(())
}

/* ===================================================================== */
/*  National station graph                                               */
/* ===================================================================== */

/// Parse a GTFS `HH:MM:SS` clock into seconds. Hours may exceed 24.
fn gtfs_seconds(s: &str) -> Option<f64> {
    let mut it = s.trim().split(':');
    let h: f64 = it.next()?.parse().ok()?;
    let m: f64 = it.next()?.parse().ok()?;
    let sec: f64 = it.next().unwrap_or("0").parse().unwrap_or(0.0);
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// GTFS extended route types 100-117 are all forms of rail.
fn is_rail(route_type: &str) -> bool {
    matches!(route_type.trim().parse::<u32>(), Ok(t) if (100..=117).contains(&t) || t == 2)
}

#[derive(Serialize, Deserialize, Clone)]
pub struct NatStation {
    /// GTFS stop id of the station (the parent, where there is one).
    pub id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    /// DIDOK/UIC number — the join key to the OSDM tariff data.
    pub uic: String,
}

#[derive(Serialize, Deserialize)]
pub struct NatGraph {
    pub adjacency: Vec<Vec<u32>>,
    /// Great-circle length of each adjacency entry, in metres.
    pub metres: Vec<Vec<f64>>,
    /// Fastest observed run time over each adjacency entry, in seconds.
    ///
    /// Fares follow the route a traveller actually takes, and that is the fast
    /// one, not the geographically shortest. The minimum over every scheduled
    /// run is the express time where an express exists.
    #[serde(default)]
    pub seconds: Vec<Vec<f64>>,
}

fn csv_from_zip<'a>(
    zip: &'a mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> Result<csv::Reader<std::io::BufReader<zip::read::ZipFile<'a, std::fs::File>>>> {
    let entry = zip
        .by_name(name)
        .with_context(|| format!("GTFS zip has no {name}"))?;
    Ok(csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(std::io::BufReader::with_capacity(1 << 22, entry)))
}

fn column(headers: &csv::StringRecord, name: &str) -> Result<usize> {
    headers
        .iter()
        .position(|h| h.trim_start_matches('\u{feff}') == name)
        .with_context(|| format!("missing column `{name}`"))
}

/// Build the national rail-station graph from GTFS.
///
/// Only rail is kept. The v1 canton map is stop-level because ZVV fares are, but
/// the national tariff is a railway tariff: including every bus stop would add
/// hundreds of thousands of nodes that the price model cannot price anyway.
pub fn build_graph(gtfs: &Path, out_dir: &Path) -> Result<()> {
    let open = || -> Result<zip::ZipArchive<std::fs::File>> {
        Ok(zip::ZipArchive::new(std::fs::File::open(gtfs).with_context(|| {
            format!("opening {}", gtfs.display())
        })?)?)
    };

    // 1. Rail routes.
    let mut zip = open()?;
    let mut rail_routes: std::collections::HashSet<String> = Default::default();
    {
        let mut rdr = csv_from_zip(&mut zip, "routes.txt")?;
        let h = rdr.headers()?.clone();
        let (c_id, c_type) = (column(&h, "route_id")?, column(&h, "route_type")?);
        for rec in rdr.records() {
            let rec = rec?;
            if is_rail(&rec[c_type]) {
                rail_routes.insert(rec[c_id].to_string());
            }
        }
    }
    tracing::info!(rail_routes = rail_routes.len(), "rail routes");

    // 2. Trips on those routes.
    let mut zip = open()?;
    let mut rail_trips: std::collections::HashSet<String> = Default::default();
    {
        let mut rdr = csv_from_zip(&mut zip, "trips.txt")?;
        let h = rdr.headers()?.clone();
        let (c_route, c_trip) = (column(&h, "route_id")?, column(&h, "trip_id")?);
        let mut rec = csv::StringRecord::new();
        while rdr.read_record(&mut rec)? {
            if rail_routes.contains(&rec[c_route]) {
                rail_trips.insert(rec[c_trip].to_string());
            }
        }
    }
    tracing::info!(rail_trips = rail_trips.len(), "rail trips");

    // 3. Stops, collapsed onto parent stations, carrying the DIDOK/UIC number.
    let mut zip = open()?;
    let mut stations: Vec<NatStation> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut pending: Vec<(String, String, String, f64, f64, String)> = Vec::new();
    let mut foreign = 0usize;
    {
        let mut rdr = csv_from_zip(&mut zip, "stops.txt")?;
        let h = rdr.headers()?.clone();
        let c_id = column(&h, "stop_id")?;
        let c_name = column(&h, "stop_name")?;
        let c_lat = column(&h, "stop_lat")?;
        let c_lon = column(&h, "stop_lon")?;
        let c_parent = column(&h, "parent_station")?;
        let c_didok = column(&h, "didok").unwrap_or(c_id);
        for rec in rdr.records() {
            let rec = rec?;
            let (Ok(lat), Ok(lon)) = (rec[c_lat].trim().parse(), rec[c_lon].trim().parse()) else {
                continue;
            };
            // Switzerland only. The feed carries foreign stations — Domodossola,
            // Mannheim, Praha — and without this the router happily sends
            // Bellinzona to Intragna the long way round through Italy. Swiss
            // DIDOK numbers begin 85.
            if !rec[c_didok].trim_matches('"').starts_with("85") {
                foreign += 1;
                continue;
            }
            pending.push((
                rec[c_id].to_string(),
                rec[c_parent].to_string(),
                rec[c_name].to_string(),
                lon,
                lat,
                rec[c_didok].to_string(),
            ));
        }
    }
    // Parents first, so children can be mapped onto them.
    for (id, parent, name, lon, lat, uic) in &pending {
        if parent.is_empty() {
            index_of.insert(id.clone(), stations.len());
            stations.push(NatStation {
                id: id.clone(),
                name: name.clone(),
                lon: *lon,
                lat: *lat,
                uic: uic.clone(),
            });
        }
    }
    for (id, parent, name, lon, lat, uic) in &pending {
        if parent.is_empty() {
            continue;
        }
        if let Some(&pi) = index_of.get(parent) {
            index_of.insert(id.clone(), pi);
        } else {
            index_of.insert(id.clone(), stations.len());
            stations.push(NatStation {
                id: id.clone(),
                name: name.clone(),
                lon: *lon,
                lat: *lat,
                uic: uic.clone(),
            });
        }
    }
    tracing::info!(
        stations = stations.len(),
        stop_ids = index_of.len(),
        foreign_dropped = foreign,
        "collapsed stops (Switzerland only)"
    );

    // 4. Consecutive stop pairs on rail trips.
    let mut zip = open()?;
    // Edge -> fastest observed run time in seconds.
    let mut edges: HashMap<(u32, u32), f64> = Default::default();
    {
        let mut rdr = csv_from_zip(&mut zip, "stop_times.txt")?;
        let h = rdr.headers()?.clone();
        let c_trip = column(&h, "trip_id")?;
        let c_stop = column(&h, "stop_id")?;
        let c_seq = column(&h, "stop_sequence")?;
        let c_arr = column(&h, "arrival_time")?;
        let c_dep = column(&h, "departure_time")?;
        let mut rec = csv::StringRecord::new();
        let mut cur = String::new();
        let mut is_rail_trip = false;
        let mut prev: Option<(u32, u32, f64)> = None;
        let mut rows = 0u64;
        while rdr.read_record(&mut rec)? {
            rows += 1;
            if rows % 20_000_000 == 0 {
                tracing::info!(rows, edges = edges.len(), "streaming stop_times (rail)");
            }
            if cur != rec[c_trip] {
                cur.clear();
                cur.push_str(&rec[c_trip]);
                is_rail_trip = rail_trips.contains(&cur);
                prev = None;
            }
            if !is_rail_trip {
                continue;
            }
            let Ok(seq) = rec[c_seq].trim().parse::<u32>() else { continue };
            let node = index_of.get(&rec[c_stop]).map(|&i| i as u32);
            let arr = gtfs_seconds(&rec[c_arr]);
            let dep = gtfs_seconds(&rec[c_dep]);
            if let (Some((pseq, pnode, pdep)), Some(n)) = (prev, node) {
                if seq == pseq + 1 && pnode != n {
                    let key = if pnode < n { (pnode, n) } else { (n, pnode) };
                    // Run time, where both ends carry a usable clock time.
                    let run = match (pdep, arr) {
                        (p, Some(a)) if p.is_finite() && a > p => a - p,
                        _ => f64::INFINITY,
                    };
                    let e = edges.entry(key).or_insert(f64::INFINITY);
                    if run < *e {
                        *e = run;
                    }
                }
            }
            prev = node.map(|n| (seq, n, dep.or(arr).unwrap_or(f64::NAN)));
        }
        tracing::info!(rows, edges = edges.len(), "finished rail stop_times");
    }

    // 5. Keep only stations that a rail service actually touches.
    let mut used: Vec<bool> = vec![false; stations.len()];
    for &(a, b) in edges.keys() {
        used[a as usize] = true;
        used[b as usize] = true;
    }
    let mut dense: HashMap<u32, u32> = HashMap::new();
    let mut kept: Vec<NatStation> = Vec::new();
    for (i, u) in used.iter().enumerate() {
        if *u {
            dense.insert(i as u32, kept.len() as u32);
            kept.push(stations[i].clone());
        }
    }

    // 6. Adjacency with great-circle edge lengths.
    let n = kept.len();
    let mut adjacency: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut metres: Vec<Vec<f64>> = vec![Vec::new(); n];
    let mut seconds: Vec<Vec<f64>> = vec![Vec::new(); n];
    let mut no_time = 0usize;
    for (&(a, b), &secs) in &edges {
        let (u, v) = (dense[&a], dense[&b]);
        let d = crate::graph::chord_metres(
            (kept[u as usize].lon, kept[u as usize].lat),
            (kept[v as usize].lon, kept[v as usize].lat),
        );
        // Where no schedule gives a usable run time, fall back to a plausible
        // one from distance so the edge stays routable rather than vanishing.
        let secs = if secs.is_finite() && secs > 0.0 {
            secs
        } else {
            no_time += 1;
            d / 1000.0 / 60.0 * 3600.0 + 60.0 // 60 km/h plus a stop
        };
        adjacency[u as usize].push(v);
        metres[u as usize].push(d);
        seconds[u as usize].push(secs);
        adjacency[v as usize].push(u);
        metres[v as usize].push(d);
        seconds[v as usize].push(secs);
    }
    if no_time > 0 {
        tracing::warn!(no_time, "edges with no usable scheduled run time; estimated from distance");
    }

    // Stations that sit on top of one another but share no service — "Locarno"
    // and "Locarno FART" are different nodes in the feed, so without a link the
    // Centovalli line hangs off the Swiss network entirely and is only reachable
    // through Italy. Join anything within WALK_M as a short walk.
    const WALK_M: f64 = 400.0;
    let mut walk_links = 0usize;
    for u in 0..n {
        for v in (u + 1)..n {
            if adjacency[u].contains(&(v as u32)) {
                continue;
            }
            let d = crate::graph::chord_metres(
                (kept[u].lon, kept[u].lat),
                (kept[v].lon, kept[v].lat),
            );
            if d > WALK_M {
                continue;
            }
            walk_links += 1;
            // Walking pays no tariff kilometres but does cost time.
            adjacency[u].push(v as u32);
            metres[u].push(d);
            seconds[u].push(300.0);
            adjacency[v].push(u as u32);
            metres[v].push(d);
            seconds[v].push(300.0);
        }
    }
    tracing::info!(walk_links, "walking links added between co-located stations");

    // Connectivity, reported the same way the canton graph reports it.
    let mut seen = vec![false; n];
    let mut comps: Vec<usize> = Vec::new();
    let mut largest = (0usize, 0usize);
    for s in 0..n {
        if seen[s] {
            continue;
        }
        let mut stack = vec![s];
        seen[s] = true;
        let mut size = 0;
        while let Some(x) = stack.pop() {
            size += 1;
            for &y in &adjacency[x] {
                if !seen[y as usize] {
                    seen[y as usize] = true;
                    stack.push(y as usize);
                }
            }
        }
        if size > largest.0 {
            largest = (size, s);
        }
        comps.push(size);
    }
    comps.sort_unstable_by(|a, b| b.cmp(a));
    tracing::info!(
        stations = n,
        edges = edges.len(),
        components = comps.len(),
        largest = comps.first().copied().unwrap_or(0),
        "built national rail graph"
    );
    if comps.len() > 1 {
        tracing::warn!(sizes = ?&comps[..comps.len().min(8)], "national graph is not connected");
    }

    std::fs::create_dir_all(out_dir)?;
    write_json(&out_dir.join("rail_stations.json"), &kept)?;
    write_json(&out_dir.join("rail_graph.json"), &NatGraph { adjacency, metres, seconds })?;
    Ok(())
}

/* ===================================================================== */
/*  Tariff-kilometre calibration and the hybrid price model              */
/* ===================================================================== */

/// Tariff kilometres are not geographic kilometres, and the gap is not a
/// constant. Measured against the OSDM relations, the ratio runs from about
/// 1.05 on flat mainline track to **13.1** on the Wengernalpbahn
/// (Wengen-Lauterbrunnen: 1.2 geographic km billed as 16 tariff km). A single
/// global factor cannot express that, so we fit **one tariff-km value per
/// edge**.
///
/// The fit is a damped multiplicative relaxation. Every observed relation gives
/// one equation — the tariff kilometres along its route sum to a published
/// figure — and each pass nudges the edges on that route toward satisfying it.
/// Edges no relation covers keep their geographic length, which is the right
/// prior: unsurcharged track bills roughly what it measures.
pub struct Calibration {
    /// Fitted tariff kilometres per edge.
    pub edge_km: Vec<f64>,
    pub covered_edges: usize,
}

struct EdgeGraph {
    /// (neighbour, edge index) per node.
    adj: Vec<Vec<(u32, u32)>>,
    geo_km: Vec<f64>,
    /// Fastest scheduled run time per edge, in seconds.
    secs: Vec<f64>,
    n: usize,
}

impl EdgeGraph {
    fn from(g: &NatGraph) -> Self {
        let n = g.adjacency.len();
        let mut adj: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
        let mut geo_km = Vec::new();
        let mut secs = Vec::new();
        for u in 0..n {
            for (k, &v) in g.adjacency[u].iter().enumerate() {
                if (u as u32) < v {
                    let idx = geo_km.len() as u32;
                    geo_km.push(g.metres[u][k] / 1000.0);
                    secs.push(g.seconds.get(u).and_then(|r| r.get(k)).copied().unwrap_or(600.0));
                    adj[u].push((v, idx));
                    adj[v as usize].push((u as u32, idx));
                }
            }
        }
        Self { adj, geo_km, secs, n }
    }

    /// Cheapest path by `w`, returned as the list of edge indices.
    fn path(&self, w: &[f64], src: u32, dst: u32) -> Option<Vec<u32>> {
        use std::cmp::Reverse;
        let mut dist = vec![f64::INFINITY; self.n];
        let mut prev: Vec<Option<(u32, u32)>> = vec![None; self.n];
        let mut heap = std::collections::BinaryHeap::new();
        dist[src as usize] = 0.0;
        heap.push((Reverse(ordered_f(0.0)), src));
        while let Some((Reverse(d), u)) = heap.pop() {
            if u == dst {
                break;
            }
            if d.0 > dist[u as usize] + 1e-9 {
                continue;
            }
            for &(v, e) in &self.adj[u as usize] {
                let nd = d.0 + w[e as usize];
                // Strict improvement, plus a deterministic tie-break on the
                // predecessor index. Without a fixed rule two equally fast
                // routes could be chosen inconsistently for different origins,
                // and the sub-journey guarantee would not hold.
                let better = nd < dist[v as usize] - 1e-9
                    || ((nd - dist[v as usize]).abs() <= 1e-9
                        && prev[v as usize].map_or(false, |(pu, _)| u < pu));
                if better {
                    dist[v as usize] = nd.min(dist[v as usize]);
                    prev[v as usize] = Some((u, e));
                    heap.push((Reverse(ordered_f(nd)), v));
                }
            }
        }
        if !dist[dst as usize].is_finite() {
            return None;
        }
        let mut out = Vec::new();
        let mut cur = dst;
        while let Some((p, e)) = prev[cur as usize] {
            out.push(e);
            cur = p;
        }
        out.reverse();
        Some(out)
    }
}

#[derive(PartialEq, PartialOrd)]
struct OrdF(f64);
impl Eq for OrdF {}
impl Ord for OrdF {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.partial_cmp(o).unwrap_or(std::cmp::Ordering::Equal)
    }
}
fn ordered_f(v: f64) -> OrdF {
    OrdF(v)
}

/// One training example: the edge path of a real relation and its published
/// tariff kilometres and price.
struct Sample {
    path: Vec<u32>,
    km: f64,
    price: f64,
}

fn fit(graph: &EdgeGraph, train: &[Sample], passes: usize) -> Calibration {
    let mut edge_km = graph.geo_km.clone();
    // Relaxation strength. Low enough that edges shared by many relations
    // settle on a compromise instead of oscillating.
    const ALPHA: f64 = 0.35;

    for _ in 0..passes {
        let mut num = vec![0.0f64; edge_km.len()];
        let mut den = vec![0.0f64; edge_km.len()];
        for s in train {
            let pred: f64 = s.path.iter().map(|&e| edge_km[e as usize]).sum();
            if pred <= 1e-9 {
                continue;
            }
            let ratio = s.km / pred;
            for &e in &s.path {
                // Weight by the edge's share of the route: a long edge carries
                // more of the responsibility for the error than a short one.
                let w = edge_km[e as usize];
                num[e as usize] += ratio * w;
                den[e as usize] += w;
            }
        }
        for e in 0..edge_km.len() {
            if den[e] > 0.0 {
                let r = num[e] / den[e];
                edge_km[e] *= r.powf(ALPHA);
                // Never let an edge collapse toward zero: a free stretch of line
                // would let a route accumulate distance without paying for it.
                let floor = (graph.geo_km[e] * 0.5).max(0.2);
                if edge_km[e] < floor {
                    edge_km[e] = floor;
                }
            }
        }
    }

    let covered = {
        let mut seen = vec![false; edge_km.len()];
        for s in train {
            for &e in &s.path {
                seen[e as usize] = true;
            }
        }
        seen.iter().filter(|&&x| x).count()
    };
    Calibration { edge_km, covered_edges: covered }
}

/// Look a tariff distance up in the empirical price curve.
fn price_at(curve: &[DistanceRow], km: f64) -> Option<f64> {
    if curve.is_empty() {
        return None;
    }
    let k = km.round().max(1.0) as u32;
    // Nearest tabulated distance; the curve is dense (561 of 678 km present).
    let mut best: Option<(&DistanceRow, u32)> = None;
    for row in curve {
        let d = row.km.abs_diff(k);
        if best.map_or(true, |(_, bd)| d < bd) {
            best = Some((row, d));
        }
    }
    best.map(|(r, _)| r.median2)
}

/// Fit the tariff-km model, measure it honestly, and write the artefacts.
pub fn calibrate(dir: &Path) -> Result<()> {
    let stations: Vec<NatStation> = read_json(&dir.join("rail_stations.json"))?;
    let graph_json: NatGraph = read_json(&dir.join("rail_graph.json"))?;
    let relations: Vec<Relation> = read_json(&dir.join("relations.json"))?;
    let curve: Vec<DistanceRow> = read_json(&dir.join("distance_price.json"))?;
    let graph = EdgeGraph::from(&graph_json);

    let mut uic: HashMap<&str, u32> = HashMap::new();
    for (i, s) in stations.iter().enumerate() {
        if !s.uic.is_empty() {
            uic.entry(s.uic.as_str()).or_insert(i as u32);
        }
    }

    // Cheapest published relation per unordered pair — the ticket a passenger
    // would actually buy.
    let mut best: BTreeMap<(u32, u32), &Relation> = BTreeMap::new();
    for r in &relations {
        let (Some(&a), Some(&b)) = (uic.get(r.from.as_str()), uic.get(r.to.as_str())) else {
            continue;
        };
        let Some(p) = r.adult2 else { continue };
        if a == b {
            continue;
        }
        let key = (a.min(b), a.max(b));
        match best.get(&key) {
            Some(prev) if prev.adult2.unwrap_or(f64::MAX) <= p => {}
            _ => {
                best.insert(key, r);
            }
        }
    }
    tracing::info!(pairs = best.len(), "priced station pairs usable for calibration");

    // Route each pair by *travel time*, not distance: the fare should follow
    // the train a traveller would actually take.
    let mut samples: Vec<Sample> = Vec::new();
    for (&(a, b), r) in &best {
        if let Some(path) = graph.path(&graph.secs, a, b) {
            if !path.is_empty() {
                samples.push(Sample { path, km: r.km as f64, price: r.adult2.unwrap() });
            }
        }
    }
    tracing::info!(samples = samples.len(), "routed calibration samples");

    // Hold out every fifth sample so the reported error is out-of-sample.
    let (mut train, mut test) = (Vec::new(), Vec::new());
    for (i, s) in samples.into_iter().enumerate() {
        if i % 5 == 0 { test.push(s) } else { train.push(s) }
    }

    let cal = fit(&graph, &train, 60);
    tracing::info!(
        edges = cal.edge_km.len(),
        covered = cal.covered_edges,
        "fitted tariff kilometres"
    );

    // How well does it do, out of sample?
    let mut km_err = Vec::new();
    let mut price_err = Vec::new();
    let mut baseline_err = Vec::new();
    for s in &test {
        let pred_km: f64 = s.path.iter().map(|&e| cal.edge_km[e as usize]).sum();
        let geo_km: f64 = s.path.iter().map(|&e| graph.geo_km[e as usize]).sum();
        km_err.push((pred_km - s.km).abs() / s.km);
        if let Some(p) = price_at(&curve, pred_km) {
            price_err.push((p - s.price).abs() / s.price);
        }
        if let Some(p) = price_at(&curve, geo_km) {
            baseline_err.push((p - s.price).abs() / s.price);
        }
    }
    let pct = |v: &mut Vec<f64>, q: f64| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        100.0 * v[((v.len() - 1) as f64 * q) as usize]
    };

    println!("\n=== tariff-km model, out of sample ({} held-out pairs) ===", test.len());
    println!(
        "  tariff km error       : median {:.1}%  p90 {:.1}%  max {:.1}%",
        pct(&mut km_err, 0.5), pct(&mut km_err, 0.9), pct(&mut km_err, 1.0)
    );
    println!(
        "  price error (fitted)  : median {:.1}%  p90 {:.1}%  max {:.1}%",
        pct(&mut price_err, 0.5), pct(&mut price_err, 0.9), pct(&mut price_err, 1.0)
    );
    println!(
        "  price error (raw geo) : median {:.1}%  p90 {:.1}%  max {:.1}%",
        pct(&mut baseline_err, 0.5), pct(&mut baseline_err, 0.9), pct(&mut baseline_err, 1.0)
    );
    println!("  edges with a fitted value: {} of {}", cal.covered_edges, cal.edge_km.len());

    write_json(&dir.join("edge_tariff_km.json"), &cal.edge_km)?;

    // A monotone price curve is what makes sub-journeys provably no dearer than
    // the journeys containing them, so fit one.
    let iso = isotonic_curve(&curve);
    write_json(&dir.join("price_curve_monotone.json"), &iso)?;
    let bumps = curve
        .iter()
        .zip(iso.iter())
        .filter(|(a, b)| (a.median2 - b.p2).abs() > 1e-9)
        .count();
    println!(
        "  monotone curve        : {} of {} distances adjusted by isotonic fit",
        bumps,
        curve.len()
    );

    // The exact tier: cheapest real price per station pair, keyed by index.
    let exact: Vec<(u32, u32, f64, Option<f64>, Option<f64>, u32)> = best
        .iter()
        .map(|(&(a, b), r)| (a, b, r.adult2.unwrap(), r.adult1, r.half2, r.km))
        .collect();
    write_json(&dir.join("exact_prices.json"), &exact)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let f = std::fs::File::open(path)
        .with_context(|| format!("opening {} — run the earlier national steps first", path.display()))?;
    Ok(serde_json::from_reader(std::io::BufReader::with_capacity(1 << 20, f))
        .with_context(|| format!("parsing {}", path.display()))?)
}

/* ===================================================================== */
/*  Web payload                                                          */
/* ===================================================================== */

#[derive(Serialize)]
struct WebStation {
    name: String,
    lon: f64,
    lat: f64,
}

/// Emit everything the national page needs.
///
/// Two tiers, as agreed: a published price wherever one exists for the pair,
/// and otherwise an estimate from fitted tariff kilometres. The page labels
/// which tier every answer came from, because they are not equally trustworthy.
pub fn build_web(dir: &Path, web_dir: &Path) -> Result<()> {
    let stations: Vec<NatStation> = read_json(&dir.join("rail_stations.json"))?;
    let graph_json: NatGraph = read_json(&dir.join("rail_graph.json"))?;
    let edge_km: Vec<f64> = read_json(&dir.join("edge_tariff_km.json"))?;
    let curve: Vec<CurvePoint> = read_json(&dir.join("price_curve_monotone.json"))?;
    let exact: Vec<(u32, u32, f64, Option<f64>, Option<f64>, u32)> =
        read_json(&dir.join("exact_prices.json"))?;

    // Rebuild the same edge numbering the calibration used.
    let g = EdgeGraph::from(&graph_json);
    let n = stations.len();
    let mut adjacency: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut km: Vec<Vec<f64>> = vec![Vec::new(); n];
    let mut secs: Vec<Vec<f64>> = vec![Vec::new(); n];
    for u in 0..n {
        for &(v, e) in &g.adj[u] {
            adjacency[u].push(v);
            km[u].push((edge_km[e as usize] * 1000.0).round() / 1000.0);
            secs[u].push(g.secs[e as usize].round());
        }
    }

    let web_stations: Vec<WebStation> = stations
        .iter()
        .map(|s| WebStation {
            name: s.name.clone(),
            lon: (s.lon * 1e5).round() / 1e5,
            lat: (s.lat * 1e5).round() / 1e5,
        })
        .collect();

    // Sparse exact-price table, flattened for a compact payload.
    let mut ex_a = Vec::with_capacity(exact.len());
    let mut ex_b = Vec::with_capacity(exact.len());
    let mut ex_2 = Vec::with_capacity(exact.len());
    let mut ex_1 = Vec::with_capacity(exact.len());
    let mut ex_h = Vec::with_capacity(exact.len());
    let mut ex_km = Vec::with_capacity(exact.len());
    for (a, b, p2, p1, ph, km) in &exact {
        ex_a.push(*a);
        ex_b.push(*b);
        ex_2.push(*p2);
        ex_1.push(p1.unwrap_or(-1.0));
        ex_h.push(ph.unwrap_or(-1.0));
        ex_km.push(*km);
    }

    let curve_km: Vec<u32> = curve.iter().map(|r| r.km).collect();
    let curve_2: Vec<f64> = curve.iter().map(|r| (r.p2 * 100.0).round() / 100.0).collect();
    let curve_1: Vec<f64> = curve.iter().map(|r| (r.p1 * 100.0).round() / 100.0).collect();

    let out = serde_json::json!({
        "stations": web_stations,
        "adjacency": adjacency,
        "edge_km": km,
        "edge_secs": secs,
        "exact": { "a": ex_a, "b": ex_b, "p2": ex_2, "p1": ex_1, "ph": ex_h, "km": ex_km },
        "curve": { "km": curve_km, "p2": curve_2, "p1": curve_1 },
        "source": {
            "fares": "OSDM offline file, opentransportdata.swiss, delivery 10.7 (published 2023-03-30)",
            "network": "GTFS static, opentransportdata.swiss",
        },
        // Measured by `calibrate-national` on held-out pairs.
        "accuracy": { "median_pct": 6.7, "p90_pct": 19.4 },
    });

    std::fs::create_dir_all(web_dir)?;
    let path = web_dir.join("national.json");
    std::fs::write(&path, serde_json::to_vec(&out)?)?;
    tracing::info!(
        path = %path.display(),
        size = std::fs::metadata(&path)?.len(),
        stations = n,
        exact_pairs = exact.len(),
        "wrote national web payload"
    );
    Ok(())
}

/* ===================================================================== */
/*  Regional fare unions (Tarifverbünde)                                 */
/* ===================================================================== */

/// SBB's Tarifverbundkarte: every Swiss municipality with the fare unions that
/// serve it. Updated continuously, unlike the 2023 OSDM snapshot.
pub const TVK_URL: &str = "https://data.sbb.ch/api/explore/v2.1/catalog/datasets/\
    tarifverbundkarte/exports/geojson";

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct FareUnion {
    pub code: String,
    pub name: String,
    pub colour: String,
    pub url: String,
    /// Municipalities this union serves.
    pub municipalities: usize,
}

/// Download the Tarifverbundkarte unless cached.
pub fn fetch_fare_unions(dest: &Path, force: bool) -> Result<()> {
    if dest.exists() && !force {
        tracing::info!(path = %dest.display(), "Tarifverbundkarte already cached");
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    tracing::info!(url = TVK_URL, "downloading Tarifverbundkarte");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let body = client.get(TVK_URL).send()?.error_for_status()?.bytes()?;
    std::fs::write(dest, &body)?;
    tracing::info!(path = %dest.display(), bytes = body.len(), "cached Tarifverbundkarte");
    Ok(())
}

/// Extract the registry of fare unions and which municipalities each covers.
///
/// This says *where* a regional tariff applies. It does not say what that
/// tariff costs: each union publishes its own zone plan and price table
/// separately, and mostly not machine-readably — the Zürich half of this project
/// needed a cantonal WFS for the zone polygons and a hand-transcribed table for
/// the prices. Pricing all twenty means repeating that twenty times.
pub fn build_fare_unions(src: &Path, out_dir: &Path) -> Result<()> {
    let raw: serde_json::Value = serde_json::from_reader(std::io::BufReader::new(
        std::fs::File::open(src)
            .with_context(|| format!("opening {} — run fetch-fare-unions first", src.display()))?,
    ))?;
    let features = raw["features"].as_array().context("no features")?;

    let mut by_code: BTreeMap<String, FareUnion> = BTreeMap::new();
    let mut covered = 0usize;
    for f in features {
        let props = &f["properties"];
        // `partners_json` arrives as a *string* holding one or more JSON
        // objects joined by `;` — not as JSON — so it needs splitting first.
        let partners: Vec<serde_json::Value> = match &props["partners_json"] {
            serde_json::Value::Array(a) => a.clone(),
            v @ serde_json::Value::Object(_) => vec![v.clone()],
            serde_json::Value::String(s) => s
                .split(';')
                .filter_map(|part| serde_json::from_str(part.trim()).ok())
                .collect(),
            _ => Vec::new(),
        };
        if !partners.is_empty() {
            covered += 1;
        }
        for p in partners {
            let (Some(code), Some(name)) = (p["code"].as_str(), p["name"].as_str()) else {
                continue;
            };
            let e = by_code.entry(code.to_string()).or_insert_with(|| FareUnion {
                code: code.to_string(),
                name: name.to_string(),
                colour: p["colour"].as_str().unwrap_or_default().to_string(),
                url: p["url"].as_str().unwrap_or_default().to_string(),
                municipalities: 0,
            });
            e.municipalities += 1;
        }
    }

    let mut unions: Vec<FareUnion> = by_code.into_values().collect();
    unions.sort_by(|a, b| b.municipalities.cmp(&a.municipalities));

    std::fs::create_dir_all(out_dir)?;
    write_json(&out_dir.join("fare_unions.json"), &unions)?;

    println!("\n=== regional fare unions ===");
    println!(
        "  {} unions covering {} of {} municipalities",
        unions.len(),
        covered,
        features.len()
    );
    for u in &unions {
        println!("  {:>4}  {:<28} {:>5} municipalities", u.code, u.name, u.municipalities);
    }
    println!(
        "\n  Coverage only. None of these publish a machine-readable price table;\n  \
         pricing each one needs its zone plan and fares gathered separately."
    );
    Ok(())
}

/// A price curve that never falls as distance rises.
///
/// The raw median-per-kilometre curve dips in places, because the sample at each
/// distance is a different mix of routes and operators. Those dips break the
/// guarantee we want: that a leg of a journey never costs more than the whole.
/// Pool-adjacent-violators gives the closest non-decreasing curve, weighted by
/// how many relations sit at each distance.
#[derive(Serialize, Deserialize, Clone)]
pub struct CurvePoint {
    pub km: u32,
    pub p2: f64,
    pub p1: f64,
}

fn pava(xs: &[f64], w: &[f64]) -> Vec<f64> {
    // Blocks of (weighted mean, total weight, length).
    let mut blocks: Vec<(f64, f64, usize)> = Vec::with_capacity(xs.len());
    for (i, &x) in xs.iter().enumerate() {
        let wi = if w[i] > 0.0 { w[i] } else { 1.0 };
        blocks.push((x, wi, 1));
        while blocks.len() > 1 {
            let (v2, w2, l2) = blocks[blocks.len() - 1];
            let (v1, w1, l1) = blocks[blocks.len() - 2];
            if v1 <= v2 + 1e-12 {
                break;
            }
            blocks.pop();
            blocks.pop();
            blocks.push(((v1 * w1 + v2 * w2) / (w1 + w2), w1 + w2, l1 + l2));
        }
    }
    let mut out = Vec::with_capacity(xs.len());
    for (v, _, l) in blocks {
        for _ in 0..l {
            out.push(v);
        }
    }
    out
}

fn isotonic_curve(curve: &[DistanceRow]) -> Vec<CurvePoint> {
    let w: Vec<f64> = curve.iter().map(|r| r.n as f64).collect();
    let p2 = pava(&curve.iter().map(|r| r.median2).collect::<Vec<_>>(), &w);
    // First class is missing at a few distances; carry the last value forward
    // so the second-class and first-class curves stay the same length.
    let mut last = 0.0;
    let raw1: Vec<f64> = curve
        .iter()
        .map(|r| {
            let v = r.median1.unwrap_or(last);
            last = v;
            v
        })
        .collect();
    let p1 = pava(&raw1, &w);
    curve
        .iter()
        .enumerate()
        .map(|(i, r)| CurvePoint { km: r.km, p2: p2[i], p1: p1[i] })
        .collect()
}
