pub struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// Trait-default constructor fixture that turns a slice-like receiver into
/// an iterator-backed cursor.
pub trait ByteCursor {
    fn as_bytes(&self) -> &[u8];

    fn cursor(&self) -> Cursor<'_> {
        Cursor {
            bytes: self.as_bytes(),
            pos: 0,
        }
    }
}

impl ByteCursor for [u8] {
    fn as_bytes(&self) -> &[u8] {
        self
    }
}

impl<'a> Iterator for Cursor<'a> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.pos >= self.bytes.len() {
            return None;
        }

        let index = self.pos;
        self.pos += 1;
        Some(unsafe { *self.bytes.get_unchecked(index) })
    }
}
