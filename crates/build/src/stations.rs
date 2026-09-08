//! GTFS stops -> stations: parent collapsing, zone assignment, edge extraction.
//!
//! The national GTFS feed is ~3.8 GB unpacked, with `stop_times.txt` alone over
//! 3 GB. We therefore read it in two passes with different tools:
//!
//! - `stops.txt` (13 MB) via `gtfs-structures` with `read_stop_times(false)`,
//!   which gives typed stops with `parent_station` for free.
//! - `stop_times.txt` streamed row by row with a plain CSV reader, keeping only
//!   `(trip_id, stop_id, stop_sequence)`. Materialising it into typed structs
//!   would need well over the available RAM, and we only ever want the
//!   deduplicated set of consecutive stop pairs.

use crate::zones::Zone;
use anyhow::{Context, Result};
use geo::algorithm::contains::Contains;
use geo_types::{Coord, Point, Rect};
use rstar::{RTree, RTreeObject, AABB};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Station {
    pub id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    /// Fare zone number this station sits in.
    pub zone_id: u32,
}

/// R-tree entry: a zone's bounding box, used to prefilter point-in-polygon.
struct ZoneBox {
    idx: usize,
    rect: Rect<f64>,
}

impl RTreeObject for ZoneBox {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(
            [self.rect.min().x, self.rect.min().y],
            [self.rect.max().x, self.rect.max().y],
        )
    }
}

/// Assigns points to fare zones, with a bounding-box prefilter so
/// point-in-polygon over thousands of stops stays fast.
pub struct ZoneLocator<'a> {
    zones: &'a [Zone],
    tree: RTree<ZoneBox>,
}

impl<'a> ZoneLocator<'a> {
    pub fn new(zones: &'a [Zone]) -> Self {
        use geo::algorithm::bounding_rect::BoundingRect;
        let boxes: Vec<ZoneBox> = zones
            .iter()
            .enumerate()
            .filter_map(|(idx, z)| z.geometry.bounding_rect().map(|rect| ZoneBox { idx, rect }))
            .collect();
        Self { zones, tree: RTree::bulk_load(boxes) }
    }

    /// Zone number containing `(lon, lat)`, if any.
    ///
    /// v1 assigns each station exactly one zone. Some ZVV stops sit on a
    /// boundary and are tariff-valid in two or more zones; see the limitations
    /// note in the README.
    pub fn locate(&self, lon: f64, lat: f64) -> Option<u32> {
        let p = Point::new(lon, lat);
        for candidate in self.tree.locate_all_at_point(&[lon, lat]) {
            let zone = &self.zones[candidate.idx];
            if zone.geometry.contains(&p) {
                return Some(zone.number);
            }
        }
        None
    }
}

impl rstar::PointDistance for ZoneBox {
    fn distance_2(&self, point: &[f64; 2]) -> f64 {
        self.envelope().distance_2(point)
    }
    fn contains_point(&self, point: &[f64; 2]) -> bool {
        let c = Coord { x: point[0], y: point[1] };
        c.x >= self.rect.min().x
            && c.x <= self.rect.max().x
            && c.y >= self.rect.min().y
            && c.y <= self.rect.max().y
    }
}

/// A GTFS stop reduced to what we need, after parent collapsing.
#[derive(Debug, Clone)]
pub struct RawStop {
    pub id: String,
    pub name: String,
    pub lon: f64,
    pub lat: f64,
}

