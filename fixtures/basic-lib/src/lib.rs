pub struct Counter {
    value: u32,
}

impl Counter {
    pub fn new(seed: u32) -> Self {
        Self { value: seed }
    }

    pub fn bump(&mut self, delta: u32) {
        self.value += delta;
    }

    pub fn get(&self) -> u32 {
        self.value
    }
}

pub fn make_counter(seed: u32) -> Counter {
    Counter::new(seed)
}

pub fn consume(counter: Counter) -> u32 {
    counter.value
}

pub fn inspect(counter: &Counter) -> u32 {
    counter.value
}
