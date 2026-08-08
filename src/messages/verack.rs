use crate::messages::message::Payload;
use crate::util::command_12;
use anyhow::{anyhow, Result};

pub const VERACK_COMMAND_NAME: &str = "verack";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verack;

impl Verack {
    pub fn parse_raw_format(bytes: Vec<u8>) -> Result<Verack> {
        if !bytes.is_empty() {
            return Err(anyhow!("verack carries no payload, got {} bytes", bytes.len()));
        }

        Ok(Verack)
    }
}

impl Payload for Verack {
    fn get_raw_format(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }

    fn get_command_name(&self) -> [u8; 12] {
        command_12(VERACK_COMMAND_NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verack_survives_a_round_trip() {
        let raw = Verack.get_raw_format().unwrap();

        assert!(raw.is_empty());
        assert_eq!(Verack, Verack::parse_raw_format(raw).unwrap());
    }

    #[test]
    fn a_verack_carrying_a_payload_is_refused() {
        Verack::parse_raw_format(vec![0])
            .expect_err("a verack with a body is not a verack this node understands");
    }
}
