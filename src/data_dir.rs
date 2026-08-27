use crate::params::Network;
use crate::util::display_order;
use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

const STAMP: &str = "network";
const LOCK: &str = "lock";

/// One node's directory, held exclusively for as long as the node runs.
/// [ADR-0013](../docs/adr/0013-persistence.md) has the reasons.
#[derive(Debug)]
pub struct DataDir {
    path: PathBuf,
    /// One node per directory, enforced rather than asserted. Dropping this
    /// releases the lock, so it lives as long as the `DataDir` does.
    _lock: File,
}

impl DataDir {
    /// Creates the directory if it is absent, takes it exclusively, and
    /// stamps it with the network that built it — refusing one another network
    /// stamped.
    pub fn open(path: impl Into<PathBuf>, network: Network) -> Result<DataDir> {
        let path = path.into();
        create(&path)
            .with_context(|| format!("could not create the data directory {}", path.display()))?;

        // Taken before the stamp is read, not after: the check and the write
        // that follows it are one operation, and two nodes racing on a fresh
        // directory would otherwise both pass the check.
        let lock = claim(&path)?;

        writable_only_by_us(&path)?;

        let stamp = path.join(STAMP);
        let ours = stamp_for(network)?;

        if let Some(theirs) = read_stamp(&stamp)? {
            if theirs.trim() != ours.trim() {
                bail!("{} {}", path.display(), disagreement(&theirs, &ours));
            }
        }

        // Written every run, not only the first: the rename needs write
        // permission on the *directory*, which truncating the stamp in place
        // would not, and every file M5 adds here needs that permission.
        replace(&stamp, &ours)
            .with_context(|| format!("could not write to the data directory {}", path.display()))?;

        Ok(DataDir { path, _lock: lock })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A directory `0700` on creation says nothing about one that was already
/// there. Anyone who can write here can unlink the wallet key and leave their
/// own — which is `0600` and therefore passes every check the key itself
/// makes, while every block is mined to somebody else's address.
#[cfg(unix)]
fn writable_only_by_us(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        bail!(
            "{} is mode {mode:o} — a data directory anyone else can write to is one \
             they can put their own wallet key in",
            path.display()
        );
    }

    Ok(())
}

#[cfg(not(unix))]
fn writable_only_by_us(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    // 0700 from the start: the wallet key lands here in its own ticket, and a
    // key at 0600 inside a world-readable directory is a smaller promise than
    // it looks.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// The lock is advisory and process-wide, held by the open file rather than by
/// its contents — so a node that dies takes its claim with it, and no stale
/// lock file has to be cleaned up by hand.
fn claim(path: &Path) -> Result<File> {
    let at = path.join(LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&at)
        .with_context(|| format!("could not open {}", at.display()))?;

    file.try_lock().map_err(|_| {
        anyhow::anyhow!(
            "another node is already running on {} — one node per directory",
            path.display()
        )
    })?;

    Ok(file)
}

fn replace(path: &Path, content: &str) -> Result<()> {
    let staged = path.with_extension("new");
    fs::write(&staged, content)?;
    fs::rename(&staged, path)?;
    Ok(())
}

fn stamp_for(network: Network) -> Result<String> {
    Ok(format!(
        "{}\n{}\n",
        network.name,
        hex::encode(display_order(network.genesis_hash()?))
    ))
}

/// A stamp naming the same network but a different genesis is not a wrong
/// turn, it is a directory the parameters were edited under. Saying "built on
/// main, and this one is on main" would send a reader looking for the wrong
/// mistake.
fn disagreement(theirs: &str, ours: &str) -> String {
    let (their_name, our_name) = (named(theirs), named(ours));

    if their_name == our_name {
        format!(
            "was built on a different {our_name} chain, whose genesis is {} rather than {}",
            hashed(theirs),
            hashed(ours)
        )
    } else {
        format!("was built by a node on the {their_name} network, and this one is on {our_name}")
    }
}

fn named(stamp: &str) -> &str {
    line(stamp, 0)
}

fn hashed(stamp: &str) -> &str {
    line(stamp, 1)
}

fn line(stamp: &str, index: usize) -> &str {
    stamp.lines().nth(index).unwrap_or("unknown").trim()
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
            clear(&path);
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            clear(&self.0);
        }
    }

    /// One of these paths is deliberately a regular file, which
    /// `remove_dir_all` cannot touch.
    fn clear(path: &Path) {
        fs::remove_dir_all(path).ok();
        fs::remove_file(path).ok();
    }

