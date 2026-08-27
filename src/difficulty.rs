use crate::block::{bits_from_target, target_from_bits};
use crate::params::Network;
use anyhow::{anyhow, bail, Result};
use primitive_types::{U256, U512};

/// ADR-0009 fixes that difficulty is recomputed **every block** from a moving
/// window and leaves the numbers to the implementation. These are them.
///
/// The window is 60 because block intervals are exponentially distributed:
/// the relative standard error is about `1/√60`, so difficulty jitters by
/// roughly 13% at constant hashrate. Much shorter oscillates; much longer
/// readmits the death spiral the per-block scheme exists to close.
///
/// The clamp is a factor of 2 per block, which is far outside that jitter and
/// still absorbs a 1000× change in hashrate in ten blocks — and, more to the
/// point, in about seventeen hours of wall clock when the change is a
/// departure and every block in between is slow.
pub const RETARGET_WINDOW: usize = 60;
pub const MAX_RETARGET_FACTOR: u32 = 2;

/// A block's timestamp must exceed the median of the previous eleven. A median
/// absorbs a far-future outlier; "later than its parent" would let one ratchet
/// the floor permanently, and per-block retarget makes timestamps load-bearing.
pub const MEDIAN_TIME_SPAN: usize = 11;

/// Bitcoin allows two hours, a 2009 artifact of unreliable clocks. At
/// thirty-second blocks that is 240 block times — four times the whole window,
/// and exactly the lever ADR-0009 closes. Five minutes is ten block times:
/// ample for ordinary skew, far too small a fraction of the window to steer.
pub const MAX_FUTURE_DRIFT: u32 = 5 * 60;

// Times are `u32` seconds, as Bitcoin's are, so the header format runs out in
// 2106. Widening the field is a hard fork either way, and inheriting the
// deadline is cheaper than inventing a different one.

/// The `n_bits` a block must state, given the timestamps of the blocks up to
/// and including its parent, oldest first, and the parent's own `n_bits`.
///
/// This computes the rule; refusing a block that states something else is
/// block validation's job, not this module's.
///
/// Fewer than two timestamps means there is no interval to measure — genesis
/// and its child — so the parent's target stands.
pub fn required_bits(timestamps: &[u32], parent_bits: u32, network: Network) -> Result<u32> {
    let limit = target_from_bits(network.starting_bits)?;
    let parent = target_from_bits(parent_bits)?;

    let window = &timestamps[timestamps.len().saturating_sub(RETARGET_WINDOW + 1)..];
    // Under two, there is no interval to measure and `expected` would be zero.
    if window.len() < 2 {
        return Ok(bits_from_target(parent.min(limit)));
    }
    let (first, last) = (window[0], window[window.len() - 1]);

    let intervals = (window.len() - 1) as u32;
    let expected = intervals * network.target_block_time;
    // Median-time-past does not make timestamps monotonic across a window, so
    // a span can come out negative. Saturating to zero then clamping up is the
    // same answer as "blocks arrived as fast as the clamp allows".
    let observed = last.saturating_sub(first).clamp(
        expected / MAX_RETARGET_FACTOR,
        expected.saturating_mul(MAX_RETARGET_FACTOR),
    );

    // In 512 bits: the multiply overflows 256 for any target near the top of
    // the range, and the test network's starting target is exactly there.
    let scaled = U512::from(parent) * U512::from(observed) / U512::from(expected);
    let next = U256::try_from(scaled).unwrap_or(limit);

    Ok(bits_from_target(next.min(limit)))
}

/// The median of the previous `MEDIAN_TIME_SPAN` timestamps, or of however many
/// there are near genesis. `None` only when there are none at all.
pub fn median_time_past(timestamps: &[u32]) -> Option<u32> {
    let mut recent: Vec<u32> = timestamps[timestamps.len().saturating_sub(MEDIAN_TIME_SPAN)..]
        .iter()
        .copied()
        .collect();
    recent.sort_unstable();

    recent.get(recent.len() / 2).copied()
}

