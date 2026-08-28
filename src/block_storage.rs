use crate::data_dir::DataDir;
use crate::params::Network;
use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

const FRAME_HEADER: u64 = 8;

/// Twice `MAX_BLOCK_SIZE`, because an undo record carries a whole `TxOut` per
/// input its block spent and can outgrow the block that produced it.
pub const MAX_RECORD: u32 = 2 * crate::validation::MAX_BLOCK_SIZE as u32;

/// The most a crash can leave behind: the one record that was in flight.
/// Anything past this is corruption rather than a torn write, and the
/// difference decides whether opening repairs the file or refuses it.
const MAX_TORN: u64 = FRAME_HEADER + MAX_RECORD as u64;

/// An append-only file of framed records, addressed by the offset an append
/// returns. The format is in
/// [on-disk-format.md](../docs/on-disk-format.md).
#[derive(Debug)]
pub struct RecordFile {
    file: File,
    path: PathBuf,
    magic: [u8; 4],
    end: u64,
    discarded: u64,
}

impl RecordFile {
    /// Opens the file, creating it if it is absent, and truncates it to the
    /// last record that reads back whole — unless more than one record's worth
    /// is unreadable, which no crash can produce and which is therefore
    /// refused rather than repaired.
    pub fn open(path: impl Into<PathBuf>, network: Network) -> Result<RecordFile> {
        let path = path.into();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("could not open {}", path.display()))?;

        let (end, size) = readable_end(&mut file, network.magic)
            .with_context(|| format!("could not read {}", path.display()))?;

        let discarded = size - end;
        if discarded > MAX_TORN {
            bail!(
                "{} is unreadable from byte {end} of {size}; a crash can cost the record in \
                 flight, and this is {discarded} bytes",
                path.display()
            );
        }

        file.set_len(end)
            .with_context(|| format!("could not truncate {}", path.display()))?;

