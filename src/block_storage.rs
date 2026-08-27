use crate::params::Network;
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The magic, then a four-byte little-endian length, then that many bytes.
const FRAME_HEADER: u64 = 8;

/// A record cannot be larger than a block, and a block is already capped at
/// `MAX_BLOCK_SIZE`. Twice that leaves an undo record — which carries a whole
/// `TxOut` per input the block spent — room to be larger than the block that
/// produced it, without letting a length prefix ask for an arbitrary buffer.
pub const MAX_RECORD: u32 = 2 * crate::validation::MAX_BLOCK_SIZE as u32;

/// An append-only file of framed records, addressed by the offset an append
/// returns.
///
/// Blocks are write-once, bulky, and read by offset, which is what a flat file
/// suits and a key-value store does not ([ADR-0013](../docs/adr/0013-persistence.md)).
/// `blocks.dat` and `undo.dat` are two of these and share nothing, so a torn
/// write in one costs the other nothing.
#[derive(Debug)]
pub struct RecordFile {
    file: File,
    path: PathBuf,
    magic: [u8; 4],
    end: u64,
}

impl RecordFile {
    /// Opens the file, creating it if it is absent, and truncates it to the
    /// last record that reads back whole.
    ///
    /// A crash mid-append leaves a record with no length, a length with no
    /// body, or a body cut short. All three end the readable region here, and
    /// the cost of the crash is the record that was in flight.
    pub fn open(path: impl Into<PathBuf>, network: Network) -> Result<RecordFile> {
        let path = path.into();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("could not open {}", path.display()))?;

        let end = readable_end(&mut file, network.magic)
            .with_context(|| format!("could not read {}", path.display()))?;

        file.set_len(end)
            .with_context(|| format!("could not truncate {}", path.display()))?;
        file.seek(SeekFrom::Start(end))?;

        Ok(RecordFile {
            file,
            path,
            magic: network.magic,
            end,
        })
    }

    /// Returns the offset the record was written at, which is how it is read
    /// back. Nothing else addresses a record.
    pub fn append(&mut self, record: &[u8]) -> Result<u64> {
        let length: u32 = record
            .len()
            .try_into()
            .ok()
            .filter(|&length| length <= MAX_RECORD)
            .with_context(|| format!("a record of {} bytes is past MAX_RECORD", record.len()))?;

        let at = self.end;
        let mut framed = Vec::with_capacity(record.len() + FRAME_HEADER as usize);
        framed.extend_from_slice(&self.magic);
        framed.extend_from_slice(&length.to_le_bytes());
        framed.extend_from_slice(record);

        self.file
            .write_all(&framed)
            .with_context(|| format!("could not append to {}", self.path.display()))?;

        self.end = at + framed.len() as u64;
        Ok(at)
    }

    pub fn read_at(&mut self, at: u64) -> Result<Vec<u8>> {
        let (record, _) = read_frame(&mut self.file, at, self.end, self.magic)?
            .with_context(|| format!("{} holds no record at {at}", self.path.display()))?;
        Ok(record)
    }

    /// Everything on disk gets to durability before the index that points at
    /// it. The order is [ADR-0013](../docs/adr/0013-persistence.md)'s, and the
    /// caller's to keep.
    pub fn sync(&self) -> Result<()> {
        self.file
            .sync_data()
            .with_context(|| format!("could not flush {}", self.path.display()))
    }

    pub fn end(&self) -> u64 {
        self.end
    }
}

/// Walks the frames from the start and returns where the last whole one ends.
/// Anything after that is what a crash left behind.
fn readable_end(file: &mut File, magic: [u8; 4]) -> Result<u64> {
    let size = file.seek(SeekFrom::End(0))?;
    let mut end = 0;

    while let Some((_, next)) = read_frame(file, end, size, magic)? {
        end = next;
    }

    Ok(end)
}

