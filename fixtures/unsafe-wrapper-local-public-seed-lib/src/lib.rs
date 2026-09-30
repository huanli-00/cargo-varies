#[derive(Clone, Copy, Default)]
pub struct Seed {
    pub byte: u8,
}

pub fn local_copy_touch<T: Copy>(value: T) -> usize {
    let total = core::mem::size_of_val(&value);
    unsafe { core::ptr::read_volatile(&total) }
}
