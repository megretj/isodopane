//! Fare-zone polygons: reproject, dissolve by zone number, simplify, emit.

use crate::crs::lv95_to_wgs84;
use crate::zoneset::weight_for_zone;
use anyhow::{Context, Result};
use geo::algorithm::simplify::Simplify;
use geo_types::{Coord, LineString, MultiPolygon, Polygon};
use std::collections::BTreeMap;
use std::path::Path;

/// One fare zone after dissolving.
#[derive(Debug, Clone)]
pub struct Zone {
    pub number: u32,
    pub geometry: MultiPolygon<f64>,
}

/// Simplification tolerance in degrees, ~15 m at Zürich's latitude.
///
/// Zone borders are administrative, not physical; nobody will notice the
/// difference and the payload shrinks a lot.
const SIMPLIFY_TOLERANCE_DEG: f64 = 0.000_15;

fn ring_to_wgs84(ring: &[Vec<f64>]) -> LineString<f64> {
    LineString(
        ring.iter()
            .map(|p| {
                let (x, y) = (p[0], p[1]);
                // The WFS mislabels its CRS: it says 4326 but sends LV95. Detect
                // by magnitude rather than trusting the declaration, so this
                // keeps working if the server is ever fixed.
                let (lon, lat) = if x.abs() > 180.0 { lv95_to_wgs84(x, y) } else { (x, y) };
                Coord { x: lon, y: lat }
            })
            .collect(),
    )
}

fn polygon_from_rings(rings: &[Vec<Vec<f64>>]) -> Option<Polygon<f64>> {
    let mut it = rings.iter();
    let exterior = ring_to_wgs84(it.next()?);
    let interiors = it.map(|r| ring_to_wgs84(r)).collect();
    Some(Polygon::new(exterior, interiors))
}

/// Read the cached WFS response and dissolve into one MultiPolygon per zone.
///
/// A zone commonly arrives as several disjoint polygons; it must end as a single
/// feature so the frontend can drive one fill per zone.
pub fn load_and_dissolve(path: &Path) -> Result<Vec<Zone>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).context("parsing zone GeoJSON")?;

    let features = json["features"]
        .as_array()
        .context("zone GeoJSON has no `features` array")?;

    let mut by_zone: BTreeMap<u32, Vec<Polygon<f64>>> = BTreeMap::new();
    let mut unassigned = 0usize;

    for feature in features {
        // Some features carry no zone number (they arrive with `farbe_plan`
        // 1111 and cover lakes and the unserved forest ridges). They hold no
        // tariff information, so they cannot price anything — drop them, and
        // any stop that falls only inside one is treated as out of network,
        // exactly like a stop outside the canton.
        let Some(number) = feature["properties"][crate::wfs::ZONE_FIELD].as_u64() else {
            unassigned += 1;
            continue;
        };
        let number = number as u32;

        let geom = &feature["geometry"];
        let coords = &geom["coordinates"];
        match geom["type"].as_str() {
            Some("Polygon") => {
                let rings: Vec<Vec<Vec<f64>>> = serde_json::from_value(coords.clone())?;
                if let Some(p) = polygon_from_rings(&rings) {
                    by_zone.entry(number).or_default().push(p);
                }
            }
            Some("MultiPolygon") => {
                let polys: Vec<Vec<Vec<Vec<f64>>>> = serde_json::from_value(coords.clone())?;
                for rings in &polys {
                    if let Some(p) = polygon_from_rings(rings) {
                        by_zone.entry(number).or_default().push(p);
                    }
                }
            }
            other => anyhow::bail!("unexpected geometry type {other:?} in zone data"),
        }
    }

    anyhow::ensure!(!by_zone.is_empty(), "no zone features found");
    if unassigned > 0 {
        tracing::warn!(
            unassigned,
            kept = by_zone.len(),
            "skipped features with no zone number (lakes and unserved areas)"
        );
    }

    Ok(by_zone
        .into_iter()
        .map(|(number, polys)| Zone {
            number,
            geometry: MultiPolygon(polys),
        })
        .collect())
}

/// Simplify each zone's geometry in place.
pub fn simplify(zones: &mut [Zone]) {
    for z in zones.iter_mut() {
        z.geometry = z.geometry.simplify(SIMPLIFY_TOLERANCE_DEG);
    }
}

/// Write `web/data/zones.geojson` with `zone_id`, `name` and `weight`.
pub fn write_geojson(zones: &[Zone], dest: &Path) -> Result<()> {
    let features: Vec<serde_json::Value> = zones
        .iter()
        .map(|z| {
            let coords: Vec<Vec<Vec<[f64; 2]>>> = z
                .geometry
                .0
                .iter()
                .map(|poly| {
                    std::iter::once(poly.exterior())
                        .chain(poly.interiors().iter())
                        .map(|ring| {
                            ring.0
                                .iter()
                                .map(|c| [round6(c.x), round6(c.y)])
                                .collect()
                        })
                        .collect()
                })
                .collect();

            serde_json::json!({
                "type": "Feature",
                "id": z.number,
                "properties": {
                    "zone_id": z.number,
                    "name": format!("Zone {}", z.number),
                    "weight": weight_for_zone(z.number),
                },
                "geometry": { "type": "MultiPolygon", "coordinates": coords }
            })
        })
        .collect();

    let fc = serde_json::json!({
        "type": "FeatureCollection",
        "features": features,
    });

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(dest, serde_json::to_vec(&fc)?)
        .with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

/// ~0.1 m precision — well beyond what the simplification retains, and it keeps
/// the payload from carrying meaningless float noise.
fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lv95_rings_are_reprojected_but_wgs84_rings_are_not() {
        let lv95 = vec![vec![2_683_200.0, 1_248_100.0]];
        let ls = ring_to_wgs84(&lv95);
        assert!((ls.0[0].x - 8.540).abs() < 0.01);

        let wgs = vec![vec![8.54, 47.37]];
        let ls = ring_to_wgs84(&wgs);
        assert_eq!(ls.0[0].x, 8.54, "already-WGS84 input must pass through");
    }

    #[test]
    fn rounding_keeps_metre_precision() {
        assert_eq!(round6(8.540123456), 8.540123);
    }
}
