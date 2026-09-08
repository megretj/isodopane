//! Zone sets as u64 bitmasks.
//!
//! ZVV has ~60 fare zones, so the set of zones a journey has touched fits in a
//! single `u64`. Union is `|`, subset test is `(a & !b) == 0`, cardinality is
//! `popcount` — all single instructions, which is what makes the multi-label
//! search cheap enough to run in the browser on every click.

use serde::{Deserialize, Serialize};

/// A set of fare zones, one bit per zone *index* (not per zone number).
///
/// The mapping from zone number (110, 121, ...) to bit index lives in
/// [`ZoneIndex`]; this type deliberately knows nothing about zone numbers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ZoneSet(pub u64);

impl ZoneSet {
    pub const EMPTY: ZoneSet = ZoneSet(0);

    #[inline]
    pub fn single(idx: u8) -> Self {
        ZoneSet(1u64 << idx)
    }

    #[inline]
    pub fn with(self, idx: u8) -> Self {
        ZoneSet(self.0 | (1u64 << idx))
    }

    #[inline]
    pub fn union(self, other: ZoneSet) -> Self {
        ZoneSet(self.0 | other.0)
    }

    #[inline]
    pub fn contains(self, idx: u8) -> bool {
        self.0 & (1u64 << idx) != 0
    }

    #[inline]
    pub fn len(self) -> u32 {
        self.0.count_ones()
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when `self` is a subset of `other`.
    ///
    /// This is the dominance test that keeps the Pareto frontier small: a label
    /// whose zone set is a subset of another's can never cost more, now or after
    /// any continuation, so the superset can be discarded.
    #[inline]
    pub fn is_subset_of(self, other: ZoneSet) -> bool {
        (self.0 & !other.0) == 0
    }

    pub fn iter(self) -> impl Iterator<Item = u8> {
        let mut bits = self.0;
        std::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            let idx = bits.trailing_zeros() as u8;
            bits &= bits - 1;
            Some(idx)
        })
    }
}

impl std::fmt::Debug for ZoneSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ZoneSet{:?}", self.iter().collect::<Vec<_>>())
    }
}

/// Maps zone numbers to bit indices and carries each zone's tariff weight.
///
/// Zones 110 (city of Zürich) and 120 (city of Winterthur) each count as two
/// zones under Verbundtarif 651.8, so weight is not simply the set cardinality.
#[derive(Debug, Clone, Default)]
pub struct ZoneIndex {
    /// Zone numbers, ordered by bit index.
    numbers: Vec<u32>,
    /// Tariff weight per bit index (2 for 110 and 120, else 1).
    weights: Vec<u32>,
}

/// Zones that count double under the ZVV tariff.
pub const DOUBLE_WEIGHT_ZONES: [u32; 2] = [110, 120];

pub fn weight_for_zone(number: u32) -> u32 {
    if DOUBLE_WEIGHT_ZONES.contains(&number) {
        2
    } else {
        1
    }
}

impl ZoneIndex {
    /// Build from zone numbers. Sorted so bit indices are stable across runs.
    pub fn new(mut numbers: Vec<u32>) -> anyhow::Result<Self> {
        numbers.sort_unstable();
        numbers.dedup();
        anyhow::ensure!(
            numbers.len() <= 64,
            "{} fare zones exceeds the 64-bit ZoneSet capacity; \
             widen ZoneSet to u128 or a fixed bitset before continuing",
            numbers.len()
        );
        let weights = numbers.iter().map(|&n| weight_for_zone(n)).collect();
        Ok(Self { numbers, weights })
    }

    pub fn len(&self) -> usize {
        self.numbers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty()
    }

    pub fn numbers(&self) -> &[u32] {
        &self.numbers
    }

    pub fn index_of(&self, number: u32) -> Option<u8> {
        self.numbers.binary_search(&number).ok().map(|i| i as u8)
    }

    pub fn number_at(&self, idx: u8) -> u32 {
        self.numbers[idx as usize]
    }

    /// Total tariff weight of a zone set — the "number of zones" the ZVV bills.
    #[inline]
    pub fn weight(&self, set: ZoneSet) -> u32 {
        set.iter().map(|i| self.weights[i as usize]).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> ZoneIndex {
        ZoneIndex::new(vec![110, 120, 121, 154, 155]).unwrap()
    }

    #[test]
    fn subset_dominance() {
        let a = ZoneSet::EMPTY.with(0).with(1);
        let b = ZoneSet::EMPTY.with(0).with(1).with(2);
        assert!(a.is_subset_of(b));
        assert!(!b.is_subset_of(a));
        assert!(a.is_subset_of(a));
    }

    #[test]
    fn union_is_idempotent() {
        let s = ZoneSet::single(3);
        assert_eq!(s.with(3), s, "re-entering a zone must cost nothing");
    }

    #[test]
    fn city_zones_count_double() {
        let zi = idx();
        let z110 = zi.index_of(110).unwrap();
        let z154 = zi.index_of(154).unwrap();
        assert_eq!(zi.weight(ZoneSet::single(z110)), 2, "zone 110 counts double");
        assert_eq!(zi.weight(ZoneSet::single(z154)), 1);
        // Zürich HB -> Oerlikon: both inside 110, must bill as two zones.
        assert_eq!(zi.weight(ZoneSet::single(z110).with(z110)), 2);
    }

    #[test]
    fn iter_yields_set_bits() {
        let s = ZoneSet::EMPTY.with(0).with(5).with(63);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![0, 5, 63]);
        assert_eq!(s.len(), 3);
    }
}
