use anyhow::{anyhow, Context, Result};

pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(count)?;
        if end > self.bytes.len() {
            return None;
        }

        let taken = &self.bytes[self.position..end];
        self.position = end;
        Some(taken)
    }

    pub fn read_byte(&mut self) -> Result<u8> {
        self.take(1)
            .map(|taken| taken[0])
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read byte"))
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        let taken = self
            .take(2)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read u16"))?;

        Ok(u16::from_le_bytes(
            taken.try_into().context("Invalid u16 bytes")?,
        ))
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        let taken = self
            .take(4)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read u32"))?;

        Ok(u32::from_le_bytes(
            taken.try_into().context("Invalid u32 bytes")?,
        ))
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        let taken = self
            .take(4)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read i32"))?;

        Ok(i32::from_le_bytes(
            taken.try_into().context("Invalid i32 bytes")?,
        ))
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        let taken = self
            .take(8)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read u64"))?;

        Ok(u64::from_le_bytes(
            taken.try_into().context("Invalid u64 bytes")?,
        ))
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let taken = self
            .take(N)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read array of {} bytes", N))?;

        taken.try_into().context("Invalid array")
    }

    pub fn read_bytes(&mut self, size: usize) -> Result<Vec<u8>> {
        self.take(size)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read vec of {} bytes", size))
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
    use rstest::rstest;

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

    fn read_of(width: usize) -> impl Fn(&mut ByteReader) -> Result<()> {
        move |reader| match width {
            1 => reader.read_byte().map(|_| ()),
            2 => reader.read_u16().map(|_| ()),
            4 => reader.read_u32().map(|_| ()),
            8 => reader.read_u64().map(|_| ()),
            32 => reader.read_array::<32>().map(|_| ()),
            _ => unreachable!("no read method of width {width}"),
        }
    }

    #[rstest]
    #[case::byte(1)]
    #[case::u16(2)]
    #[case::u32(4)]
    #[case::u64(8)]
    #[case::array(32)]
    fn exactly_enough_bytes_succeeds(#[case] width: usize) {
        let bytes = vec![0u8; width];
        let mut reader = ByteReader::new(&bytes);

        assert!(read_of(width)(&mut reader).is_ok(), "width {width}");
    }

    #[rstest]
    #[case::byte(1)]
    #[case::u16(2)]
    #[case::u32(4)]
    #[case::u64(8)]
    #[case::array(32)]
    fn one_byte_short_fails(#[case] width: usize) {
        let bytes = vec![0u8; width - 1];
        let mut reader = ByteReader::new(&bytes);

        assert!(read_of(width)(&mut reader).is_err(), "width {width}");
    }

    #[rstest]
    #[case::byte(1)]
    #[case::u16(2)]
    #[case::u32(4)]
    #[case::u64(8)]
    #[case::array(32)]
    fn a_reader_at_the_end_fails_every_read(#[case] width: usize) {
        let bytes = vec![0u8; 64];
        let mut reader = ByteReader::new(&bytes);
        reader.read_bytes(64).unwrap();

        assert!(read_of(width)(&mut reader).is_err(), "width {width}");
        assert!(reader.read_bytes(1).is_err());
        assert!(reader.read_compact().is_err());
    }

    #[rstest]
    #[case::max(usize::MAX)]
    #[case::one_below_max(usize::MAX - 1)]
    #[case::half_the_range(usize::MAX / 2)]
    fn a_size_that_would_wrap_the_cursor_is_rejected(#[case] size: usize) {
        let bytes = [0u8; 16];
        let mut reader = ByteReader::new(&bytes);
        reader.read_bytes(5).unwrap();

        let result = reader.read_bytes(size);

        assert!(
            result.is_err(),
            "position + {size} wraps below the buffer length, which would let the check pass"
        );
    }

    #[test]
    fn a_failed_read_does_not_move_the_cursor() {
        let bytes = [1u8, 2, 3];
        let mut reader = ByteReader::new(&bytes);

        assert!(reader.read_u64().is_err());
        assert!(reader.read_bytes(usize::MAX).is_err());

        assert_eq!(
            1,
            reader.read_byte().unwrap(),
            "the cursor must not have moved"
        );
    }
}
