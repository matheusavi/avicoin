use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::transaction::Txid;
use crate::util::{command_12, get_compact_int};
use anyhow::{anyhow, Result};

pub const INV_COMMAND_NAME: &str = "inv";
pub const GETDATA_COMMAND_NAME: &str = "getdata";

/// One entry is a 4-byte kind and a 32-byte hash, so this is a bound on
/// memory as well as on count.
pub const MAX_INVENTORY: usize = 1_000;

const TRANSACTION_KIND: u32 = 1;

/// What a peer is offering or asking for. Only transactions exist to name in
/// this milestone; blocks join in M4, which is why the kind is on the wire
/// rather than implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Transaction(Txid),
}

impl Item {
    fn write(&self) -> [u8; 36] {
        let Item::Transaction(txid) = self;

        let mut bytes = [0u8; 36];
        bytes[..4].copy_from_slice(&TRANSACTION_KIND.to_le_bytes());
        bytes[4..].copy_from_slice(txid.as_bytes());
        bytes
    }

    fn read(reader: &mut ByteReader) -> Result<Item> {
        match reader.read_u32()? {
            TRANSACTION_KIND => Ok(Item::Transaction(Txid::from_bytes(
                reader.read_array::<32>()?,
            ))),
            unknown => Err(anyhow!("{unknown} is not a kind of thing to ask for")),
        }
    }
}

/// `inv` and `getdata` carry the same list and differ only in what the
/// receiver does with it, so they share an encoding and a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    pub items: Vec<Item>,
    command: &'static str,
}

impl Inventory {
    pub fn offered(items: Vec<Item>) -> Self {
        Inventory {
            items,
            command: INV_COMMAND_NAME,
        }
    }

    pub fn requested(items: Vec<Item>) -> Self {
        Inventory {
            items,
            command: GETDATA_COMMAND_NAME,
        }
    }

    pub fn parse_raw_format(bytes: Vec<u8>, command: &'static str) -> Result<Inventory> {
        let mut reader = ByteReader::new(&bytes);
        let count = reader.read_compact()?;

        if count as usize > MAX_INVENTORY {
            return Err(anyhow!(
                "{command} claims {count} items, over {MAX_INVENTORY}"
            ));
        }

        // Counted up to, never reserved: the reads below are what prove the
        // bytes were really there.
        let mut items = Vec::new();
        for _ in 0..count {
            items.push(Item::read(&mut reader)?);
        }

        Ok(Inventory { items, command })
    }
}

impl Payload for Inventory {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        if self.items.len() > MAX_INVENTORY {
            return Err(anyhow!(
                "refusing to send {} items, over {MAX_INVENTORY}",
                self.items.len()
            ));
        }

        let mut raw_format = get_compact_int(self.items.len() as u64);
        for item in &self.items {
            raw_format.extend_from_slice(&item.write());
        }

        Ok(raw_format)
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(self.command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn items(count: usize) -> Vec<Item> {
        (0..count)
            .map(|index| Item::Transaction(Txid::from_bytes([index as u8; 32])))
            .collect()
    }

    #[rstest]
    #[case::empty(0)]
    #[case::one(1)]
    #[case::past_a_single_byte_count(253)]
    #[case::at_the_cap(MAX_INVENTORY)]
    fn an_inv_survives_a_round_trip(#[case] count: usize) {
        let original = Inventory::offered(items(count));

        let parsed =
            Inventory::parse_raw_format(original.get_raw_format().unwrap(), INV_COMMAND_NAME)
                .unwrap();

        assert_eq!(original, parsed);
    }

    #[test]
    fn a_getdata_survives_a_round_trip_and_keeps_its_command() {
        let original = Inventory::requested(items(3));

        let parsed =
            Inventory::parse_raw_format(original.get_raw_format().unwrap(), GETDATA_COMMAND_NAME)
                .unwrap();

        assert_eq!(original, parsed);
        assert_eq!(parsed.get_command_name(), command_12(GETDATA_COMMAND_NAME));
    }

    #[test]
    fn an_inventory_over_the_cap_is_refused_on_its_count_alone() {
        let mut claiming_too_many = get_compact_int(MAX_INVENTORY as u64 + 1);
        claiming_too_many.extend_from_slice(&items(1)[0].write());

        let error = Inventory::parse_raw_format(claiming_too_many, INV_COMMAND_NAME)
            .expect_err("a flood must be refused before its items are read");

        assert!(format!("{error:#}").contains("over"), "got: {error:#}");
    }

    #[test]
    fn a_count_that_outruns_the_bytes_is_refused_rather_than_filled_in() {
        let mut lying = get_compact_int(4);
        lying.extend_from_slice(&items(1)[0].write());

        Inventory::parse_raw_format(lying, INV_COMMAND_NAME)
            .expect_err("four claimed, one supplied: the count is not evidence");
    }

    #[test]
    fn an_unknown_kind_of_item_is_refused() {
        let mut strange = get_compact_int(1);
        strange.extend_from_slice(&9u32.to_le_bytes());
        strange.extend_from_slice(&[0u8; 32]);

        assert!(Inventory::parse_raw_format(strange, INV_COMMAND_NAME).is_err());
    }

    #[test]
    fn we_refuse_to_send_more_than_we_would_accept() {
        Inventory::offered(items(MAX_INVENTORY + 1))
            .get_raw_format()
            .expect_err("a message we would reject on arrival is not one to send");
    }
}
