use anyhow::{anyhow, Context, Result};

pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub fn read_byte(&mut self) -> Result<u8> {
        if self.position >= self.bytes.len() {
            return Err(anyhow!("EOF: Not sufficient bytes to read byte"));
        }
        let result = self.bytes[self.position];
        self.position += 1;
        Ok(result)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        if self.position + 2 > self.bytes.len() {
            return Err(anyhow!("EOF: Not sufficient bytes to read u16"));
        }
        let result = u16::from_le_bytes(
            self.bytes[self.position..self.position + 2]
                .try_into()
                .context("Invalid u16 bytes")?,
        );
        self.position += 2;
        Ok(result)
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        if self.position + 4 > self.bytes.len() {
            return Err(anyhow!("EOF: Not sufficient bytes to read u32"));
        }
        let result = u32::from_le_bytes(
            self.bytes[self.position..self.position + 4]
                .try_into()
                .context("Invalid u32 bytes")?,
        );
        self.position += 4;
        Ok(result)
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        if self.position + 4 > self.bytes.len() {
            return Err(anyhow!("EOF: Not sufficient bytes to read i32"));
        }
        let result = i32::from_le_bytes(
            self.bytes[self.position..self.position + 4]
                .try_into()
                .context("Invalid i32 bytes")?,
        );
        self.position += 4;
        Ok(result)
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        if self.position + 8 > self.bytes.len() {
            return Err(anyhow!("EOF: Not sufficient bytes to read u64"));
        }
        let result = u64::from_le_bytes(
            self.bytes[self.position..self.position + 8]
                .try_into()
                .context("Invalid u64 bytes")?,
        );
        self.position += 8;
        Ok(result)
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.position + N > self.bytes.len() {
            return Err(anyhow!(
                "EOF: Not sufficient bytes to read array of {} bytes",
                N
            ));
        }

        let results = self.bytes[self.position..self.position + N]
            .try_into()
            .context("Invalid array")?;

        self.position += N;
        Ok(results)
    }

    pub fn read_compact(&mut self) -> Result<u64> {
        match self.read_byte()? {
            0xfd => Ok(self.read_u16()? as u64),
            0xfe => Ok(self.read_u32()? as u64),
            0xff => Ok(self.read_u64()?),
            v => Ok(v as u64),
        }
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
        assert_eq!(
            result.unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read byte"
        );
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
        assert_eq!(
            reader.read_byte().unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read byte"
        );
    }

    #[test]
    fn test_read_u16() {
        let bytes = [0x34, 0x12];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_u16().unwrap(), 0x1234);
        assert_eq!(
            reader.read_u16().unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read u16"
        );
    }

    #[test]
    fn test_read_u32() {
        let bytes = [0x78, 0x56, 0x34, 0x12];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_u32().unwrap(), 0x12345678);
        assert_eq!(
            reader.read_u32().unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read u32"
        );
    }

    #[test]
    fn test_read_i32() {
        let bytes = [0x78, 0x56, 0x34, 0x12];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_i32().unwrap(), 0x12345678);

        let bytes = [0xFF, 0xFF, 0xFF, 0xFF];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_i32().unwrap(), -1);
        assert_eq!(
            reader.read_i32().unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read i32"
        );
    }

    #[test]
    fn test_read_u64() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_u64().unwrap(), 0x0807060504030201);
        assert_eq!(
            reader.read_u64().unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read u64"
        );
    }

    #[test]
    fn test_read_array() {
        let bytes = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let mut reader = ByteReader::new(&bytes);

        assert_eq!(reader.read_array::<4>().unwrap(), [0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(reader.read_array::<1>().unwrap(), [0xEE]);
        assert_eq!(
            reader.read_array::<1>().unwrap_err().to_string(),
            "EOF: Not sufficient bytes to read array of 1 bytes"
        );
    }

    #[test]
    fn test_read_compact_single_byte() {
        let bytes = [0x42];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_compact().unwrap(), 0x42);
    }

    #[test]
    fn test_read_compact_two_bytes() {
        let bytes = [0xfd, 0x34, 0x12];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_compact().unwrap(), 0x1234);
    }

    #[test]
    fn test_read_compact_four_bytes() {
        let bytes = [0xfe, 0x78, 0x56, 0x34, 0x12];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_compact().unwrap(), 0x12345678);
    }

    #[test]
    fn test_read_compact_eight_bytes() {
        let bytes = [0xff, 0x21, 0x43, 0x65, 0x87, 0x09, 0xBA, 0xDC, 0xFE];
        let mut reader = ByteReader::new(&bytes);
        assert_eq!(reader.read_compact().unwrap(), 0xFEDCBA0987654321);
    }
}
