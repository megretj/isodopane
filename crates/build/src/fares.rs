//! ZVV single-ticket price table.
//!
//! Prices are transcribed by hand from zvv.ch (Abos und Tickets ->
//! Einzelbillette) into `data/fares.toml`. ZVV revises them at the December
//! timetable change, so `valid_from` travels with the table and is shown in the
//! UI footer.
//!
//! Never adjust these numbers to make a `validate` fixture pass. A mismatch is a
//! bug in the graph or the zone assignment, not in the price table.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Prices for one Tarifstufe, in CHF.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Prices {
    pub second: f64,
    pub first: f64,
    pub reduced_second: f64,
    pub reduced_first: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tarifstufe {
    /// 1..=8, where 8 is the all-zones ticket.
    pub stufe: u32,
    /// Human label, e.g. "Lokalnetz", "1-2 Zonen", "Alle Zonen".
    pub label: String,
    /// Validity window in minutes.
    pub validity_minutes: u32,
    #[serde(flatten)]
    pub prices: Prices,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FareTable {
    /// ISO date the table takes effect.
    pub valid_from: String,
    pub source: String,
    pub stufen: Vec<Tarifstufe>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    First,
    Second,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reduction {
    Full,
    Reduced,
}

impl FareTable {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading fare table {}", path.display()))?;
        let table: FareTable = toml::from_str(&text)
            .with_context(|| format!("parsing fare table {}", path.display()))?;
        table.check()?;
        Ok(table)
    }

    fn check(&self) -> Result<()> {
        for expected in 1..=8u32 {
            anyhow::ensure!(
                self.stufen.iter().any(|s| s.stufe == expected),
                "fare table is missing Tarifstufe {expected}"
            );
        }
        Ok(())
    }

    /// Map a zone weight to its Tarifstufe.
    ///
    /// Weight 1 and 2 collapse onto Stufe 1 and 2 respectively; a weight of 8 or
    /// more is capped at the all-zones ticket.
    pub fn stufe_for_weight(weight: u32) -> u32 {
        weight.clamp(1, 8)
    }

    pub fn price(&self, weight: u32, class: Class, reduction: Reduction) -> Option<f64> {
        let stufe = Self::stufe_for_weight(weight);
        let row = self.stufen.iter().find(|s| s.stufe == stufe)?;
        Some(match (class, reduction) {
            (Class::Second, Reduction::Full) => row.prices.second,
            (Class::First, Reduction::Full) => row.prices.first,
            (Class::Second, Reduction::Reduced) => row.prices.reduced_second,
            (Class::First, Reduction::Reduced) => row.prices.reduced_first,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> FareTable {
        FareTable::load(Path::new("../../data/fares.toml")).expect("fares.toml must parse")
    }

    #[test]
    fn weight_maps_to_tarifstufe() {
        assert_eq!(FareTable::stufe_for_weight(1), 1);
        assert_eq!(FareTable::stufe_for_weight(2), 2);
        assert_eq!(FareTable::stufe_for_weight(7), 7);
        assert_eq!(FareTable::stufe_for_weight(8), 8);
        assert_eq!(FareTable::stufe_for_weight(30), 8, "caps at all-zones");
    }

    #[test]
    fn known_prices_round_trip() {
        let t = table();
        assert_eq!(t.price(1, Class::Second, Reduction::Full), Some(2.80));
        assert_eq!(t.price(2, Class::Second, Reduction::Full), Some(4.70));
        assert_eq!(t.price(2, Class::First, Reduction::Full), Some(7.80));
        assert_eq!(t.price(8, Class::Second, Reduction::Reduced), Some(9.00));
        assert_eq!(t.price(12, Class::Second, Reduction::Full), Some(18.00));
    }

    #[test]
    fn prices_increase_with_zone_count() {
        let t = table();
        let mut prev = 0.0;
        for w in 1..=8 {
            let p = t.price(w, Class::Second, Reduction::Full).unwrap();
            assert!(p > prev, "Stufe {w} priced {p}, not above {prev}");
            prev = p;
        }
    }
}
