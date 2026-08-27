use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::messages::net_address::{read_address, write_address};
use crate::util::{command_12, get_compact_int};
use anyhow::{anyhow, Result};
use std::net::SocketAddr;

pub const ADDR_COMMAND_NAME: &str = "addr";

/// ADR-0017.
pub const MAX_ADDRESSES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Addr {
    pub addresses: Vec<SocketAddr>,
}

impl Addr {
    pub fn new(addresses: Vec<SocketAddr>) -> Self {
        Addr { addresses }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Addr> {
        let mut reader = ByteReader::new(&bytes);
        let count = reader.read_compact()?;

        if count as usize > MAX_ADDRESSES {
            return Err(anyhow!("addr claims {count} addresses, over {MAX_ADDRESSES}"));
        }

        // Counted up to, never reserved: the count is a claim by a stranger,
        // and the reads below are what prove the bytes were really there.
        let mut addresses = Vec::new();
        for _ in 0..count {
            addresses.push(read_address(&mut reader)?);
        }

        Ok(Addr { addresses })
    }
}

impl Payload for Addr {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        if self.addresses.len() > MAX_ADDRESSES {
            return Err(anyhow!(
                "refusing to send {} addresses, over {MAX_ADDRESSES}",
                self.addresses.len()
            ));
        }

        let mut raw_format = get_compact_int(self.addresses.len() as u64);
        for address in &self.addresses {
            raw_format.extend_from_slice(&write_address(*address));
        }

        Ok(raw_format)
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(ADDR_COMMAND_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn addresses(count: usize) -> Vec<SocketAddr> {
        (0..count)
            .map(|index| format!("127.0.0.1:{}", 5000 + index).parse().unwrap())
            .collect()
    }

    #[rstest]
    #[case::empty(0)]
    #[case::one(1)]
    #[case::past_a_single_byte_count(253)]
    #[case::at_the_cap(MAX_ADDRESSES)]
    fn an_addr_survives_a_round_trip(#[case] count: usize) {
        let original = Addr::new(addresses(count));

        let parsed = Addr::parse_raw_format(original.get_raw_format().unwrap()).unwrap();

        assert_eq!(original, parsed);
        assert_eq!(count, parsed.addresses.len());
    }

    #[test]
    fn a_mix_of_address_families_survives_a_round_trip() {
        let original = Addr::new(vec![
            "127.0.0.1:34352".parse().unwrap(),
            "[2001:db8::1]:8333".parse().unwrap(),
        ]);

        assert_eq!(
            original,
            Addr::parse_raw_format(original.get_raw_format().unwrap()).unwrap()
        );
    }

    #[test]
    fn an_addr_over_the_cap_is_refused_on_its_count_alone() {
        let mut claiming_too_many = get_compact_int(MAX_ADDRESSES as u64 + 1);
        claiming_too_many.extend_from_slice(&write_address("127.0.0.1:1".parse().unwrap()));

        let error = Addr::parse_raw_format(claiming_too_many)
            .expect_err("a flood must be refused before its addresses are read");

        assert!(format!("{error:#}").contains("over"), "got: {error:#}");
    }

    #[test]
    fn a_count_that_outruns_the_bytes_is_refused_rather_than_filled_in() {
        let mut lying = get_compact_int(4);
        lying.extend_from_slice(&write_address("127.0.0.1:1".parse().unwrap()));

        Addr::parse_raw_format(lying)
            .expect_err("four claimed, one supplied: the count is not evidence");
    }

    #[test]
    fn we_refuse_to_send_more_than_we_would_accept() {
        Addr::new(addresses(MAX_ADDRESSES + 1))
            .get_raw_format()
            .expect_err("a message we would reject on arrival is not one to send");
    }
}
