mod inner {
    pub struct PublicBuf {
        pub bytes: [u8; 1],
    }
}

pub use inner::PublicBuf;

pub fn touch(buf: PublicBuf) -> usize {
    unsafe { *buf.bytes.get_unchecked(0) as usize }
}
