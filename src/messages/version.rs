use crate::byte_reader::ByteReader;
use crate::messages::message::Payload;
use crate::util::command_12;
use anyhow::Result;
use rand::Rng;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

pub const VERSION_COMMAND_NAME: &str = "version";
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub protocol_version: u32,
    pub nonce: u64,
    pub listen_address: SocketAddr,
}

impl Version {
    pub fn new(nonce: u64, listen_address: SocketAddr) -> Self {
        Version {
            protocol_version: PROTOCOL_VERSION,
            nonce,
            listen_address,
        }
    }

    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Version> {
        let mut reader = ByteReader::new(&bytes);

        Ok(Version {
            protocol_version: reader.read_u32()?,
            nonce: reader.read_u64()?,
            listen_address: read_address(&mut reader)?,
        })
    }
}

impl Payload for Version {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        let mut raw_format = Vec::new();
        raw_format.extend_from_slice(&self.protocol_version.to_le_bytes());
        raw_format.extend_from_slice(&self.nonce.to_le_bytes());
        raw_format.extend_from_slice(&write_address(self.listen_address));

        Ok(raw_format)
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(VERSION_COMMAND_NAME)
    }
}

pub fn a_nonce() -> u64 {
    rand::rng().next_u64()
}

// Always 16 bytes plus a port, IPv4 mapped into IPv6, so the field is fixed
// width and a v4 peer and a v6 peer parse through the same path.
fn write_address(address: SocketAddr) -> [u8; 18] {
    let mut out = [0u8; 18];

    let ip = match address.ip() {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    };

    out[..16].copy_from_slice(&ip.octets());
    out[16..].copy_from_slice(&address.port().to_le_bytes());
    out
}

fn read_address(reader: &mut ByteReader) -> Result<SocketAddr> {
    let mapped = Ipv6Addr::from(reader.read_array::<16>()?);
    let port = reader.read_u16()?;

    let ip = match mapped.to_ipv4_mapped() {
        Some(v4) => IpAddr::V4(v4),
        None => IpAddr::V6(mapped),
    };

    Ok(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::loopback("127.0.0.1:34352")]
    #[case::any_v4("0.0.0.0:1")]
    #[case::highest_port("192.168.1.10:65535")]
    #[case::v6_loopback("[::1]:34352")]
    #[case::v6_full("[2001:db8::dead:beef]:8333")]
    fn a_version_survives_a_round_trip(#[case] listen_address: &str) {
        let original = Version::new(0xdead_beef_cafe_f00d, listen_address.parse().unwrap());

        let parsed = Version::parse_raw_format(original.get_raw_format().unwrap()).unwrap();

        assert_eq!(original, parsed);
        assert_eq!(listen_address.parse::<SocketAddr>().unwrap(), parsed.listen_address);
    }

    #[test]
    fn a_v4_address_does_not_come_back_as_a_mapped_v6_one() {
        let original = Version::new(1, "127.0.0.1:34352".parse().unwrap());

        let parsed = Version::parse_raw_format(original.get_raw_format().unwrap()).unwrap();

        assert!(
            parsed.listen_address.is_ipv4(),
            "a v4 peer must not become v6 by round-tripping, or it will never match a dialled address"
        );
    }

    #[test]
    fn a_truncated_version_is_refused_rather_than_filled_in() {
        let complete = Version::new(1, "127.0.0.1:1".parse().unwrap())
            .get_raw_format()
            .unwrap();

        for length in 0..complete.len() {
            assert!(
                Version::parse_raw_format(complete[..length].to_vec()).is_err(),
                "{length} of {} bytes should not parse",
                complete.len()
            );
        }
    }

    #[test]
    fn nonces_differ_between_runs() {
        assert_ne!(a_nonce(), a_nonce());
    }
}
