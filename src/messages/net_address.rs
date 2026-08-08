use crate::byte_reader::ByteReader;
use anyhow::Result;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

pub const NET_ADDRESS_LENGTH: usize = 18;

// IPv4 mapped into IPv6, so a v4 peer and a v6 peer parse through one path.
pub fn write_address(address: SocketAddr) -> [u8; NET_ADDRESS_LENGTH] {
    let mut out = [0u8; NET_ADDRESS_LENGTH];

    let ip = match address.ip() {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    };

    out[..16].copy_from_slice(&ip.octets());
    out[16..].copy_from_slice(&address.port().to_le_bytes());
    out
}

pub fn read_address(reader: &mut ByteReader) -> Result<SocketAddr> {
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
    fn an_address_survives_a_round_trip(#[case] address: &str) {
        let original: SocketAddr = address.parse().unwrap();
        let written = write_address(original);

        assert_eq!(
            original,
            read_address(&mut ByteReader::new(&written)).unwrap()
        );
    }

    #[test]
    fn a_v4_address_does_not_come_back_as_a_mapped_v6_one() {
        let written = write_address("127.0.0.1:34352".parse().unwrap());

        assert!(
            read_address(&mut ByteReader::new(&written))
                .unwrap()
                .is_ipv4(),
            "a v4 peer must not become v6 by round-tripping, or the address it \
             advertises is not one anybody can dial"
        );
    }
}
