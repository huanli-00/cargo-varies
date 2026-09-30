mod inner {
    pub struct Counter {
        value: u32,
    }

    pub fn new_counter(seed: u32) -> Counter {
        Counter { value: seed }
    }

    pub fn bump(counter: &mut Counter) {
        unsafe {
            let ptr = &mut counter.value as *mut u32;
            *ptr += 1;
        }
    }

    pub fn read(counter: &Counter) -> u32 {
        counter.value
    }
}

pub use inner::{Counter, bump, new_counter, read};