        Ok(RecordFile {
            file,
            path,
            magic: network.magic,
            end,
            discarded,
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

        // The reads seek, and reads and writes share one cursor. Seeking here
        // is also what makes a failed write harmless: `end` did not move, so
        // the next append lands on whatever the failure left.
        self.file.seek(SeekFrom::Start(at))?;
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

    /// A record is durable before anything that points at it is written. The
    /// order is [ADR-0013](../docs/adr/0013-persistence.md)'s.
    pub fn sync(&self) -> Result<()> {
        self.file
            .sync_data()
            .with_context(|| format!("could not flush {}", self.path.display()))
    }

    /// Where the next record will go. Only the tests read it — an append
    /// returns the offset a caller actually stores — and it is what says
    /// *where* a repair stopped.
    #[cfg(test)]
    pub fn end(&self) -> u64 {
        self.end
    }

    /// How many bytes opening threw away. Zero on a clean file; the record a
    /// crash left in flight otherwise. The caller decides whether losing that
    /// much is worth saying out loud.
    pub fn discarded(&self) -> u64 {
        self.discarded
    }
}

/// Seeks over the payloads rather than reading them: this runs over the whole
/// file at every startup, and the answer is an offset, not the contents.
fn readable_end(file: &mut File, magic: [u8; 4]) -> Result<(u64, u64)> {
    let size = file.seek(SeekFrom::End(0))?;
    let mut end = 0;

    while let Some(next) = skip_frame(file, end, size, magic)? {
        end = next;
    }

    Ok((end, size))
}

/// Where the frame at `at` ends, or `None` where the readable region does.
fn skip_frame(file: &mut File, at: u64, size: u64, magic: [u8; 4]) -> Result<Option<u64>> {
    let Some(length) = frame_length(file, at, size, magic)? else {
        return Ok(None);
    };

    Ok(Some(at + FRAME_HEADER + u64::from(length)))
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
    let Some(length) = frame_length(file, at, size, magic)? else {
        return Ok(None);
    };

    let mut record = vec![0u8; length as usize];
    file.read_exact(&mut record)?;

    Ok(Some((record, at + FRAME_HEADER + u64::from(length))))
}

/// Leaves the cursor at the payload when it returns a length, which is what
/// lets `read_frame` read it without seeking again.
fn frame_length(file: &mut File, at: u64, size: u64, magic: [u8; 4]) -> Result<Option<u32>> {
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
    // number on disk a torn write can make arbitrary, and `vec![0; length]`
    // would believe it.
    if length > MAX_RECORD {
        return Ok(None);
    }

    if at + FRAME_HEADER + u64::from(length) > size {
        return Ok(None);
    }

    Ok(Some(length))
}

#[derive(Debug)]
pub struct BlockFiles {
    pub blocks: RecordFile,
    pub undo: RecordFile,
}

impl BlockFiles {
    /// Takes the directory rather than a path, so the advisory lock that stops
    /// two nodes sharing these files is a precondition of the type rather than
    /// a convention.
    pub fn open(directory: &DataDir, network: Network) -> Result<BlockFiles> {
        Ok(BlockFiles {
            blocks: RecordFile::open(directory.path().join("blocks.dat"), network)?,
            undo: RecordFile::open(directory.path().join("undo.dat"), network)?,
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

    /// A crash costs the record in flight. It cannot cost the ninety before
    /// it, so a file that is unreadable that far back is not a crash and is
    /// not silently rewritten to nothing.
    #[test]
    fn a_file_unreadable_further_back_than_a_crash_could_reach_is_refused_not_erased() {
        let scratch = Scratch::new("rotted");
        let written = {
            let mut file = opened(&scratch, "blocks.dat");
            for n in 0..8u8 {
                file.append(&vec![n; MAX_RECORD as usize / 4]).unwrap();
            }
            file.end()
        };

        let mut bytes = fs::read(scratch.file("blocks.dat")).unwrap();
        bytes[2] ^= 0x01;
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        let error = format!(
            "{:#}",
            RecordFile::open(scratch.file("blocks.dat"), &MAINNET)
                .expect_err("this is corruption, not a torn write")
        );

        assert!(error.contains("blocks.dat"), "{error}");
        assert_eq!(
            fs::metadata(scratch.file("blocks.dat")).unwrap().len(),
            written,
            "a refusal must not be a deletion"
        );
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

    /// Inside `MAX_RECORD`, so only the file's own size can refuse it — the
    /// other half of the pair below.
    #[test]
    fn a_length_prefix_that_overruns_the_file_is_refused() {
        let scratch = Scratch::new("liar");
        let mut bytes = MAINNET.magic.to_vec();
        bytes.extend_from_slice(&1000u32.to_le_bytes());
        bytes.extend_from_slice(b"four");
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        let file = opened(&scratch, "blocks.dat");

        assert_eq!(file.end(), 0);
    }

    /// The file is exactly as long as the prefix claims, so *only* the bound
    /// can refuse this one — which is what pins the bound to being checked at
    /// all, rather than the file's own size doing the work by accident.
    #[test]
    fn a_length_prefix_past_max_record_is_refused_even_when_the_file_is_that_long() {
        let scratch = Scratch::new("greedy");
        let mut bytes = MAINNET.magic.to_vec();
        bytes.extend_from_slice(&(MAX_RECORD + 1).to_le_bytes());
        bytes.extend_from_slice(&vec![0u8; MAX_RECORD as usize + 1]);
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        RecordFile::open(scratch.file("blocks.dat"), &MAINNET)
            .expect_err("no record is this large, whatever the prefix claims");
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
        let directory = DataDir::open(&scratch.0, &MAINNET).unwrap();
        let kept = {
            let mut files = BlockFiles::open(&directory, &MAINNET).unwrap();
            let at = files.blocks.append(b"a block").unwrap();
            files.undo.append(b"its undo record").unwrap();
            at
        };

        let bytes = fs::read(scratch.file("undo.dat")).unwrap();
        fs::write(scratch.file("undo.dat"), &bytes[..bytes.len() - 4]).unwrap();

        let mut files = BlockFiles::open(&directory, &MAINNET).unwrap();

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

    /// Reads and writes share one file cursor, and a read leaves it wherever
    /// the record it read ended. An append that trusted it would land on top
    /// of a record already written and hand back an offset past the end.
    #[test]
    fn an_append_after_a_read_lands_where_the_offset_says() {
        let scratch = Scratch::new("read-then-append");
        let mut file = opened(&scratch, "blocks.dat");
        let first = file.append(b"first").unwrap();
        let second = file.append(b"second-record").unwrap();

        file.read_at(first).unwrap();
        let third = file.append(b"third").unwrap();

        assert_eq!(file.read_at(second).unwrap(), b"second-record");
        assert_eq!(file.read_at(third).unwrap(), b"third");
        assert_eq!(
            fs::metadata(scratch.file("blocks.dat")).unwrap().len(),
            file.end()
        );
    }

    #[test]
    fn opening_a_clean_file_discards_nothing_and_a_torn_one_says_how_much() {
        let scratch = Scratch::new("discarded");
        let torn = {
            let mut file = opened(&scratch, "blocks.dat");
            file.append(b"kept").unwrap();
            file.end()
        };
        assert_eq!(opened(&scratch, "blocks.dat").discarded(), 0);

        let mut bytes = fs::read(scratch.file("blocks.dat")).unwrap();
        bytes.extend_from_slice(b"half a fram");
        fs::write(scratch.file("blocks.dat"), &bytes).unwrap();

        let reopened = opened(&scratch, "blocks.dat");

        assert_eq!(reopened.end(), torn);
        assert_eq!(reopened.discarded(), 11);
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
