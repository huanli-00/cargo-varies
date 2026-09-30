mod inner {
    pub struct Counter {
        value: u32,
    }

    impl Counter {
        pub fn new(seed: u32) -> Self {
            Self { value: seed }
        }

        pub fn bump(&mut self) {
            unsafe {
                let ptr = &mut self.value as *mut u32;
                *ptr += 1;
            }
        }

        pub fn read(&self) -> u32 {
            self.value
        }
    }

    pub fn wrap_bump(counter: &mut Counter) {
        unsafe {
            let ptr = &mut counter.value as *mut u32;
            *ptr += 1;
        }
    }
}

pub use inner::Counter as PublicCounter;
pub use inner::wrap_bump as poke;
