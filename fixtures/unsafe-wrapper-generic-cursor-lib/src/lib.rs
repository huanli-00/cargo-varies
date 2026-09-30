use std::io::{Cursor, Read};

macro_rules! unit_readers {
    ($($name:ident),* $(,)?) => {
        $(
            pub struct $name;

            impl Read for $name {
                fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                    let len = out.len().min(1);
                    if len == 1 {
                        out[0] = 1;
                    }
                    Ok(len)
                }
            }
        )*
    };
}

unit_readers!(
    Reader00,
    Reader01,
    Reader02,
    Reader03,
    Reader04,
    Reader05,
    Reader06,
    Reader07,
    Reader08,
    Reader09,
    Reader10,
    Reader11,
    Reader12,
    Reader13,
    Reader14,
);

pub fn owned_cursor() -> Cursor<Vec<u8>> {
    Cursor::new(vec![1, 2, 3, 4])
}

pub fn borrowed_cursor(data: &[u8]) -> Cursor<&[u8]> {
    Cursor::new(data)
}

pub fn cursor_touch<R: Read>(mut reader: R) {
    let mut byte = [0u8; 1];
    let _ = reader.read(&mut byte);
    unsafe {
        let ptr = &byte as *const _;
        std::ptr::read_volatile(&ptr);
    }
}
