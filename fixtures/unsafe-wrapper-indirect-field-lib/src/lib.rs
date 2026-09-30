pub struct Counter {
    value: u32,
    label: String,
}

impl Counter {
    pub fn new(seed: u32) -> Self {
        Self {
            value: seed,
            label: String::new(),
        }
    }

    pub fn rename(&mut self, label: String) {
        self.label = label;
    }

    pub fn bump_core(&mut self) {
        unsafe {
            let ptr = &mut self.value as *mut u32;
            *ptr += 1;
        }
    }

    pub fn bump_twice(&mut self) {
        self.bump_core();
        self.bump_core();
    }

    pub fn bump_alias(&mut self) {
        self.bump_twice();
    }

    pub fn value(&self) -> u32 {
        self.value
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}
