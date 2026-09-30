pub struct Seed {
    pub byte: u8,
}

pub fn make_seed() -> Seed {
    Seed { byte: 1 }
}

pub fn copy_touch<T: Copy>(value: T) -> usize {
    let total = core::mem::size_of_val(&value);
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn clone_touch<T: Clone>(value: T) -> usize {
    let cloned = value.clone();
    let total = core::mem::size_of_val(&cloned);
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn debug_touch<T: core::fmt::Debug>(value: T) -> usize {
    let rendered = format!("{value:?}");
    let total = rendered.len();
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn default_touch<T: Default>(value: T) -> usize {
    let fallback = T::default();
    let total = core::mem::size_of_val(&value) + core::mem::size_of_val(&fallback);
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn partial_eq_touch<T: PartialEq>(left: T, right: T) -> usize {
    let total = usize::from(left == right);
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn eq_touch<T: Eq>(left: T, right: T) -> usize {
    let total = usize::from(left == right);
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn partial_ord_touch<T: PartialOrd>(left: T, right: T) -> usize {
    let total = usize::from(left.partial_cmp(&right).is_some());
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn ord_touch<T: Ord>(left: T, right: T) -> usize {
    let total = match left.cmp(&right) {
        core::cmp::Ordering::Less => 0,
        core::cmp::Ordering::Equal => 1,
        core::cmp::Ordering::Greater => 2,
    };
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn hash_touch<T: core::hash::Hash>(value: T) -> usize {
    use core::hash::{Hash, Hasher};

    let mut hasher = std::hash::DefaultHasher::new();
    value.hash(&mut hasher);
    let total = hasher.finish() as usize;
    unsafe { core::ptr::read_volatile(&total) }
}