/// `Ok(None)` where the readable region ends — a short frame, a length that
/// overruns the file, or magic that is not ours. An error is a failed read,
/// which is a different thing and must not be mistaken for a torn write.
fn read_frame(
    file: &mut File,
    at: u64,
    size: u64,
    magic: [u8; 4],
) -> Result<Option<(Vec<u8>, u64)>> {
    if at.saturating_add(FRAME_HEADER) > size {
        return Ok(None);
    }

    let mut header = [0u8; FRAME_HEADER as usize];
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(&mut header)?;

    if header[..4] != magic {
        return Ok(None);
    }

    let length = u32::from_le_bytes(header[4..].try_into().expect("four bytes"));
    // Checked before the allocation, not after it: a length prefix is the one
    // number on disk a torn write can make arbitrary, and `Vec::with_capacity`
    // would believe it.
    if length > MAX_RECORD {
        return Ok(None);
    }

    let ends_at = at + FRAME_HEADER + u64::from(length);
    if ends_at > size {
        return Ok(None);
    }

    let mut record = vec![0u8; length as usize];
    file.read_exact(&mut record)?;

    Ok(Some((record, ends_at)))
}

/// The two files a node keeps, opened together so a torn one is repaired
/// before anything reads from it.
#[derive(Debug)]
pub struct BlockFiles {
    pub blocks: RecordFile,
    pub undo: RecordFile,
}

