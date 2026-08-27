use crate::params::Network;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const STAMP: &str = "network";

/// Where one node keeps everything it must still have tomorrow, and the proof
/// that everything already there was written by a node on the same chain.
///
/// Every node owns one. That is what lets several run on one host — the local
/// network and the functional suite both depend on it — and it is why nothing
/// below reaches outside the path it was given.
#[derive(Debug)]
pub struct DataDir {
    path: PathBuf,
}

impl DataDir {
    /// Creates the directory if it is not there, stamps it with the network
    /// that built it, and refuses one stamped by another.
    ///
    /// [ADR-0007](../docs/adr/0007-genesis-and-network-parameters.md) separates
    /// the networks by their genesis hash; this is the same separation applied
    /// to disk, so two chains cannot be merged by pointing a node at the wrong
    /// directory.
    pub fn open(path: impl Into<PathBuf>, network: Network) -> Result<DataDir> {
        let path = path.into();
        fs::create_dir_all(&path)
            .with_context(|| format!("could not create the data directory {}", path.display()))?;

        let stamp = path.join(STAMP);
        let ours = stamp_for(network)?;

        if let Some(theirs) = read_stamp(&stamp)? {
            if theirs != ours {
                bail!(
                    "{} was built by a node on the {} network, and this one is on {}",
                    path.display(),
                    described(&theirs),
                    described(&ours)
                );
            }
        }

        // Written every run, not only the first. Creating a file and renaming
        // it over the stamp needs write permission on the *directory*, which
        // truncating the stamp in place would not — and every ticket after
        // this one creates files here.
        replace(&stamp, &ours)
            .with_context(|| format!("could not write to the data directory {}", path.display()))?;

        Ok(DataDir { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

/// Writes through a temporary name so the directory is never left unstamped by
/// a crash mid-write, and so the check above is a check on the directory.
fn replace(path: &Path, content: &str) -> Result<()> {
    let staged = path.with_extension("new");
    fs::write(&staged, content)?;
    fs::rename(&staged, path)?;
    Ok(())
}

/// The name is for the reader; the genesis hash is what actually decides. Two
/// networks could share a name across a rename, but not a genesis.
fn stamp_for(network: Network) -> Result<String> {
    Ok(format!(
        "{}\n{}\n",
        network.name,
        hex::encode(network.genesis_hash()?)
    ))
}

fn described(stamp: &str) -> &str {
    stamp.lines().next().unwrap_or("unknown").trim()
}

fn read_stamp(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path = std::env::temp_dir().join(format!("avicoin-{name}-{}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_fresh_directory_is_created_and_stamped() {
        let scratch = Scratch::new("fresh");

        let dir = DataDir::open(&scratch.0, &MAINNET).unwrap();

        assert!(dir.path().is_dir());
        assert!(fs::read_to_string(dir.join(STAMP))
            .unwrap()
            .contains(&hex::encode(MAINNET.genesis_hash().unwrap())));
    }

    #[test]
    fn the_same_node_opens_its_own_directory_again() {
        let scratch = Scratch::new("reopen");
        DataDir::open(&scratch.0, &MAINNET).unwrap();

        DataDir::open(&scratch.0, &MAINNET).expect("a node must be able to restart");
    }

    #[test]
    fn a_directory_built_by_another_network_is_refused_naming_both() {
        let scratch = Scratch::new("mismatch");
        DataDir::open(&scratch.0, &TESTNET).unwrap();

        let error = format!(
            "{:#}",
            DataDir::open(&scratch.0, &MAINNET)
                .expect_err("two chains must not be merged by a wrong path")
        );

        assert!(error.contains("test"), "{error}");
        assert!(error.contains("main"), "{error}");
        assert!(error.contains(&scratch.0.display().to_string()), "{error}");
    }

    /// The name alone would let a rename smuggle one chain into the other's
    /// directory, so the hash is what the comparison turns on.
    #[test]
    fn a_stamp_that_agrees_on_the_name_but_not_the_genesis_is_refused() {
        let scratch = Scratch::new("forged");
        fs::create_dir_all(&scratch.0).unwrap();
        fs::write(scratch.0.join(STAMP), format!("main\n{}\n", "0".repeat(64))).unwrap();

        DataDir::open(&scratch.0, &MAINNET)
            .expect_err("a stamp naming the right network but the wrong chain is still wrong");
    }

    #[test]
    fn two_nodes_with_different_directories_do_not_share_anything() {
        let one = Scratch::new("one");
        let two = Scratch::new("two");

        let first = DataDir::open(&one.0, &MAINNET).unwrap();
        let second = DataDir::open(&two.0, &TESTNET).unwrap();

        assert_ne!(first.path(), second.path());
        assert_ne!(
            fs::read_to_string(first.join(STAMP)).unwrap(),
            fs::read_to_string(second.join(STAMP)).unwrap()
        );
    }

    #[test]
    fn a_path_that_cannot_be_a_directory_is_an_error_naming_it() {
        let scratch = Scratch::new("a-file");
        fs::create_dir_all(scratch.0.parent().unwrap()).ok();
        fs::write(&scratch.0, "not a directory").unwrap();

        let error = format!("{:#}", DataDir::open(&scratch.0, &MAINNET).unwrap_err());

        assert!(error.contains(&scratch.0.display().to_string()), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_written_is_an_error_naming_it() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("read-only");
        DataDir::open(&scratch.0, &MAINNET).unwrap();
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o500)).unwrap();

        let outcome = DataDir::open(&scratch.0, &MAINNET);

        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o700)).unwrap();
        let error = format!(
            "{:#}",
            outcome.expect_err("root aside, this cannot be used")
        );
        assert!(error.contains(&scratch.0.display().to_string()), "{error}");
    }
}
