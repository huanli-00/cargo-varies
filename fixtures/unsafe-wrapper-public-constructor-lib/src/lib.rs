pub struct PublicBuf {
    bytes: Vec<u8>,
}

impl PublicBuf {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    pub fn touch(&self) -> usize {
        unsafe { *self.bytes.get_unchecked(0) as usize }
    }
}
