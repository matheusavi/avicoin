pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub fn read_byte(&mut self) -> Result<u8, String> {
        if self.position >= self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let byte = self.bytes[self.position];
        self.position += 1;
        Ok(byte)
    }

    pub fn read_u16(&mut self) -> Result<u16, String> {
        if self.position + 2 > self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let byte = u16::from_le_bytes(
            self.bytes[self.position..self.position + 2]
                .try_into()
                .map_err(|_| String::from("Invalid u16 bytes"))?,
        );
        self.position += 2;
        Ok(byte)
    }

    pub fn read_u32(&mut self) -> Result<u32, String> {
        if self.position + 4 > self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let byte = u32::from_le_bytes(
            self.bytes[self.position..self.position + 4]
                .try_into()
                .map_err(|_| String::from("Invalid u32 bytes"))?,
        );
        self.position += 4;
        Ok(byte)
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>, String> {
        if self.position + len > self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let bytes = self.bytes[self.position..self.position + len].to_vec();
        self.position += len;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_byte_empty() {
        let bytes = [];
        let mut reader = ByteReader::new(&bytes);
        let result = reader.read_byte();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "EOF");
    }

    #[test]
    fn test_mixed_type_reads() {
        let bytes = [42, 1, 2, 3, 4, 5, 6, 7, 8];
        let mut reader = ByteReader::new(&bytes);

        assert_eq!(reader.read_byte().unwrap(), 42);

        assert_eq!(reader.read_u32().unwrap(), 0x04030201);

        assert_eq!(reader.read_u16().unwrap(), 0x0605);

        assert_eq!(reader.read_byte().unwrap(), 7);

        assert_eq!(reader.read_byte().unwrap(), 8);

        assert!(reader.read_byte().is_err());
    }
}
