use anyhow::{anyhow, Result};

pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn take_array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.first_chunk::<N>().copied()
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
        self.take_array::<2>()
            .map(u16::from_le_bytes)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read u16"))
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        self.take_array::<4>()
            .map(u32::from_le_bytes)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read u32"))
    }

    pub fn read_i32(&mut self) -> Result<i32> {
        self.take_array::<4>()
            .map(i32::from_le_bytes)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read i32"))
    }

    pub fn read_u64(&mut self) -> Result<u64> {
        self.take_array::<8>()
            .map(u64::from_le_bytes)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read u64"))
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take_array::<N>()
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read array of {} bytes", N))
    }

    pub fn read_bytes(&mut self, size: usize) -> Result<Vec<u8>> {
        self.take(size)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| anyhow!("EOF: Not sufficient bytes to read vec of {} bytes", size))
    }

    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    /// A count is a claim, not a fact: nothing reserves capacity on one, and a
    /// count past what the remaining bytes could hold is refused here.
    pub fn read_count(&mut self, min_element_size: usize) -> Result<usize> {
        debug_assert!(min_element_size > 0, "an element occupies at least a byte");

        let claimed = self.read_compact()?;
        let possible = (self.remaining() / min_element_size) as u64;

        if claimed > possible {
            return Err(anyhow!(
                "a count of {claimed} needs at least {} bytes, and {} remain",
                claimed.saturating_mul(min_element_size as u64),
                self.remaining()
            ));
        }

        Ok(claimed as usize)
    }

    pub fn read_var_bytes(&mut self) -> Result<Vec<u8>> {
        let length = self.read_count(1)?;
        self.read_bytes(length)
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
    fn a_count_within_what_the_input_could_hold_is_returned() {
        let bytes = [2, 0, 0, 0, 0];
        let mut reader = ByteReader::new(&bytes);

        assert_eq!(reader.read_count(2).unwrap(), 2);
    }

    #[test]
    fn a_count_past_what_the_input_could_hold_is_refused() {
        let bytes = [3, 0, 0, 0, 0];
        let mut reader = ByteReader::new(&bytes);

        assert!(reader.read_count(2).is_err());
    }

    #[test]
    fn a_count_of_u64_max_is_refused_before_anything_is_reserved_for_it() {
        let bytes = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0];
        let mut reader = ByteReader::new(&bytes);

        assert!(reader.read_count(1).is_err());
    }

    #[test]
    fn a_count_leaves_the_reader_where_the_elements_begin() {
        let bytes = [1, 42, 43];
        let mut reader = ByteReader::new(&bytes);

        assert_eq!(reader.read_count(1).unwrap(), 1);
        assert_eq!(reader.read_byte().unwrap(), 42);
    }

    #[test]
    fn a_length_prefixed_string_of_bytes_reads_back() {
        let bytes = [3, 7, 8, 9, 10];
        let mut reader = ByteReader::new(&bytes);

        assert_eq!(reader.read_var_bytes().unwrap(), vec![7, 8, 9]);
        assert_eq!(reader.remaining(), 1);
    }

    #[test]
    fn a_length_longer_than_the_bytes_behind_it_is_refused() {
        let bytes = [4, 7, 8, 9];
        let mut reader = ByteReader::new(&bytes);

        assert!(reader.read_var_bytes().is_err());
    }

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

    type Read = fn(&mut ByteReader) -> Result<()>;

    fn a_byte(r: &mut ByteReader) -> Result<()> {
        r.read_byte().map(|_| ())
    }
    fn an_u16(r: &mut ByteReader) -> Result<()> {
        r.read_u16().map(|_| ())
    }
    fn an_u32(r: &mut ByteReader) -> Result<()> {
        r.read_u32().map(|_| ())
    }
    fn an_i32(r: &mut ByteReader) -> Result<()> {
        r.read_i32().map(|_| ())
    }
    fn an_u64(r: &mut ByteReader) -> Result<()> {
        r.read_u64().map(|_| ())
    }
    fn array32(r: &mut ByteReader) -> Result<()> {
        r.read_array::<32>().map(|_| ())
    }
    fn bytes7(r: &mut ByteReader) -> Result<()> {
        r.read_bytes(7).map(|_| ())
    }

    #[rstest]
    #[case::byte(1, a_byte as Read)]
    #[case::u16(2, an_u16 as Read)]
    #[case::u32(4, an_u32 as Read)]
    #[case::i32(4, an_i32 as Read)]
    #[case::u64(8, an_u64 as Read)]
    #[case::array(32, array32 as Read)]
    #[case::bytes(7, bytes7 as Read)]
    fn exactly_enough_bytes_succeeds(#[case] width: usize, #[case] read: Read) {
        let bytes = vec![0u8; width];

        assert!(read(&mut ByteReader::new(&bytes)).is_ok());
    }

    #[rstest]
    #[case::byte(1, a_byte as Read)]
    #[case::u16(2, an_u16 as Read)]
    #[case::u32(4, an_u32 as Read)]
    #[case::i32(4, an_i32 as Read)]
    #[case::u64(8, an_u64 as Read)]
    #[case::array(32, array32 as Read)]
    #[case::bytes(7, bytes7 as Read)]
    fn one_byte_short_fails(#[case] width: usize, #[case] read: Read) {
        let bytes = vec![0u8; width - 1];

        assert!(read(&mut ByteReader::new(&bytes)).is_err());
    }

    #[rstest]
    #[case::byte(a_byte as Read)]
    #[case::u16(an_u16 as Read)]
    #[case::u32(an_u32 as Read)]
    #[case::i32(an_i32 as Read)]
    #[case::u64(an_u64 as Read)]
    #[case::array(array32 as Read)]
    #[case::bytes(bytes7 as Read)]
    fn a_reader_at_the_end_fails_every_read(#[case] read: Read) {
        let bytes = vec![0u8; 64];
        let mut reader = ByteReader::new(&bytes);
        reader.read_bytes(64).unwrap();

        assert!(read(&mut reader).is_err());
    }

    #[rstest]
    #[case::one_byte(&[0x07], 7)]
    #[case::two_byte(&[0xfd, 0x34, 0x12], 0x1234)]
    #[case::four_byte(&[0xfe, 0x78, 0x56, 0x34, 0x12], 0x1234_5678)]
    #[case::eight_byte(&[0xff, 1, 0, 0, 0, 0, 0, 0, 0], 1)]
    fn a_compact_int_reads_its_full_width(#[case] encoded: &[u8], #[case] expected: u64) {
        assert_eq!(expected, ByteReader::new(encoded).read_compact().unwrap());
    }

    #[rstest]
    #[case::two_byte_prefix_alone(&[0xfd])]
    #[case::two_byte_one_short(&[0xfd, 0x34])]
    #[case::four_byte_one_short(&[0xfe, 0x78, 0x56, 0x34])]
    #[case::eight_byte_one_short(&[0xff, 1, 0, 0, 0, 0, 0, 0])]
    fn a_truncated_compact_int_fails(#[case] encoded: &[u8]) {
        assert!(ByteReader::new(encoded).read_compact().is_err());
    }

    #[rstest]
    #[case::max(usize::MAX)]
    #[case::one_below_max(usize::MAX - 1)]
    #[case::exactly_enough_to_wrap_to_zero(usize::MAX - 5 + 1)]
    fn a_size_that_would_wrap_the_cursor_is_rejected(#[case] size: usize) {
        let bytes = [0u8; 16];
        let mut reader = ByteReader::new(&bytes);
        reader.read_bytes(5).unwrap();

        assert!(
            reader.read_bytes(size).is_err(),
            "position 5 + {size} wraps, which would let an unchecked guard pass"
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