/// Read `stops.txt` and collapse child stops onto their `parent_station`.
///
/// Returns the station list plus a map from *every* GTFS stop id (children
/// included) to the index of the station it belongs to, so `stop_times` rows can
/// be resolved without a second lookup table.
pub fn read_stops(gtfs_path: &Path) -> Result<(Vec<RawStop>, HashMap<String, usize>)> {
    tracing::info!(path = %gtfs_path.display(), "reading GTFS stops (skipping stop_times)");
    let gtfs = gtfs_structures::GtfsReader::default()
        .read_stop_times(false)
        .read_shapes(false)
        .unkown_enum_as_default(true)
        .read_from_path(gtfs_path.to_str().context("non-UTF8 GTFS path")?)
        .map_err(|e| anyhow::anyhow!("parsing GTFS stops: {e}"))?;

    // `gtfs.stops` is a HashMap, so iterating it directly would number the
    // stations differently on every run — making stations.json and graph.json
    // irreproducible and invalidating any cached cross-check. Walk the stop ids
    // in sorted order instead so a rebuild from identical inputs is identical.
    let mut stop_ids: Vec<&String> = gtfs.stops.keys().collect();
    stop_ids.sort_unstable();

    // First pass: every stop that is its own station (no parent).
    let mut stations: Vec<RawStop> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();

    for id in &stop_ids {
        let stop = &gtfs.stops[*id];
        if stop.parent_station.is_some() {
            continue;
        }
        let (Some(lat), Some(lon)) = (stop.latitude, stop.longitude) else {
            continue;
        };
        index_of.insert((*id).clone(), stations.len());
        stations.push(RawStop {
            id: (*id).clone(),
            name: stop.name.clone().unwrap_or_default(),
            lon,
            lat,
        });
    }

    // Second pass: map children onto their parent's index. A child whose parent
    // is missing from the feed becomes its own station rather than vanishing.
    let mut orphans = 0usize;
    for id in &stop_ids {
        let stop = &gtfs.stops[*id];
        let Some(parent) = &stop.parent_station else {
            continue;
        };
        if let Some(&pi) = index_of.get(parent) {
            index_of.insert((*id).clone(), pi);
        } else {
            let (Some(lat), Some(lon)) = (stop.latitude, stop.longitude) else {
                continue;
            };
            orphans += 1;
            index_of.insert((*id).clone(), stations.len());
            stations.push(RawStop {
                id: (*id).clone(),
                name: stop.name.clone().unwrap_or_default(),
                lon,
                lat,
            });
        }
    }

    if orphans > 0 {
        tracing::warn!(orphans, "child stops whose parent_station is absent from the feed");
    }
    tracing::info!(
        stations = stations.len(),
        stop_ids = index_of.len(),
        "collapsed GTFS stops onto stations"
    );
    Ok((stations, index_of))
}

/// Zones a straight segment between two stops passes through.
///
/// ZVV charges for every zone a vehicle crosses, not just the zones it stops in,
/// so each edge needs the whole traversed set. We do not have route geometry —
/// `shapes.txt` is deferred to Phase 2 — so we sample the straight line between
/// consecutive stops at roughly `STEP_M` intervals.
///
/// For short hops the two stops are usually in the same or adjacent zones and
/// the approximation is exact. It matters most for long non-stop express hops,
/// and those run along corridors straight enough for this to pick up the right
/// intermediate zones. Where a real alignment curves away from the chord this
/// can miss a zone or add a spurious one; `validate`'s fixtures are what catch
/// that, and `shapes.txt` is the exact fix if it ever proves necessary.
const STEP_M: f64 = 250.0;

pub fn traversed_zones(
    locator: &ZoneLocator,
    zone_bit: &dyn Fn(u32) -> Option<u8>,
    a: (f64, f64),
    b: (f64, f64),
) -> u64 {
    // Equirectangular metres — fine over the few kilometres of a stop hop.
    let mid_lat = ((a.1 + b.1) / 2.0).to_radians();
    let dx = (b.0 - a.0).to_radians() * 6_371_000.0 * mid_lat.cos();
    let dy = (b.1 - a.1).to_radians() * 6_371_000.0;
    let dist = (dx * dx + dy * dy).sqrt();

    let steps = ((dist / STEP_M).ceil() as usize).clamp(1, 400);
    let mut mask = 0u64;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        let lon = a.0 + (b.0 - a.0) * t;
        let lat = a.1 + (b.1 - a.1) * t;
        if let Some(z) = locator.locate(lon, lat) {
            if let Some(bit) = zone_bit(z) {
                mask |= 1u64 << bit;
            }
        }
    }
    mask
}