impl BlockFiles {
    pub fn open(directory: &Path, network: Network) -> Result<BlockFiles> {
        Ok(BlockFiles {
            blocks: RecordFile::open(directory.join("blocks.dat"), network)?,
            undo: RecordFile::open(directory.join("undo.dat"), network)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{MAINNET, TESTNET};
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path =
                std::env::temp_dir().join(format!("avicoin-records-{name}-{}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn opened(scratch: &Scratch, name: &str) -> RecordFile {
        RecordFile::open(scratch.file(name), &MAINNET).unwrap()
    }

    #[test]
    fn a_record_reads_back_byte_for_byte_at_the_offset_the_append_returned() {
        let scratch = Scratch::new("round-trip");
        let mut file = opened(&scratch, "blocks.dat");
        let record = b"a block, more or less".to_vec();

        let at = file.append(&record).unwrap();

        assert_eq!(file.read_at(at).unwrap(), record);
    }

    #[test]
    fn several_records_read_back_independently_and_in_any_order() {
        let scratch = Scratch::new("several");
        let mut file = opened(&scratch, "blocks.dat");
        let records: Vec<Vec<u8>> = (0u8..8).map(|n| vec![n; 10 + n as usize]).collect();

        let offsets: Vec<u64> = records
            .iter()
            .map(|record| file.append(record).unwrap())
            .collect();

        for i in [5usize, 0, 7, 3, 1] {
            assert_eq!(file.read_at(offsets[i]).unwrap(), records[i]);
        }
    }

    #[test]
    fn records_survive_a_reopen_and_the_next_append_lands_after_them() {
        let scratch = Scratch::new("reopen");
        let (first, second) = {
            let mut file = opened(&scratch, "blocks.dat");
            (file.append(b"one").unwrap(), file.append(b"two").unwrap())
        };

        let mut reopened = opened(&scratch, "blocks.dat");
        let third = reopened.append(b"three").unwrap();

        assert_eq!(reopened.read_at(first).unwrap(), b"one");
        assert_eq!(reopened.read_at(second).unwrap(), b"two");
        assert_eq!(reopened.read_at(third).unwrap(), b"three");
    }

    #[test]
    fn a_corrupted_magic_ends_the_readable_region_and_the_file_truncates_to_it() {
        let scratch = Scratch::new("bad-magic");
        let (good, bad) = {
            let mut file = opened(&scratch, "blocks.dat");
            (file.append(b"kept").unwrap(), file.append(b"lost").unwrap())
        };

        let mut bytes = fs::read(scratch.file("blocks.dat")).unwrap();
        bytes[bad as usize] ^= 0xff;
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        let mut reopened = opened(&scratch, "blocks.dat");

        assert_eq!(reopened.read_at(good).unwrap(), b"kept");
        assert_eq!(reopened.end(), bad, "the file ends at the last good record");
        assert_eq!(fs::metadata(scratch.file("blocks.dat")).unwrap().len(), bad);
    }

    #[test]
    fn a_record_cut_short_by_a_crash_is_discarded() {
        let scratch = Scratch::new("torn");
        let (good, torn) = {
            let mut file = opened(&scratch, "blocks.dat");
            (
                file.append(b"kept").unwrap(),
                file.append(&[7u8; 64]).unwrap(),
            )
        };

        let bytes = fs::read(scratch.file("blocks.dat")).unwrap();
        fs::write(scratch.file("blocks.dat"), &bytes[..bytes.len() - 20]).unwrap();

        let mut reopened = opened(&scratch, "blocks.dat");

        assert_eq!(reopened.read_at(good).unwrap(), b"kept");
        assert_eq!(reopened.end(), torn);
    }

    #[test]
    fn a_frame_header_cut_in_half_is_discarded() {
        let scratch = Scratch::new("half-header");
        let torn = {
            let mut file = opened(&scratch, "blocks.dat");
            file.append(b"kept").unwrap();
            file.end()
        };

        let bytes = fs::read(scratch.file("blocks.dat")).unwrap();
        let mut short = bytes.clone();
        short.extend_from_slice(&MAINNET.magic[..2]);
        fs::write(scratch.file("blocks.dat"), &short).unwrap();

        assert_eq!(opened(&scratch, "blocks.dat").end(), torn);
    }

    /// The one number on disk a torn write can make arbitrary. `read_count`
    /// applies the same rule to a count a stranger sends.
    #[test]
    fn a_length_prefix_that_overruns_the_file_is_refused_without_allocating() {
        let scratch = Scratch::new("liar");
        let mut bytes = MAINNET.magic.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(b"four");
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        let file = opened(&scratch, "blocks.dat");

        assert_eq!(file.end(), 0);
    }

    #[test]
    fn a_length_prefix_past_max_record_is_refused_before_the_file_size_is_consulted() {
        let scratch = Scratch::new("greedy");
        let mut bytes = MAINNET.magic.to_vec();
        bytes.extend_from_slice(&(MAX_RECORD + 1).to_le_bytes());
        bytes.extend_from_slice(&vec![0u8; MAX_RECORD as usize + 1]);
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        assert_eq!(opened(&scratch, "blocks.dat").end(), 0);
    }

    #[test]
    fn a_record_past_max_record_is_refused_rather_than_written() {
        let scratch = Scratch::new("too-big");
        let mut file = opened(&scratch, "blocks.dat");

        file.append(&vec![0u8; MAX_RECORD as usize + 1])
            .expect_err("a record no reader would accept must not be written");

        assert_eq!(file.end(), 0);
    }

    #[test]
    fn a_torn_undo_file_costs_a_good_blocks_file_nothing() {
        let scratch = Scratch::new("independent");
        let kept = {
            let mut files = BlockFiles::open(&scratch.0, &MAINNET).unwrap();
            let at = files.blocks.append(b"a block").unwrap();
            files.undo.append(b"its undo record").unwrap();
            at
        };

        let bytes = fs::read(scratch.file("undo.dat")).unwrap();
        fs::write(scratch.file("undo.dat"), &bytes[..bytes.len() - 4]).unwrap();

        let mut files = BlockFiles::open(&scratch.0, &MAINNET).unwrap();

        assert_eq!(files.blocks.read_at(kept).unwrap(), b"a block");
        assert_eq!(files.undo.end(), 0, "the torn record is the one that goes");
    }

    /// The stamp keeps a directory on one network, but a file carries its own
    /// magic so a record is self-describing wherever it is read.
    #[test]
    fn a_file_written_on_one_network_reads_as_empty_on_another() {
        let scratch = Scratch::new("other-network");
        RecordFile::open(scratch.file("blocks.dat"), &MAINNET)
            .unwrap()
            .append(b"a block")
            .unwrap();

        let confused = RecordFile::open(scratch.file("blocks.dat"), &TESTNET).unwrap();

        assert_eq!(confused.end(), 0);
    }

    #[test]
    fn an_empty_record_is_a_record() {
        let scratch = Scratch::new("empty");
        let mut file = opened(&scratch, "blocks.dat");

        let at = file.append(b"").unwrap();
        let next = file.append(b"after").unwrap();

        assert_eq!(file.read_at(at).unwrap(), b"");
        assert_eq!(file.read_at(next).unwrap(), b"after");
    }

    #[test]
    fn reading_past_the_end_is_an_error_naming_the_file() {
        let scratch = Scratch::new("past-the-end");
        let mut file = opened(&scratch, "blocks.dat");
        file.append(b"one").unwrap();

        let error = format!("{:#}", file.read_at(file.end()).unwrap_err());

        assert!(error.contains("blocks.dat"), "{error}");
    }
}