    #[test]
    fn a_fresh_directory_is_created_and_stamped() {
        let scratch = Scratch::new("fresh");

        let dir = DataDir::open(&scratch.0, &MAINNET).unwrap();

        assert!(dir.path().is_dir());
        assert_eq!(
            fs::read_to_string(dir.path().join(STAMP))
                .unwrap()
                .lines()
                .next(),
            Some("main")
        );
    }

    #[test]
    fn the_same_node_opens_its_own_directory_again() {
        let scratch = Scratch::new("reopen");
        drop(DataDir::open(&scratch.0, &MAINNET).unwrap());

        DataDir::open(&scratch.0, &MAINNET).expect("a node must be able to restart");
    }

    /// A stamp someone opened in an editor, or a file copied through something
    /// that trims, is still this network's stamp.
    #[test]
    fn a_stamp_missing_its_trailing_newline_is_still_ours() {
        let scratch = Scratch::new("trimmed");
        drop(DataDir::open(&scratch.0, &MAINNET).unwrap());
        let stamp = scratch.0.join(STAMP);
        let trimmed = fs::read_to_string(&stamp).unwrap().trim().to_string();
        fs::write(&stamp, trimmed).unwrap();

        DataDir::open(&scratch.0, &MAINNET).expect("whitespace is not a network");
    }

    #[test]
    fn a_directory_built_by_another_network_is_refused_naming_both() {
        let scratch = Scratch::new("mismatch");
        drop(DataDir::open(&scratch.0, &TESTNET).unwrap());

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
    /// directory, so the hash is what the comparison turns on — and a
    /// disagreement about it has to read as one, not as "main is not main".
    #[test]
    fn a_stamp_that_agrees_on_the_name_but_not_the_genesis_is_refused() {
        let scratch = Scratch::new("forged");
        fs::create_dir_all(&scratch.0).unwrap();
        let theirs = "0".repeat(64);
        fs::write(scratch.0.join(STAMP), format!("main\n{theirs}\n")).unwrap();

        let error = format!("{:#}", DataDir::open(&scratch.0, &MAINNET).unwrap_err());

        assert!(error.contains("a different main chain"), "{error}");
        assert!(error.contains(&theirs), "{error}");
    }

    /// Every hash a person reads is big-endian — invariant 5 — and a stamp is
    /// read by a person more often than by anything else.
    #[test]
    fn the_stamped_genesis_is_the_one_the_node_prints() {
        let scratch = Scratch::new("display-order");
        let dir = DataDir::open(&scratch.0, &MAINNET).unwrap();

        let stamp = fs::read_to_string(dir.path().join(STAMP)).unwrap();

        assert!(
            stamp.contains(&hex::encode(display_order(MAINNET.genesis_hash().unwrap()))),
            "{stamp}"
        );
    }

    #[test]
    fn a_second_node_cannot_take_a_directory_a_first_one_holds() {
        let scratch = Scratch::new("contended");
        let held = DataDir::open(&scratch.0, &MAINNET).unwrap();

        let error = format!(
            "{:#}",
            DataDir::open(&scratch.0, &MAINNET)
                .expect_err("one node per directory, enforced rather than asserted")
        );

        assert!(error.contains("one node per directory"), "{error}");
        drop(held);
        DataDir::open(&scratch.0, &MAINNET).expect("the claim goes when the node does");
    }

    /// A fresh directory is the case a check followed by a write cannot
    /// survive on its own: neither node reads a stamp, so both would write one.
    #[test]
    fn two_networks_cannot_both_claim_one_fresh_directory() {
        let scratch = Scratch::new("race");
        let first = DataDir::open(&scratch.0, &MAINNET);

        let second = DataDir::open(&scratch.0, &TESTNET);

        assert!(first.is_ok());
        assert!(
            second.is_err(),
            "two networks must not both believe they own one directory"
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
    fn a_directory_anyone_can_write_to_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("world-writable");
        fs::create_dir_all(&scratch.0).unwrap();
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o777)).unwrap();

        let error = format!(
            "{:#}",
            DataDir::open(&scratch.0, &MAINNET)
                .expect_err("a key is only as private as the directory holding it")
        );

        assert!(error.contains("777"), "{error}");
        assert!(error.contains(&scratch.0.display().to_string()), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_written_is_an_error_naming_it() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("read-only");
        drop(DataDir::open(&scratch.0, &MAINNET).unwrap());
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
