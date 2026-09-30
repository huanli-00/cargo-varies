pub struct Option;
pub struct Result;
pub struct String;
pub struct Vec;

pub fn touch(bytes: [u8; 1]) -> usize {
    unsafe { *bytes.get_unchecked(0) as usize }
}
