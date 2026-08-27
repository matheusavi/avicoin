use crate::block::{BlockHash, Header};
use crate::byte_reader::ByteReader;
use crate::data_dir::DataDir;
use crate::transaction::{Outpoint, TxOut};
use crate::utxo::Coin;
use anyhow::{bail, Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::Path;

const DATABASE: &str = "chain.redb";

const HEADERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("headers");
const COINS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("coins");
const MARKERS: TableDefinition<&str, &[u8]> = TableDefinition::new("markers");

const BEST_BLOCK: &str = "best_block";

/// A block whose body the node has not applied has no record in either file.
const NOWHERE: u64 = u64::MAX;

/// A header, and where its block and undo record live in the two files. The
/// offsets arrive later than the header does, so this is rewritten when the
/// block is applied rather than written once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Indexed {
    pub header: Header,
    pub block_at: Option<u64>,
    pub undo_at: Option<u64>,
}

/// The block index and the UTXO set, in an embedded key-value store.
///
/// Both are random-access by a hash the caller already holds, which is what a
/// B-tree is for and what the append-only block files are not
/// ([ADR-0013](../docs/adr/0013-persistence.md)).
#[derive(Debug)]
pub struct Store {
    db: Database,
    path: std::path::PathBuf,
}

/// One redb write transaction. Dropping it without committing leaves nothing.
pub struct Batch {
    transaction: redb::WriteTransaction,
}

impl Store {
    /// Takes the directory rather than a path, so the file's name lives with
    /// the code that reads it and the advisory lock is a precondition of the
    /// type.
    pub fn open(directory: &DataDir) -> Result<Store> {
        Store::at(&directory.path().join(DATABASE))
    }

    fn at(path: &Path) -> Result<Store> {
        let db = Database::create(path)
            .with_context(|| format!("could not open the store at {}", path.display()))?;

        // The tables have to exist before a read transaction can open them,
        // and a freshly created database has none.
        let transaction = db.begin_write()?;
        transaction.open_table(HEADERS)?;
        transaction.open_table(COINS)?;
        transaction.open_table(MARKERS)?;
        transaction.commit()?;

        Ok(Store {
            db,
            path: path.to_path_buf(),
        })
    }

    /// Every header the store holds, in no particular order. The caller sorts
    /// them into a chain, because only it knows what genesis is.
    pub fn headers(&self) -> Result<Vec<Indexed>> {
        let path = self.path.clone();
        self.read_headers()
            .with_context(|| format!("reading the block index from {}", path.display()))
    }

    fn read_headers(&self) -> Result<Vec<Indexed>> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(HEADERS)?;

        table
            .iter()?
            .map(|entry| {
                let (_, value) = entry?;
                parse_indexed(value.value())
            })
            .collect()
    }

    pub fn coins(&self) -> Result<Vec<(Outpoint, Coin)>> {
        let path = self.path.clone();
        self.read_coins()
            .with_context(|| format!("reading the UTXO set from {}", path.display()))
    }

    fn read_coins(&self) -> Result<Vec<(Outpoint, Coin)>> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(COINS)?;

        table
            .iter()?
            .map(|entry| {
                let (key, value) = entry?;
                Ok((
                    Outpoint::parse(&mut ByteReader::new(key.value()))?,
                    parse_coin(value.value())?,
                ))
            })
            .collect()
    }

    pub fn best_block(&self) -> Result<Option<BlockHash>> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(MARKERS)?;

        match table.get(BEST_BLOCK)? {
            None => Ok(None),
            Some(value) => {
                let bytes: [u8; 32] = value
                    .value()
                    .try_into()
                    .ok()
                    .context("the best-block marker is not a hash")?;
                Ok(Some(BlockHash::from_bytes(bytes)))
            }
        }
    }

    pub fn batch(&self) -> Result<Batch> {
        Ok(Batch {
            transaction: self.db.begin_write()?,
        })
    }
}

impl Batch {
    pub fn put_header(&self, indexed: &Indexed) -> Result<()> {
        let mut table = self.transaction.open_table(HEADERS)?;
        table.insert(
            indexed.header.hash().as_bytes().as_slice(),
            raw_indexed(indexed).as_slice(),
        )?;
        Ok(())
    }

    pub fn put_coin(&self, outpoint: &Outpoint, coin: &Coin) -> Result<()> {
        let mut table = self.transaction.open_table(COINS)?;
        table.insert(outpoint.raw().as_slice(), raw_coin(coin).as_slice())?;
        Ok(())
    }

    pub fn remove_coin(&self, outpoint: &Outpoint) -> Result<()> {
        let mut table = self.transaction.open_table(COINS)?;
        table.remove(outpoint.raw().as_slice())?;
        Ok(())
    }