/// Whether a block may claim this time, given its ancestors' and ours.
///
/// A rejection on the future limit is the node's clock disagreeing with the
/// network's as often as it is a hostile block, and it presents as an
/// unexplained partition. ADR-0009 therefore asks that it be **logged
/// loudly**, which this cannot do — so `too_far_ahead` lets the caller tell
/// the two refusals apart, and block validation is where the logging lives.
pub fn check_timestamp(timestamp: u32, timestamps: &[u32], now: u32) -> Result<()> {
    if let Some(median) = median_time_past(timestamps) {
        if timestamp <= median {
            bail!("timestamp {timestamp} is not past the median of the last eleven, {median}");
        }
    }

    if too_far_ahead(timestamp, now) {
        return Err(anyhow!(
            "timestamp {timestamp} is more than {MAX_FUTURE_DRIFT}s past this node's clock ({now}); \
             check this machine's time before suspecting the network"
        ));
    }

    Ok(())
}

/// Which of the two timestamp refusals happened. The future limit is the one
/// a wrong local clock trips, so a caller logs it where it logs nothing else.
pub fn too_far_ahead(timestamp: u32, now: u32) -> bool {
    timestamp > now.saturating_add(MAX_FUTURE_DRIFT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};
    use rstest::rstest;

    const TARGET_BLOCK_TIME: u32 = MAINNET.target_block_time;

    /// `count` blocks arriving every `interval` seconds, ending at `t0`.
    fn arriving_every(interval: u32, count: usize) -> Vec<u32> {
        (0..count as u32)
            .map(|n| 1_000_000 + n * interval)
            .collect()
    }

    fn target_of(bits: u32) -> U256 {
        target_from_bits(bits).unwrap()
    }

    /// Well inside mainnet's floor, so a test can move difficulty a long way
    /// in both directions without the floor being what stops it.
    const STEADY: u32 = 0x1a00ffff;

    #[test]
    fn a_steady_chain_keeps_the_difficulty_it_has() {
        let bits = required_bits(
            &arriving_every(TARGET_BLOCK_TIME, RETARGET_WINDOW + 1),
            STEADY,
            &MAINNET,
        )
        .unwrap();

        assert_eq!(bits, STEADY);
    }

    #[test]
    fn hashrate_arriving_makes_the_next_block_harder() {
        let bits = required_bits(
            &arriving_every(TARGET_BLOCK_TIME / 3, RETARGET_WINDOW + 1),
            STEADY,
            &MAINNET,
        )
        .unwrap();

        assert!(
            target_of(bits) < target_of(STEADY),
            "a smaller target is more work"
        );
    }

    #[test]
    fn hashrate_leaving_makes_the_next_block_easier() {
        let bits = required_bits(
            &arriving_every(TARGET_BLOCK_TIME * 3, RETARGET_WINDOW + 1),
            STEADY,
            &MAINNET,
        )
        .unwrap();

        assert!(target_of(bits) > target_of(STEADY));
    }

    #[test]
    fn one_anomalous_interval_is_clamped_rather_than_swinging_the_target() {
        let mut timestamps = arriving_every(TARGET_BLOCK_TIME, RETARGET_WINDOW + 1);
        *timestamps.last_mut().unwrap() += 100 * TARGET_BLOCK_TIME * RETARGET_WINDOW as u32;

        let bits = required_bits(&timestamps, STEADY, &MAINNET).unwrap();

        assert!(
            target_of(bits) <= target_of(STEADY) * MAX_RETARGET_FACTOR,
            "the clamp is what stops one timestamp moving the target far"
        );
    }

    /// The property ADR-0009 states as its intent, measured rather than
    /// asserted: hashrate leaves and difficulty has to catch up.
    #[test]
    fn a_thousandfold_change_is_absorbed_in_tens_of_blocks() {
        let mut bits = STEADY;
        let slowed = TARGET_BLOCK_TIME * 1_000;
        let mut blocks = 0;

        while target_of(bits) < target_of(STEADY) * 1_000u32 {
            bits = required_bits(&arriving_every(slowed, RETARGET_WINDOW + 1), bits, &MAINNET)
                .unwrap();
            blocks += 1;
            assert!(blocks < 100, "still {blocks} blocks and not there yet");
        }

        assert!(blocks >= 10, "a clamp that loose is not a clamp: {blocks}");
        assert!(blocks <= 20, "tens of blocks, not hundreds: {blocks}");
    }

    #[test]
    fn difficulty_never_falls_below_the_networks_floor() {
        let bits = required_bits(
            &arriving_every(TARGET_BLOCK_TIME * 1_000, RETARGET_WINDOW + 1),
            TESTNET.starting_bits,
            &TESTNET,
        )
        .unwrap();

        assert_eq!(bits, TESTNET.starting_bits);
    }

    #[rstest]
    #[case::nothing_at_all(0)]
    #[case::genesis_alone(1)]
    #[case::a_short_window(5)]
    #[case::one_short_of_the_window(RETARGET_WINDOW)]
    fn a_chain_shorter_than_the_window_measures_what_it_has(#[case] blocks: usize) {
        let bits =
            required_bits(&arriving_every(TARGET_BLOCK_TIME, blocks), STEADY, &MAINNET).unwrap();

        assert_eq!(
            bits, STEADY,
            "blocks arriving on time change nothing, however few of them there are"
        );
    }

    #[test]
    fn a_window_that_runs_backwards_reads_as_fast_rather_than_panicking() {
        let mut backwards = arriving_every(TARGET_BLOCK_TIME, RETARGET_WINDOW + 1);
        // Legal: median-time-past constrains a block against its ancestors'
        // median, not against the block before it.
        *backwards.last_mut().unwrap() = backwards[0] - 1;

        let bits = required_bits(&backwards, STEADY, &MAINNET).unwrap();

        assert!(
            target_of(bits) < target_of(STEADY),
            "a span of nothing is the fastest the clamp allows, not an error"
        );
    }

    #[test]
    fn a_short_window_still_reacts_to_what_it_can_see() {
        let hurrying = required_bits(&arriving_every(1, 5), STEADY, &MAINNET).unwrap();

        assert!(
            target_of(hurrying) < target_of(STEADY),
            "five fast blocks are enough to raise difficulty; the rule is not asleep"
        );
    }

    #[test]
    fn the_future_limit_is_the_refusal_a_caller_singles_out() {
        let now = 1_000_000;

        assert!(too_far_ahead(now + MAX_FUTURE_DRIFT + 1, now));
        assert!(!too_far_ahead(now + MAX_FUTURE_DRIFT, now));
        assert!(
            !too_far_ahead(1, now),
            "a stale timestamp is a different rule"
        );
    }

    #[test]
    fn the_median_is_the_middle_of_the_last_eleven_however_they_arrived() {
        let jumbled = vec![50, 10, 40, 20, 30];

        assert_eq!(median_time_past(&jumbled), Some(30));
        assert_eq!(median_time_past(&[]), None);
    }

    #[test]
    fn the_median_ignores_everything_before_the_last_eleven() {
        let mut long = vec![0; 100];
        long.extend([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);

        assert_eq!(median_time_past(&long), Some(6));
    }

    #[test]
    fn a_timestamp_must_be_past_the_median_and_one_second_is_enough() {
        let previous: Vec<u32> = (1..=11).collect();

        assert!(check_timestamp(6, &previous, 1_000).is_err());
        assert!(check_timestamp(7, &previous, 1_000).is_ok());
    }

    #[test]
    fn one_far_future_timestamp_does_not_ratchet_the_floor() {
        let mut previous: Vec<u32> = (1..=11).collect();
        previous[10] = 2_000_000_000;

        assert!(
            check_timestamp(7, &previous, 2_000_000_000).is_ok(),
            "a median absorbs the outlier where a maximum would not"
        );
    }

    #[test]
    fn a_block_from_too_far_in_the_future_is_refused() {
        let now = 1_000_000;

        assert!(check_timestamp(now + MAX_FUTURE_DRIFT, &[], now).is_ok());
        assert!(check_timestamp(now + MAX_FUTURE_DRIFT + 1, &[], now).is_err());
    }

    #[test]
    fn the_future_limit_says_to_suspect_the_clock_before_the_network() {
        let refusal = format!(
            "{:#}",
            check_timestamp(2_000_000, &[], 1_000_000).unwrap_err()
        );

        assert!(refusal.contains("clock"), "{refusal}");
    }
}
