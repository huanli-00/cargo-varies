pub fn tweak(value: &mut u32, delta: u32) {
    *value = value.saturating_add(delta);
}

pub fn bump(value: &mut u32) {
    unsafe {
        let ptr = value as *mut u32;
        *ptr += 1;
    }
}

pub fn read(value: &u32) -> u32 {
    *value
}
