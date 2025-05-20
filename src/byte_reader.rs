pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_byte(&mut self) -> Result<u8, String> {
        if self.position >= self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let byte = self.bytes[self.position];
        self.position += 1;
        Ok(byte)
    }
    fn read_u16(&mut self) -> Result<u16, String> {
        if self.position >= self.bytes.len() {
            return Err(String::from("EOF"));
        }
        let byte = self.bytes[self.position];
        self.position += 1;
        Ok(byte)
    }
}
