pub struct WorkspaceCounter {
    total: u32,
}

impl WorkspaceCounter {
    pub fn new(seed: u32) -> Self {
        Self { total: seed }
    }

    pub fn push(&mut self, delta: u32) {
        self.total += delta;
    }

    pub fn total(&self) -> u32 {
        self.total
    }
}

pub fn make_counter(seed: u32) -> WorkspaceCounter {
    WorkspaceCounter::new(seed)
}

pub fn inspect(counter: &WorkspaceCounter) -> u32 {
    counter.total
}
