pub struct Handle {
    value: u32,
}

impl Handle {
    pub fn one(seed: u32) -> Self {
        Self { value: seed + 1 }
    }

    pub fn two(seed: u32) -> Self {
        Self { value: seed + 2 }
    }

    pub fn three(seed: u32) -> Self {
        Self { value: seed + 3 }
    }

    pub fn four(seed: u32) -> Self {
        Self { value: seed + 4 }
    }

    pub fn five(seed: u32) -> Self {
        Self { value: seed + 5 }
    }

    pub fn six(seed: u32) -> Self {
        Self { value: seed + 6 }
    }

    pub fn seven(seed: u32) -> Self {
        Self { value: seed + 7 }
    }
}

pub fn pair_touch(left: Handle, right: Handle) -> u32 {
    unsafe {
        let ptr = &left.value as *const u32;
        *ptr + right.value
    }
}

pub fn observe(handle: &Handle) -> u32 {
    handle.value
}
