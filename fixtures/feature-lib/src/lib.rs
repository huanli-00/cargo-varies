#[cfg(feature = "extra")]
pub struct FeaturedCounter {
    value: u32,
}

#[cfg(feature = "extra")]
impl FeaturedCounter {
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

#[cfg(feature = "extra")]
pub fn make_counter(seed: u32) -> FeaturedCounter {
    FeaturedCounter::new(seed)
}

#[cfg(feature = "extra")]
pub fn inspect(counter: &FeaturedCounter) -> u32 {
    counter.value
}
