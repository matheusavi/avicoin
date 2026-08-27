use crate::messages::message::Payload;
use crate::util::command_12;
use anyhow::{anyhow, Result};

pub const GETADDR_COMMAND_NAME: &str = "getaddr";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Getaddr;

impl Getaddr {
    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Getaddr> {
        if !bytes.is_empty() {
            return Err(anyhow!(
                "getaddr carries no payload, got {} bytes",
                bytes.len()
            ));
        }

        Ok(Getaddr)
    }
}

impl Payload for Getaddr {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(GETADDR_COMMAND_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_getaddr_survives_a_round_trip() {
        let raw = Getaddr.get_raw_format().unwrap();

        assert!(raw.is_empty());
        assert_eq!(Getaddr, Getaddr::parse_raw_format(raw).unwrap());
    }

    #[test]
    fn a_getaddr_carrying_a_payload_is_refused() {
        Getaddr::parse_raw_format(vec![0])
            .expect_err("a getaddr with a body is not one this node understands");
    }
}
