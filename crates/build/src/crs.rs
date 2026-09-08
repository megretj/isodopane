//! LV95 (EPSG:2056) -> WGS84 (EPSG:4326).
//!
//! The Kanton Zürich WFS advertises EPSG:4326 and even stamps that CRS on its
//! output, but the coordinates it returns are LV95 regardless of what `srsName`
//! we ask for. So we reproject ourselves.
//!
//! This is swisstopo's official approximate formula (from "Formeln und Konstanten
//! für die Berechnung der Schweizerischen schiefachsigen Zylinderprojektion und
//! der Transformation zwischen Koordinatensystemen"). It is accurate to well
//! under a metre across Switzerland, which is two orders of magnitude finer than
//! the simplification tolerance we apply to the zone polygons — exact for our
//! purposes, and it avoids a native `proj` dependency entirely.

/// Convert LV95 easting/northing (metres) to WGS84 (lon, lat) in degrees.
pub fn lv95_to_wgs84(e: f64, n: f64) -> (f64, f64) {
    // Express as civilian units relative to the Bern projection origin, in
    // units of 1000 km.
    let y = (e - 2_600_000.0) / 1_000_000.0;
    let x = (n - 1_200_000.0) / 1_000_000.0;

    let lon = 2.677_909_4
        + 4.728_982_0 * y
        + 0.791_484_0 * y * x
        + 0.130_600_0 * y * x * x
        - 0.043_610_0 * y.powi(3);

    let lat = 16.902_389_2
        + 3.238_272_0 * x
        - 0.270_978_0 * y * y
        - 0.002_528_0 * x * x
        - 0.044_700_0 * y * y * x
        - 0.014_000_0 * x.powi(3);

    // Results above are in units of 10_000 seconds of arc.
    (lon * 100.0 / 36.0, lat * 100.0 / 36.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// swisstopo's published reference point: Zimmerwald observatory,
    /// 46°52'37.540"N 7°27'54.983"E. The approximate formula lands within
    /// ~0.25 m of it, which is far finer than our simplification tolerance.
    #[test]
    fn zimmerwald_reference_point() {
        let (lon, lat) = lv95_to_wgs84(2_602_030.74, 1_191_775.03);
        let (want_lon, want_lat) = (7.465_273_056, 46.877_094_444);
        // 1e-5 degrees is about 1.1 m.
        assert!((lon - want_lon).abs() < 1e-5, "lon was {lon}, want {want_lon}");
        assert!((lat - want_lat).abs() < 1e-5, "lat was {lat}, want {want_lat}");
    }

    /// Sanity check that Zürich HB lands where Zürich HB actually is.
    #[test]
    fn zurich_hb_is_in_zurich() {
        let (lon, lat) = lv95_to_wgs84(2_683_200.0, 1_248_100.0);
        assert!((lon - 8.540).abs() < 0.01, "lon was {lon}");
        assert!((lat - 47.378).abs() < 0.01, "lat was {lat}");
    }
}
