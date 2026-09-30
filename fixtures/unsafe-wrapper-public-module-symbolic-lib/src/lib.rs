pub mod api {
    pub struct PublicBuf {
        pub bytes: [u8; 1],
    }
}

pub fn touch(buf: api::PublicBuf) -> usize {
    unsafe { *buf.bytes.get_unchecked(0) as usize }
}