    pub fn mark_best(&self, hash: &BlockHash) -> Result<()> {
        let mut table = self.transaction.open_table(MARKERS)?;
        table.insert(BEST_BLOCK, hash.as_bytes().as_slice())?;
        Ok(())
    }

    pub fn commit(self) -> Result<()> {
        self.transaction.commit()?;
        Ok(())
    }
}

fn raw_indexed(indexed: &Indexed) -> Vec<u8> {
    let mut raw = indexed.header.raw().to_vec();
    raw.extend(indexed.block_at.unwrap_or(NOWHERE).to_le_bytes());
    raw.extend(indexed.undo_at.unwrap_or(NOWHERE).to_le_bytes());
    raw
}

fn parse_indexed(bytes: &[u8]) -> Result<Indexed> {
    let mut reader = ByteReader::new(bytes);
    let indexed = Indexed {
        header: Header::parse(&mut reader)?,
        block_at: somewhere(reader.read_u64()?),
        undo_at: somewhere(reader.read_u64()?),
    };
    exhausted(&reader, "an index entry")?;

    Ok(indexed)
}

/// A record with bytes to spare is corruption, not a newer format. Refusing it
/// is the same instinct as checking the coinbase flag is one of its two legal
/// values — a value that parses is not yet a value that is right.
fn exhausted(reader: &ByteReader, what: &str) -> Result<()> {
    match reader.remaining() {
        0 => Ok(()),
        extra => bail!("{what} is followed by {extra} bytes that are not part of it"),
    }
}

fn somewhere(offset: u64) -> Option<u64> {
    (offset != NOWHERE).then_some(offset)
}

fn raw_coin(coin: &Coin) -> Vec<u8> {
    let mut raw = coin.height.to_le_bytes().to_vec();
    raw.push(coin.from_coinbase as u8);
    raw.extend(coin.output.raw());
    raw
}

fn parse_coin(bytes: &[u8]) -> Result<Coin> {
    let mut reader = ByteReader::new(bytes);
    let coin = parse_coin_from(&mut reader)?;
    exhausted(&reader, "a coin")?;
    Ok(coin)
}

