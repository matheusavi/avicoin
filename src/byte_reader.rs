pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn read_byte(&mut self) -> Result<u8, String> {
        if self.position + 1 >= self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let byte = self.bytes[self.position];
        self.position += 1;
        Ok(byte)
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        if self.position + 2 >= self.bytes.len() {
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

    fn read_u32(&mut self) -> Result<u32, String> {
        if self.position + 4 >= self.bytes.len() {
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

}
