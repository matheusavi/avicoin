use anyhow::{anyhow, Result};
use std::fmt;

pub const ATOMS_PER_AVI: u64 = 100_000_000;
pub const MAX_MONEY: u64 = 2_016_000 * ATOMS_PER_AVI;

/// ADR-0006. Fifty AVI, halved by integer right-shift every 20,160 blocks —
/// about a week at a thirty-second target. The ~2,016,000 AVI cap is what this
/// schedule sums to, not something any code checks.
pub const INITIAL_SUBSIDY: u64 = 50 * ATOMS_PER_AVI;
pub const HALVING_INTERVAL: u32 = 20_160;

pub fn subsidy(height: u32) -> Amount {
    let halvings = height / HALVING_INTERVAL;

    // Past 63 the shift is undefined rather than zero, and a chain does reach
    // heights that large — 63 halvings is 1.27 billion blocks.
    if halvings >= u64::BITS {
        return Amount::ZERO;
    }

    // Constructed directly: a right shift only ever makes the initial subsidy
    // smaller, so the MAX_MONEY bound `from_atoms` would check cannot fail.
    Amount(INITIAL_SUBSIDY >> halvings)
}

/// Atoms, never outside `0..=MAX_MONEY`. The arithmetic is checked anyway, so
/// correctness does not rest on that bound being right — ADR-0006.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Amount(u64);

impl Amount {
    pub const ZERO: Amount = Amount(0);

    /// For constants, where the bound is checked when the program is compiled
    /// rather than when it runs.
    pub const fn constant(atoms: u64) -> Amount {
        assert!(atoms <= MAX_MONEY, "a constant Amount is within MAX_MONEY");

        Amount(atoms)
    }

    pub fn from_atoms(atoms: u64) -> Result<Amount> {
        if atoms > MAX_MONEY {
            return Err(anyhow!("{atoms} atoms is above MAX_MONEY ({MAX_MONEY})"));
        }

        Ok(Amount(atoms))
    }

    /// The number a person reads. `Display` adds the unit; the API's JSON
    /// carries the atoms beside it, so it wants the number alone.
    pub fn in_avi(&self) -> String {
        format!("{}.{:08}", self.0 / ATOMS_PER_AVI, self.0 % ATOMS_PER_AVI)
    }

    pub fn atoms(&self) -> u64 {
        self.0
    }

    pub fn checked_add(&self, other: Amount) -> Option<Amount> {
        self.0.checked_add(other.0).and_then(in_range)
    }

    pub fn checked_sub(&self, other: Amount) -> Option<Amount> {
        self.0.checked_sub(other.0).and_then(in_range)
    }

    pub fn sum(amounts: impl IntoIterator<Item = Amount>) -> Option<Amount> {
        amounts
            .into_iter()
            .try_fold(Amount::ZERO, |total, next| total.checked_add(next))
    }
}

fn in_range(atoms: u64) -> Option<Amount> {
    (atoms <= MAX_MONEY).then_some(Amount(atoms))
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} AVI", self.in_avi())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn the_first_reward_is_fifty_avi() {
        assert_eq!(subsidy(0), Amount::from_atoms(50 * ATOMS_PER_AVI).unwrap());
    }

    #[rstest]
    #[case::the_last_block_of_the_first_era(HALVING_INTERVAL - 1, 50)]
    #[case::the_first_of_the_second(HALVING_INTERVAL, 25)]
    #[case::the_third(2 * HALVING_INTERVAL, 12)]
    fn the_reward_halves_on_the_interval(#[case] height: u32, #[case] avi: u64) {
        assert!(subsidy(height).atoms() / ATOMS_PER_AVI == avi);
    }

    #[test]
    fn the_reward_reaches_zero_after_thirty_three_halvings_and_stays_there() {
        let last_paying = 32 * HALVING_INTERVAL;

        assert_eq!(subsidy(last_paying), Amount::from_atoms(1).unwrap());
        assert_eq!(subsidy(33 * HALVING_INTERVAL), Amount::ZERO);
        assert_eq!(subsidy(u32::MAX), Amount::ZERO);
    }

    /// Not a rule anything enforces — ADR-0006 is explicit that supply is
    /// emergent. This is the derivation, so a change to the interval or the
    /// initial reward cannot quietly change the cap.
    #[test]
    fn the_whole_schedule_sums_to_the_cap_the_adr_derives() {
        let total: u64 = (0..64)
            .map(|halvings| INITIAL_SUBSIDY >> halvings)
            .take_while(|reward| *reward > 0)
            .map(|reward| reward * HALVING_INTERVAL as u64)
            .sum();

        assert_eq!(total, 201_599_999_778_240);
        assert!(total <= MAX_MONEY);
        assert_eq!(total / ATOMS_PER_AVI, 2_015_999);
    }

    #[test]
    fn a_constant_amount_is_the_atoms_it_names() {
        assert_eq!(Amount::constant(546), Amount::from_atoms(546).unwrap());
        assert_eq!(Amount::constant(MAX_MONEY).atoms(), MAX_MONEY);
    }

    #[test]
    fn max_money_is_the_halving_series_and_is_a_legal_amount() {
        assert_eq!(MAX_MONEY, 201_600_000_000_000);
        assert!(Amount::from_atoms(MAX_MONEY).is_ok());
    }

    #[test]
    fn an_amount_above_max_money_cannot_be_constructed() {
        assert!(Amount::from_atoms(MAX_MONEY + 1).is_err());
        assert!(Amount::from_atoms(u64::MAX).is_err());
    }

    #[test]
    fn a_sum_that_leaves_the_range_is_none() {
        let half = Amount::from_atoms(MAX_MONEY / 2 + 1).unwrap();

        assert_eq!(half.checked_add(half), None);
    }

    #[test]
    fn a_sum_that_stays_in_range_is_the_sum() {
        let half = Amount::from_atoms(MAX_MONEY / 2).unwrap();

        assert_eq!(half.checked_add(half), Amount::from_atoms(MAX_MONEY).ok());
    }

    #[test]
    fn subtracting_more_than_is_there_is_none() {
        let one = Amount::from_atoms(1).unwrap();

        assert_eq!(Amount::ZERO.checked_sub(one), None);
    }

    #[test]
    fn summing_an_empty_collection_is_zero() {
        assert_eq!(Amount::sum([]), Some(Amount::ZERO));
    }

    #[test]
    fn a_sum_stops_at_the_first_amount_that_leaves_the_range() {
        let most = Amount::from_atoms(MAX_MONEY).unwrap();
        let one = Amount::from_atoms(1).unwrap();

        assert_eq!(Amount::sum([most, one, most]), None);
    }

    #[rstest]
    #[case(0, "0.00000000 AVI")]
    #[case(1, "0.00000001 AVI")]
    #[case(ATOMS_PER_AVI, "1.00000000 AVI")]
    #[case(50 * ATOMS_PER_AVI + 5, "50.00000005 AVI")]
    #[case(MAX_MONEY, "2016000.00000000 AVI")]
    fn atoms_are_displayed_as_avi(#[case] atoms: u64, #[case] expected: &str) {
        assert_eq!(Amount::from_atoms(atoms).unwrap().to_string(), expected);
    }
}
