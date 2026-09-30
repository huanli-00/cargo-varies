pub struct Counter {
    value: u32,
}

impl Counter {
    pub fn new(seed: u32) -> Self {
        Self { value: seed }
    }

    pub fn rewrite(&mut self, seed: u32) {
        *self = Self::new(seed);
    }

    pub fn bump(&mut self) {
        unsafe {
            let ptr = &mut self.value as *mut u32;
            *ptr += 1;
        }
    }

    pub fn value(&self) -> u32 {
        self.value
    }
}
