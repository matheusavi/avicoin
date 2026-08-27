use crate::block::{BlockHash, Header, HEADER_SIZE};
use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::util::{command_12, get_compact_int};
use anyhow::{anyhow, Result};

pub const GETHEADERS_COMMAND_NAME: &str = "getheaders";
pub const HEADERS_COMMAND_NAME: &str = "headers";

/// What one `headers` may carry. Bitcoin's number, and the reason a peer
/// cannot answer a locator with an endless stream.
pub const MAX_HEADERS: usize = 2_000;

/// A locator names a chain by stepping back from its tip — ten one at a time,
/// then doubling — so two nodes find where they agree in `log(height)` hashes
/// rather than by sending a chain.
pub const MAX_LOCATOR: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetHeaders {
    pub locator: Vec<BlockHash>,
    /// Where to stop, or all zeroes for "as far as you can".
    pub stop: BlockHash,
}

impl GetHeaders {
    pub fn new(locator: Vec<BlockHash>) -> Self {
        GetHeaders {
            locator,
            stop: BlockHash::from_bytes([0; 32]),
        }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<GetHeaders> {
        let mut reader = ByteReader::new(&bytes);
        let count = reader.read_compact()?;

        if count as usize > MAX_LOCATOR {
            return Err(anyhow!("a locator of {count} is over {MAX_LOCATOR}"));
        }

        let mut locator = Vec::new();
        for _ in 0..count {
            locator.push(BlockHash::from_bytes(reader.read_array::<32>()?));
        }

        Ok(GetHeaders {
            locator,
            stop: BlockHash::from_bytes(reader.read_array::<32>()?),
        })
    }
}

impl Payload for GetHeaders {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        if self.locator.len() > MAX_LOCATOR {
            return Err(anyhow!(
                "refusing to send a locator of {}, over {MAX_LOCATOR}",
                self.locator.len()
            ));
        }

        let mut raw = get_compact_int(self.locator.len() as u64);
        for hash in &self.locator {
            raw.extend_from_slice(hash.as_bytes());
        }
        raw.extend_from_slice(self.stop.as_bytes());

        Ok(raw)
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(GETHEADERS_COMMAND_NAME)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Headers {
    pub headers: Vec<Header>,
}

impl Headers {
    pub fn new(headers: Vec<Header>) -> Self {
        Headers { headers }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Headers> {
        let mut reader = ByteReader::new(&bytes);
        let count = reader.read_count(HEADER_SIZE)?;

        if count > MAX_HEADERS {
            return Err(anyhow!("{count} headers is over {MAX_HEADERS}"));
        }

        let mut headers = Vec::new();
        for _ in 0..count {
            headers.push(Header::parse(&mut reader)?);
        }

        Ok(Headers { headers })
    }
}

impl Payload for Headers {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        if self.headers.len() > MAX_HEADERS {
            return Err(anyhow!(
                "refusing to send {} headers, over {MAX_HEADERS}",
                self.headers.len()
            ));
        }

        let mut raw = get_compact_int(self.headers.len() as u64);
        for header in &self.headers {
            raw.extend_from_slice(&header.raw());
        }

        Ok(raw)
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(HEADERS_COMMAND_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn a_header(nonce: u32) -> Header {
        Header {
            version: 1,
            previous_block_hash: BlockHash::from_bytes([nonce as u8; 32]),
            merkle_root: [7; 32],
            time: 1_756_252_800 + nonce,
            n_bits: 0x2000ffff,
            nonce,
        }
    }

    #[rstest]
    #[case::empty(0)]
    #[case::one(1)]
    #[case::past_a_single_byte_count(253)]
    #[case::at_the_cap(MAX_HEADERS)]
    fn headers_survive_a_round_trip(#[case] count: usize) {
        let original = Headers::new((0..count as u32).map(a_header).collect());

        let parsed = Headers::parse_raw_format(original.get_raw_format().unwrap()).unwrap();

        assert_eq!(parsed, original);
    }

    #[test]
    fn a_getheaders_survives_a_round_trip() {
        let original = GetHeaders::new((0..5).map(|n| BlockHash::from_bytes([n; 32])).collect());

        assert_eq!(
            GetHeaders::parse_raw_format(original.get_raw_format().unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn more_headers_than_the_cap_are_refused_on_the_count_alone() {
        let mut claiming = get_compact_int(MAX_HEADERS as u64 + 1);
        claiming.extend_from_slice(&a_header(1).raw());

        assert!(Headers::parse_raw_format(claiming).is_err());
    }

    #[test]
    fn a_count_that_outruns_the_bytes_is_refused_rather_than_filled_in() {
        let mut lying = get_compact_int(4);
        lying.extend_from_slice(&a_header(1).raw());

        Headers::parse_raw_format(lying)
            .expect_err("four claimed, one supplied: the count is not evidence");
    }

    #[test]
    fn a_locator_over_the_cap_is_refused() {
        let too_many = GetHeaders::new(
            (0..=MAX_LOCATOR as u8)
                .map(|n| BlockHash::from_bytes([n; 32]))
                .collect(),
        );

        assert!(too_many.get_raw_format().is_err());
    }

    #[test]
    fn we_refuse_to_send_more_headers_than_we_would_accept() {
        Headers::new((0..MAX_HEADERS as u32 + 1).map(a_header).collect())
            .get_raw_format()
            .expect_err("a message we would reject on arrival is not one to send");
    }
}
