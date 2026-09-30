pub struct PublicBuf {
    bytes: Vec<u8>,
}

impl PublicBuf {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl core::convert::AsRef<[u8]> for PublicBuf {
    fn as_ref(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.bytes.as_ptr(), self.bytes.len()) }
    }
}
