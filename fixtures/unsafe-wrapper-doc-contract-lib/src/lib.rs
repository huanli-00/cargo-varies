pub fn make_data(seed: u8) -> Vec<u8> {
    vec![seed, seed.wrapping_add(1)]
}

pub fn make_noise(seed: u8) -> u8 {
    seed.wrapping_add(10)
}

/// # Safety
/// `data` must contain initialized bytes and `len` must not exceed `data.len()`.
unsafe fn inspect_core(data: Vec<u8>, len: usize, noise: u8) -> usize {
    data.iter().take(len).map(|value| usize::from(*value)).sum::<usize>() + usize::from(noise)
}

pub fn inspect(data: Vec<u8>, noise: u8) -> usize {
    unsafe { inspect_core(data, 1, noise) }
}
