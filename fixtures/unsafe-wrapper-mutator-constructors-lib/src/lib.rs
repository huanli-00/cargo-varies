pub struct Counter {
    value: u32,
}

pub struct Token {
    delta: u32,
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
}

impl Token {
    pub fn alpha(seed: u32) -> Self {
        Self {
            delta: u32::from(seed % 3),
        }
    }

    pub fn beta(seed: u32) -> Self {
        Self {
            delta: u32::from(seed % 5),
        }
    }
}

pub fn tune(counter: &mut Counter, token: Token) {
    counter.value += token.delta;
}
