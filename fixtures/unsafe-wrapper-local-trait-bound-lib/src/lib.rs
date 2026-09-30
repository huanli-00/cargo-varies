/// Local trait bound used to exercise generic candidate seeding from impls.
pub trait LocalSink<Rhs> {
    fn touch(&self, rhs: Rhs) -> usize;
}

impl<Rhs> LocalSink<Rhs> for String
where
    Rhs: Copy,
{
    fn touch(&self, rhs: Rhs) -> usize {
        let _ = rhs;
        let total = self.len();
        unsafe { std::ptr::read_volatile(&total) }
    }
}

pub fn local_into_vec_touch<T, R>(value: T, rhs: R) -> usize
where
    T: LocalSink<R>,
{
    let total = value.touch(rhs);
    unsafe { std::ptr::read_volatile(&total) }
}

#[derive(Default)]
pub struct Seed {
    pub byte: u8,
}

#[derive(Default)]
pub struct Wrapper<T: Default>(pub T);

/// Marker trait used to test local-bound seeding from free functions and impls.
pub trait LocalMarker {}

impl LocalMarker for Seed {}
impl<T: Default> LocalMarker for Wrapper<T> {}

pub fn local_marker_touch<T>() -> usize
where
    T: LocalMarker + Default,
{
    let value = T::default();
    let total = core::mem::size_of_val(&value);
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn local_marker_wrap_touch<T>() -> usize
where
    T: LocalMarker + Default,
{
    let value = T::default();
    let total = core::mem::size_of_val(&value);
    unsafe { core::ptr::read_volatile(&total) }
}

/// Local trait with a provided method used to test seeding `Self` from
/// generic impl receivers.
pub trait LocalBridge {
    fn wrap_touch(&self) -> usize {
        let total = core::mem::size_of_val(self);
        unsafe { core::ptr::read_volatile(&total) }
    }
}

impl<T: Default> LocalBridge for Wrapper<T> {}
