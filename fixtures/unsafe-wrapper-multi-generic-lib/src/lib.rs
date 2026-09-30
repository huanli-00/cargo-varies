pub fn join_touch<L: Into<String>, R: Into<Vec<u8>>>(left: L, right: R) -> usize {
    let left = left.into();
    let right = right.into();
    let total = left.len() + right.len();
    unsafe { std::ptr::read_volatile(&total) }
}

/// Trait with a provided method used to test `Self` seeding plus extra
/// generic argument resolution.
pub trait ByteBridge {
    fn append_touch<S: AsRef<[u8]>>(&self, suffix: S) -> usize {
        let total = self.as_ref().len() + suffix.as_ref().len();
        unsafe { std::ptr::read_volatile(&total) }
    }

    fn as_ref(&self) -> &[u8];
}

impl ByteBridge for [u8] {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
