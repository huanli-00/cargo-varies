use std::io::{Cursor, Read};
use std::rc::Rc;

pub struct RcReader {
    buf: Rc<Vec<u8>>,
}

impl Read for RcReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let len = out.len().min(self.buf.len());
        out[..len].copy_from_slice(&self.buf[..len]);
        Ok(len)
    }
}

pub fn rc_reader() -> RcReader {
    RcReader {
        buf: Rc::new(vec![1, 2, 3, 4]),
    }
}

pub fn cursor_reader() -> Cursor<Vec<u8>> {
    Cursor::new(vec![1, 2, 3, 4])
}

pub fn boxed_cursor() -> Box<Cursor<Vec<u8>>> {
    Box::new(cursor_reader())
}

pub fn touch_cursor(cursor: Cursor<u8>) {
    unsafe {
        let ptr = &cursor as *const _;
        std::ptr::read_volatile(&ptr);
    }
}

pub fn touch(reader: &(dyn Read + Send)) {
    unsafe {
        let ptr = reader as *const _;
        std::ptr::read_volatile(&ptr);
    }
}

pub fn boxed_touch(reader: Box<dyn Read + Send>) {
    unsafe {
        let ptr = &reader as *const _;
        std::ptr::read_volatile(&ptr);
    }
}

pub fn touch_static(reader: &'static (dyn Read + Send)) {
    unsafe {
        let ptr = reader as *const _;
        std::ptr::read_volatile(&ptr);
    }
}
