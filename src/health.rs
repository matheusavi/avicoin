use crate::send::ask_status;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;

/// Whether the node is *working*, which is not the same as up.
///
/// A node whose miner has wedged, or which has lost every peer, answers
/// `/status` perfectly well and is not doing its job. So the question is
/// whether the tip has moved, and answering it needs a memory: the height last
/// seen and when it was seen.
///
/// The marker lives in the container rather than the data directory. A
/// restarted container should start the clock again rather than inherit a
/// verdict about the node it replaced, and nothing but the node should write
/// to the node's directory.
pub fn health(api: SocketAddr, stall: Option<u64>, marker: &Path) -> Result<()> {
    let status = match ask_status(api) {
        Ok(status) => status,
        // A node too busy to answer *is* answering. The API's own backpressure
        // is not the node failing, and three of those in a row would take a
        // working container down.
        Err(why) if format!("{why:#}").contains("the API is busy") => return Ok(()),
        Err(why) => return Err(why),
    };

    let height = status["height"]
        .as_u64()
        .context("the node's answer has no height")?;
    let stall = match stall {
        Some(given) => given,
        None => forty_blocks(&status),
    };

    verdict(height, stall, marker, crate::util::now() as u64)
}

/// How long a tip may stand still, in this network's terms rather than in
/// seconds somebody picked: forty blocks. Long enough that an unlucky stretch
/// of mining is not an alarm, short enough that a wedged node is noticed
/// within a few minutes — and it means the same thing on a chain that wants a
/// block every second as on one that wants one every thirty.
fn forty_blocks(status: &Value) -> u64 {
    let spacing = status["network"]
        .as_str()
        .and_then(|name| crate::params::by_name(name).ok())
        .map(|network| network.target_block_time)
        .unwrap_or(30);

    40 * spacing as u64
}

/// Split from `health` so the decision can be tested without a node — the
/// reorg case below is one no functional test can stage.
fn verdict(height: u64, stall: u64, marker: &Path, now: u64) -> Result<()> {
    let (seen, since) = match fs::read_to_string(marker)
        .ok()
        .and_then(|text| remembered(&text))
    {
        Some(remembered) => remembered,
        // Nothing remembered: the node answered, and a first look cannot tell
        // a stalled chain from a young one.
        None => {
            remember(marker, height, now);
            return Ok(());
        }
    };

    // Moved, not risen. A reorg lowers the tip, and a node that had just done
    // one would otherwise be called unhealthy for ever after — `height > seen`
    // can never become true again once the marker is above it.
    if height != seen {
        remember(marker, height, now);
        return Ok(());
    }

    let standing = now.saturating_sub(since);
    if standing > stall {
        bail!("the tip has stood at {height} for {standing}s, past {stall}s");
    }

    Ok(())
}

fn remembered(text: &str) -> Option<(u64, u64)> {
    let (height, since) = text.trim().split_once(' ')?;

    Some((height.parse().ok()?, since.parse().ok()?))
}

/// Best effort. A marker that cannot be written makes every check look like a
/// first one, which reports healthy — and a healthcheck that failed the
/// container over its own scratch file would be worse than one that is blind.
fn remember(marker: &Path, height: u64, now: u64) {
    let _ = fs::write(marker, format!("{height} {now}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("avicoin-health-{name}-{}", std::process::id()));
        fs::remove_file(&path).ok();
        path
    }

    #[test]
    fn a_marker_round_trips() {
        let path = scratch("round-trip");
        remember(&path, 412, 1_000);

        assert_eq!(
            remembered(&fs::read_to_string(&path).unwrap()),
            Some((412, 1_000))
        );
        fs::remove_file(&path).ok();
    }

    /// A reorg lowers the tip. `height > seen` would be false for ever after,
    /// so a node that had just reorganised would be unhealthy until somebody
    /// deleted the marker — and no functional test can stage a reorg deep
    /// enough to sit still afterwards.
    #[test]
    fn a_tip_that_moved_down_is_a_tip_that_moved() {
        let path = scratch("reorged");
        remember(&path, 412, 1_000);

        assert!(
            verdict(410, 0, &path, 9_999).is_ok(),
            "a reorg is movement, not a stall"
        );
        assert_eq!(
            remembered(&fs::read_to_string(&path).unwrap()),
            Some((410, 9_999)),
            "and the clock starts again from where it moved to"
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_tip_that_stood_still_past_the_window_is_unhealthy() {
        let path = scratch("stalled");
        remember(&path, 412, 1_000);

        assert!(verdict(412, 10, &path, 1_009).is_ok(), "inside the window");
        assert!(
            verdict(412, 10, &path, 1_011).is_err(),
            "past it, and the tip has not moved"
        );
    }

    #[test]
    fn a_first_look_cannot_tell_a_stalled_chain_from_a_young_one() {
        let path = scratch("first");
        fs::remove_file(&path).ok();

        assert!(verdict(0, 0, &path, 1_000).is_ok());
        assert_eq!(
            remembered(&fs::read_to_string(&path).unwrap()),
            Some((0, 1_000))
        );
        fs::remove_file(&path).ok();
    }

    /// A network wanting a block a second and one wanting one every thirty
    /// should not share a number of seconds.
    #[test]
    fn the_window_is_forty_of_this_networks_block_times() {
        use crate::params::{MAINNET, TESTNET};

        assert_eq!(
            forty_blocks(&serde_json::json!({"network": "main"})),
            40 * MAINNET.target_block_time as u64
        );
        assert_eq!(
            forty_blocks(&serde_json::json!({"network": "test"})),
            40 * TESTNET.target_block_time as u64
        );
        assert_eq!(
            forty_blocks(&serde_json::json!({})),
            40 * MAINNET.target_block_time as u64,
            "a node that did not say falls back to the public chain's"
        );
    }

    #[test]
    fn a_marker_that_is_not_one_is_no_memory_at_all() {
        assert_eq!(remembered("nonsense"), None);
        assert_eq!(remembered(""), None);
        assert_eq!(remembered("412"), None);
        assert_eq!(remembered("high early"), None);
    }
}
