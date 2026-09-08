//! Build-time library for isodapane.ch.
//!
//! The CLI in `main.rs` is a thin wrapper over these modules; the fare logic
//! lives here so `validate` can exercise it directly.

pub mod crs;
pub mod fares;
pub mod graph;
pub mod national;
pub mod search;
pub mod stations;
pub mod wfs;
pub mod zones;
pub mod validate;
pub mod zoneset;
