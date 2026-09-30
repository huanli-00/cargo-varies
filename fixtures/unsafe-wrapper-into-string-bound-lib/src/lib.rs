use std::borrow::Cow;

pub fn into_string_touch<T: Into<String>>(value: T) -> usize {
    let owned = value.into();
    let total = owned.len();
    unsafe { core::ptr::read_volatile(&total) }
}

pub fn into_static_cow_touch<T: Into<Cow<'static, str>>>(value: T) -> usize {
    let owned = value.into();
    let total = owned.len();
    unsafe { core::ptr::read_volatile(&total) }
}