/// Stream `stop_times.txt` and collect deduplicated consecutive station pairs.
///
/// Only rows whose stop resolves to a station in `keep` produce edges. When a
/// trip passes through a dropped stop we do not bridge across it: joining the
/// stops either side would invent a direct connection that no service actually
/// offers, which is exactly the kind of fake adjacency the topology-based model
/// exists to avoid.
pub fn stream_edges(
    gtfs_path: &Path,
    index_of: &HashMap<String, usize>,
    keep: &HashMap<usize, u32>,
) -> Result<Vec<(u32, u32)>> {
    let file = std::fs::File::open(gtfs_path)
        .with_context(|| format!("opening {}", gtfs_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("opening GTFS zip")?;
    let entry = archive
        .by_name("stop_times.txt")
        .context("GTFS zip has no stop_times.txt")?;

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(std::io::BufReader::with_capacity(1 << 22, entry));

    let headers = reader.headers().context("reading stop_times header")?.clone();
    let col = |name: &str| -> Result<usize> {
        headers
            .iter()
            .position(|h| h.trim_start_matches('\u{feff}') == name)
            .with_context(|| format!("stop_times.txt has no `{name}` column"))
    };
    let c_trip = col("trip_id")?;
    let c_stop = col("stop_id")?;
    let c_seq = col("stop_sequence")?;

    let mut edges: HashSet<(u32, u32)> = HashSet::new();
    let mut current_trip: Option<String> = None;
    // (stop_sequence, station index) of the previous row in this trip.
    let mut prev: Option<(u32, u32)> = None;
    let mut rows = 0u64;
    let mut record = csv::StringRecord::new();

    while reader.read_record(&mut record).context("reading stop_times row")? {
        rows += 1;
        if rows % 20_000_000 == 0 {
            tracing::info!(rows, edges = edges.len(), "streaming stop_times");
        }

        let trip = &record[c_trip];
        if current_trip.as_deref() != Some(trip) {
            current_trip = Some(trip.to_string());
            prev = None;
        }

        let seq: u32 = match record[c_seq].trim().parse() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let station = index_of.get(&record[c_stop]).copied();
        let node = station.filter(|s| keep.contains_key(s)).map(|s| s as u32);

        // Only consecutive stop_sequence values form an edge; a gap means we
        // skipped a stop outside the canton and must not bridge it.
        if let (Some((pseq, pnode)), Some(node)) = (prev, node) {
            if seq == pseq + 1 && pnode != node {
                let key = if pnode < node { (pnode, node) } else { (node, pnode) };
                edges.insert(key);
            }
        }
        // A dropped stop clears `prev`, so the `seq == pseq + 1` check above can
        // never bridge across it.
        prev = node.map(|n| (seq, n));
    }

    tracing::info!(rows, edges = edges.len(), "finished streaming stop_times");
    let mut out: Vec<(u32, u32)> = edges.into_iter().collect();
    out.sort_unstable();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{LineString, MultiPolygon, Polygon};

    fn square(number: u32, x0: f64, y0: f64, x1: f64, y1: f64) -> Zone {
        Zone {
            number,
            geometry: MultiPolygon(vec![Polygon::new(
                LineString::from(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]),
                vec![],
            )]),
        }
    }

    #[test]
    fn locator_assigns_points_to_the_containing_zone() {
        let zones = vec![square(110, 0.0, 0.0, 1.0, 1.0), square(121, 2.0, 2.0, 3.0, 3.0)];
        let loc = ZoneLocator::new(&zones);
        assert_eq!(loc.locate(0.5, 0.5), Some(110));
        assert_eq!(loc.locate(2.5, 2.5), Some(121));
        assert_eq!(loc.locate(1.5, 1.5), None, "gaps between zones are outside");
        assert_eq!(loc.locate(50.0, 50.0), None);
    }
}
