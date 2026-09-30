mod inner {
    pub struct PublicBuf {
        pub bytes: [u8; 1],
    }
}

pub mod api {
    pub use super::inner::PublicBuf;
}

pub use api::*;

pub fn touch(buf: PublicBuf) -> usize {
    unsafe { *buf.bytes.get_unchecked(0) as usize }
}
