//! Fetching fare-zone geometry from the Kanton Zürich OGD WFS.
//!
//! Dataset: "Tarifzonen des öffentlichen Verkehrs" (GIS-ZH Nr. 348), maintained
//! by the ARE GIS-Zentrum on behalf of ZVV. Licence: opendata.swiss "Reference
//! Required" (BY) — attribution is mandatory and appears in the UI footer.
//!
//! Discovered from GetCapabilities (see README):
//!   typename:   ms:ogd-0348_giszhpub_zvv_tarifzonen_f
//!   zone field: `zone` (integer)
//!
//! The server advertises EPSG:4326 and stamps it on its output, but returns LV95
//! coordinates whatever `srsName` we send. We reproject in `crs.rs`.

use anyhow::{Context, Result};
use std::path::Path;

pub const WFS_BASE: &str = "https://maps.zh.ch/wfs/OGDZHWFS";
pub const TYPENAME: &str = "ms:ogd-0348_giszhpub_zvv_tarifzonen_f";
/// Attribute holding the zone number (110, 121, 154, ...).
pub const ZONE_FIELD: &str = "zone";

pub fn zones_url() -> String {
    format!(
        "{WFS_BASE}?Service=WFS&Version=2.0.0&Request=GetFeature\
         &typeName={TYPENAME}&outputFormat=application/json&srsName=EPSG:4326"
    )
}

/// Download the zone GeoJSON to `dest`, unless it is already cached there.
pub fn fetch_zones(dest: &Path, force: bool) -> Result<()> {
    if dest.exists() && !force {
        tracing::info!(path = %dest.display(), "zone geometry already cached, skipping fetch");
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let url = zones_url();
    tracing::info!(%url, "requesting fare zones from WFS");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let body = client
        .get(&url)
        .send()
        .context("WFS request failed")?
        .error_for_status()
        .context("WFS returned an error status")?
        .bytes()
        .context("reading WFS response body")?;

    anyhow::ensure!(
        body.starts_with(b"{"),
        "WFS did not return JSON — got: {}",
        String::from_utf8_lossy(&body[..body.len().min(200)])
    );

    std::fs::write(dest, &body)
        .with_context(|| format!("writing {}", dest.display()))?;
    tracing::info!(path = %dest.display(), bytes = body.len(), "cached zone geometry");
    Ok(())
}

/// GTFS static feed for all of Switzerland, from opentransportdata.swiss.
/// The `/permalink` endpoint always redirects to the current publication.
pub const GTFS_PERMALINK: &str =
    "https://data.opentransportdata.swiss/en/dataset/timetable-2026-gtfs2020/permalink";

/// Download the GTFS zip unless a copy is already present. It is ~240 MB, so
/// this checks before spending the bandwidth.
pub fn fetch_gtfs(dest: &Path, force: bool) -> Result<()> {
    if dest.exists() && !force {
        let size = std::fs::metadata(dest)?.len();
        tracing::info!(path = %dest.display(), size, "GTFS already cached, skipping download");
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::info!(url = GTFS_PERMALINK, "downloading GTFS (this is a large file)");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;
    let mut resp = client
        .get(GTFS_PERMALINK)
        .send()
        .context("GTFS request failed")?
        .error_for_status()
        .context("GTFS download returned an error status")?;

    // Stream to a .part file so an interrupted run never leaves a truncated zip
    // that a later run would mistake for a complete cache.
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let bytes = std::io::copy(&mut resp, &mut file)?;
    drop(file);
    std::fs::rename(&part, dest)?;

    tracing::info!(path = %dest.display(), bytes, "cached GTFS feed");
    Ok(())
}

/// Download the OSDM offline file (machine-readable national fares) unless it
/// is already cached. See `national.rs` for why this replaces the T601 PDF.
pub fn fetch_osdm(dest: &std::path::Path, force: bool) -> Result<()> {
    if dest.exists() && !force {
        let size = std::fs::metadata(dest)?.len();
        tracing::info!(path = %dest.display(), size, "OSDM file already cached, skipping download");
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracing::info!(url = crate::national::OSDM_URL, "downloading OSDM offline file");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;
    let mut resp = client
        .get(crate::national::OSDM_URL)
        .send()
        .context("OSDM request failed")?
        .error_for_status()
        .context("OSDM download returned an error status")?;
    let part = dest.with_extension("part");
    let mut file = std::fs::File::create(&part)?;
    let bytes = std::io::copy(&mut resp, &mut file)?;
    drop(file);
    std::fs::rename(&part, dest)?;
    tracing::info!(path = %dest.display(), bytes, "cached OSDM offline file");
    Ok(())
}
