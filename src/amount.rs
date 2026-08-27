use anyhow::{anyhow, Result};
use std::fmt;

pub const ATOMS_PER_AVI: u64 = 100_000_000;
pub const MAX_MONEY: u64 = 2_016_000 * ATOMS_PER_AVI;

/// An amount of coin, counted in atoms and never outside `0..=MAX_MONEY`.
///
/// The bound is an invariant of the type rather than a check callers remember,
/// so a sum of any number of `Amount`s cannot approach `u64`'s ceiling. The
/// arithmetic is checked anyway: ADR-0006's whole lesson is that resting
/// correctness on a bound being right is the reasoning that failed in 2010.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Amount(u64);

impl Amount {
    pub const ZERO: Amount = Amount(0);

    pub fn from_atoms(atoms: u64) -> Result<Amount> {
        if atoms > MAX_MONEY {
            return Err(anyhow!("{atoms} atoms is above MAX_MONEY ({MAX_MONEY})"));
        }

        Ok(Amount(atoms))
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
        write!(
            f,
            "{}.{:08} AVI",
            self.0 / ATOMS_PER_AVI,
            self.0 % ATOMS_PER_AVI
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

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
    fn a_sum_that_leaves_the_range_is_none_rather_than_a_wrapped_amount() {
        let half = Amount::from_atoms(MAX_MONEY / 2 + 1).unwrap();

        assert_eq!(half.checked_add(half), None);
    }

    #[test]
    fn a_sum_that_stays_in_range_is_the_sum() {
        let half = Amount::from_atoms(MAX_MONEY / 2).unwrap();

        assert_eq!(half.checked_add(half), Amount::from_atoms(MAX_MONEY).ok());
    }

    #[test]
    fn subtracting_more_than_is_there_is_none_rather_than_a_wrapped_amount() {
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
