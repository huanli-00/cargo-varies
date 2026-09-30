pub struct Counter {
    value: u32,
}

pub struct RestoreError;

impl Counter {
    pub fn new(seed: u32) -> Self {
        Self { value: seed }
    }

    pub fn recover(seed: u32) -> Result<Self, RestoreError> {
        Ok(Self { value: seed + 1 })
    }

    pub fn restore(seed: u32) -> Option<Self> {
        Some(Self { value: seed + 2 })
    }

    pub fn scrub(&mut self, delta: u32) {
        self.value = self.value.saturating_add(delta);
    }

    pub fn bump(&mut self) {
        unsafe {
            let ptr = &mut self.value as *mut u32;
            *ptr += 1;
        }
    }

    pub fn peek(&self) -> u32 {
        self.value
    }
}
