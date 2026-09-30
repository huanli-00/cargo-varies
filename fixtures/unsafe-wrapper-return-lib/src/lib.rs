pub struct Counter {
    value: u32,
}

pub fn wrap(seed: u32) -> Counter {
    unsafe {
        let ptr = &seed as *const u32;
        Counter { value: *ptr + 1 }
    }
}

pub fn scrub(counter: &mut Counter, delta: u32) {
    counter.value = counter.value.saturating_add(delta);
}

pub fn read(counter: &Counter) -> u32 {
    counter.value
}
