use crate::send::ask_status;
use anyhow::{bail, Context, Result};
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
pub fn health(api: SocketAddr, stall: u64, marker: &Path) -> Result<()> {
    let status = ask_status(api)?;
    let height = status["height"]
        .as_u64()
        .context("the node's answer has no height")?;
    let now = crate::util::now() as u64;

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
    /// deleted the marker.
    #[test]
    fn a_tip_that_moved_down_is_a_tip_that_moved() {
        let path = scratch("reorged");
        remember(&path, 412, 1_000);
        let (seen, _) = remembered(&fs::read_to_string(&path).unwrap()).unwrap();

        assert_ne!(410, seen, "a reorg is movement, not a stall");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_marker_that_is_not_one_is_no_memory_at_all() {
        assert_eq!(remembered("nonsense"), None);
        assert_eq!(remembered(""), None);
        assert_eq!(remembered("412"), None);
        assert_eq!(remembered("high early"), None);
    }
}