fn parse_coin_from(reader: &mut ByteReader) -> Result<Coin> {
    let height = reader.read_u32()?;
    let from_coinbase = match reader.read_byte()? {
        0 => false,
        1 => true,
        other => bail!("{other} is not a coinbase flag"),
    };

    Ok(Coin {
        output: TxOut::parse(reader)?,
        height,
        from_coinbase,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::Amount;
    use crate::blockchain::BlockIndex;
    use crate::params::MAINNET;
    use crate::transaction::Txid;
    use crate::utxo::UtxoSet;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let path =
                std::env::temp_dir().join(format!("avicoin-store-{name}-{}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn open(&self) -> Store {
            Store::open(&DataDir::open(&self.0, &MAINNET).unwrap()).unwrap()
        }

        fn database(&self) -> PathBuf {
            self.0.join(DATABASE)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Headers only — no bodies, no work done. `restored` cares about parents
    /// and nothing else.
    fn a_chain_of_headers(genesis: &crate::block::Block, count: u32) -> Vec<Header> {
        let mut headers = vec![genesis.header().unwrap()];
        for nonce in 1..=count {
            let mut next = headers[headers.len() - 1];
            next.previous_block_hash = next.hash();
            next.nonce = nonce;
            headers.push(next);
        }
        headers
    }

    fn a_header(nonce: u32) -> Header {
        let mut block = MAINNET.genesis().unwrap();
        block.nonce = nonce;
        block.header().unwrap()
    }

    fn a_coin(atoms: u64, height: u32, from_coinbase: bool) -> Coin {
        Coin {
            output: TxOut {
                value: Amount::from_atoms(atoms).unwrap(),
                script_pubkey: vec![0x76, 0xa9, atoms as u8],
            },
            height,
            from_coinbase,
        }
    }

    fn an_outpoint(seed: u8, v_out: u32) -> Outpoint {
        Outpoint {
            txid: Txid::from_bytes([seed; 32]),
            v_out,
        }
    }

    #[test]
    fn a_header_comes_back_from_a_reopened_store() {
        let scratch = Scratch::new("headers");
        let header = a_header(7);
        {
            let store = scratch.open();
            let batch = store.batch().unwrap();
            batch
                .put_header(&Indexed {
                    header,
                    block_at: Some(0),
                    undo_at: Some(96),
                })
                .unwrap();
            batch.commit().unwrap();
        }

        let reopened = scratch.open();

        assert_eq!(
            reopened.headers().unwrap(),
            vec![Indexed {
                header,
                block_at: Some(0),
                undo_at: Some(96)
            }]
        );
    }

    /// redb holds its own lock on the database file, so a second `Store` on
    /// one path is refused. Belt to `DataDir`'s braces, and the reason these
    /// tests drop before they reopen.
    #[test]
    fn a_store_already_open_cannot_be_opened_again() {
        let scratch = Scratch::new("contended");
        let held = scratch.open();

        Store::at(&scratch.database())
            .expect_err("two handles on one store is two nodes on one directory");

        drop(held);
        scratch.open();
    }

    /// A header is indexed long before its body is applied, so the offsets are
    /// written by a second put rather than by the first.
    #[test]
    fn a_header_stored_without_offsets_reads_back_without_them() {
        let scratch = Scratch::new("no-offsets");
        let store = scratch.open();
        let batch = store.batch().unwrap();
        batch
            .put_header(&Indexed {
                header: a_header(3),
                block_at: None,
                undo_at: None,
            })
            .unwrap();
        batch.commit().unwrap();

        let stored = store.headers().unwrap();

        assert_eq!(stored[0].block_at, None);
        assert_eq!(stored[0].undo_at, None);
    }

    #[test]
    fn putting_a_header_again_replaces_what_was_there() {
        let scratch = Scratch::new("rewrite");
        let store = scratch.open();
        let header = a_header(11);
        for offsets in [(None, None), (Some(64), Some(8))] {
            let batch = store.batch().unwrap();
            batch
                .put_header(&Indexed {
                    header,
                    block_at: offsets.0,
                    undo_at: offsets.1,
                })
                .unwrap();
            batch.commit().unwrap();
        }

        assert_eq!(store.headers().unwrap().len(), 1);
        assert_eq!(store.headers().unwrap()[0].block_at, Some(64));
    }

    #[test]
    fn coins_come_back_from_a_reopened_store() {
        let scratch = Scratch::new("coins");
        let put: Vec<(Outpoint, Coin)> = vec![
            (an_outpoint(1, 0), a_coin(5_000, 1, true)),
            (an_outpoint(2, 7), a_coin(600, 9, false)),
        ];
        {
            let store = scratch.open();
            let batch = store.batch().unwrap();
            for (outpoint, coin) in &put {
                batch.put_coin(outpoint, coin).unwrap();
            }
            batch.commit().unwrap();
        }

        let mut back: HashMap<Outpoint, Coin> =
            scratch.open().coins().unwrap().into_iter().collect();

        assert_eq!(back.remove(&put[0].0), Some(put[0].1.clone()));
        assert_eq!(back.remove(&put[1].0), Some(put[1].1.clone()));
        assert!(back.is_empty());
    }

    #[test]
    fn a_removed_coin_does_not_come_back() {
        let scratch = Scratch::new("spent");
        let store = scratch.open();
        let outpoint = an_outpoint(3, 0);
        let batch = store.batch().unwrap();
        batch.put_coin(&outpoint, &a_coin(100, 1, false)).unwrap();
        batch.commit().unwrap();

        let batch = store.batch().unwrap();
        batch.remove_coin(&outpoint).unwrap();
        batch.commit().unwrap();
        drop(store);

        assert!(scratch.open().coins().unwrap().is_empty());
    }

    /// The property the whole arrangement rests on: a batch that is not
    /// committed leaves nothing behind, so a crash mid-block cannot land half
    /// of one.
    #[test]
    fn a_batch_that_is_dropped_changes_nothing() {
        let scratch = Scratch::new("uncommitted");
        let store = scratch.open();
        let batch = store.batch().unwrap();
        batch
            .put_coin(&an_outpoint(4, 0), &a_coin(1, 1, false))
            .unwrap();
        batch
            .put_header(&Indexed {
                header: a_header(1),
                block_at: None,
                undo_at: None,
            })
            .unwrap();
        batch.mark_best(&a_header(1).hash()).unwrap();

        drop(batch);

        assert!(store.coins().unwrap().is_empty());
        assert!(store.headers().unwrap().is_empty());
        assert_eq!(store.best_block().unwrap(), None);
    }

    #[test]
    fn the_marker_is_absent_until_something_sets_it() {
        let scratch = Scratch::new("marker");
        let store = scratch.open();
        let hash = a_header(5).hash();

        assert_eq!(store.best_block().unwrap(), None);

        let batch = store.batch().unwrap();
        batch.mark_best(&hash).unwrap();
        batch.commit().unwrap();
        drop(store);

        assert_eq!(scratch.open().best_block().unwrap(), Some(hash));
    }

    /// The pair the milestone turns on: what a running node held comes back
    /// from disk, at the cost of the set's size rather than the chain's
    /// height, and without a block being executed again.
    #[test]
    fn an_index_and_a_set_come_back_from_a_store_without_replaying_anything() {
        let scratch = Scratch::new("restored");
        let genesis = MAINNET.genesis().unwrap();
        let chain = a_chain_of_headers(&genesis, 4);
        let coins: Vec<(Outpoint, Coin)> = (0..6u8)
            .map(|n| {
                (
                    an_outpoint(n, n as u32),
                    a_coin(100 + n as u64, n as u32, n == 0),
                )
            })
            .collect();
        {
            let store = scratch.open();
            let batch = store.batch().unwrap();
            for header in &chain {
                batch
                    .put_header(&Indexed {
                        header: *header,
                        block_at: None,
                        undo_at: None,
                    })
                    .unwrap();
            }
            for (outpoint, coin) in &coins {
                batch.put_coin(outpoint, coin).unwrap();
            }
            batch.mark_best(&chain[chain.len() - 1].hash()).unwrap();
            batch.commit().unwrap();
        }

        let store = scratch.open();
        let headers: Vec<Header> = store
            .headers()
            .unwrap()
            .into_iter()
            .map(|i| i.header)
            .collect();
        let index = BlockIndex::restored(genesis.header().unwrap(), &headers).unwrap();
        let set = UtxoSet::restored(store.coins().unwrap());

        assert_eq!(index.best().height, 4);
        assert_eq!(index.best_hash(), chain[chain.len() - 1].hash());
        assert_eq!(store.best_block().unwrap(), Some(index.best_hash()));
        for (outpoint, coin) in &coins {
            assert_eq!(set.get(outpoint).as_ref(), Some(coin));
        }
    }

    /// The store hands headers back in whatever order its keys sort in, which
    /// is by hash and therefore arbitrary. Rebuilding has to put the parent
    /// first regardless.
    #[test]
    fn an_index_rebuilds_from_headers_in_any_order() {
        let genesis = MAINNET.genesis().unwrap();
        let mut chain = a_chain_of_headers(&genesis, 5);
        let tip = chain[chain.len() - 1].hash();
        chain.reverse();

        let index = BlockIndex::restored(genesis.header().unwrap(), &chain).unwrap();

        assert_eq!(index.best_hash(), tip);
        assert_eq!(index.best().height, 5);
    }

    #[test]
    fn a_stored_header_descending_from_nothing_is_corruption_and_says_so() {
        let genesis = MAINNET.genesis().unwrap();
        let mut orphan = a_header(1);
        orphan.previous_block_hash = crate::block::BlockHash::from_bytes([9; 32]);

        let error = format!(
            "{:#}",
            BlockIndex::restored(genesis.header().unwrap(), &[orphan]).unwrap_err()
        );

        assert!(error.contains("descend from no known block"), "{error}");
        assert!(error.contains(&orphan.hash().to_string()), "{error}");
    }

    #[test]
    fn a_coin_flag_that_is_neither_true_nor_false_is_refused() {
        let mut raw = raw_coin(&a_coin(10, 2, true));
        raw[4] = 2;

        assert!(parse_coin(&raw).is_err());
    }

    /// A value that parses is not yet a value that is right. Bytes to spare
    /// mean the record is not the record it claims to be.
    #[test]
    fn a_coin_with_bytes_to_spare_is_refused() {
        let padded = [raw_coin(&a_coin(10, 2, true)), vec![0x41; 16]].concat();

        assert!(parse_coin(&padded).is_err());
    }

    #[test]
    fn an_index_entry_with_bytes_to_spare_is_refused() {
        let entry = Indexed {
            header: a_header(1),
            block_at: None,
            undo_at: None,
        };
        let padded = [raw_indexed(&entry), vec![0x41; 16]].concat();

        assert!(parse_indexed(&padded).is_err());
    }

    /// One unreadable row must not come back as a set missing one coin: the
    /// node would spend what it does not have and refuse what it does.
    #[test]
    fn one_corrupt_row_refuses_the_whole_load_and_names_the_store() {
        let scratch = Scratch::new("bad-row");
        let outpoint = an_outpoint(1, 0);
        {
            let store = scratch.open();
            let batch = store.batch().unwrap();
            batch.put_coin(&outpoint, &a_coin(10, 1, false)).unwrap();
            batch.commit().unwrap();
        }

        {
            let db = Database::create(scratch.database()).unwrap();
            let transaction = db.begin_write().unwrap();
            {
                let mut table = transaction.open_table(COINS).unwrap();
                table
                    .insert(outpoint.raw().as_slice(), b"not a coin".as_slice())
                    .unwrap();
            }
            transaction.commit().unwrap();
        }

        let error = format!("{:#}", scratch.open().coins().unwrap_err());

        assert!(error.contains(DATABASE), "{error}");
    }

    #[test]
    fn a_database_that_is_not_a_database_is_an_error_naming_the_path() {
        let scratch = Scratch::new("garbage");
        fs::create_dir_all(&scratch.0).unwrap();
        fs::write(scratch.database(), vec![0x41; 4096]).unwrap();

        let error = format!("{:#}", Store::at(&scratch.database()).unwrap_err());

        assert!(error.contains(DATABASE), "{error}");
    }
}
