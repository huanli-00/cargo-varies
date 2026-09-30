use std::sync::Arc;

pub struct Token<T>(pub Arc<T>);

pub struct Wrapper<T>(pub Token<T>);

pub fn make_arc<T: Default>() -> Arc<T> {
    Arc::new(T::default())
}

pub fn make_token<T>(value: Arc<T>) -> Token<T> {
    Token(value)
}

pub fn wrap_token<T>(value: Token<T>) -> Wrapper<T> {
    Wrapper(value)
}

pub fn touch(wrapper: Wrapper<u8>) -> u8 {
    unsafe {
        let ptr = Arc::as_ptr(&wrapper.0.0);
        *ptr
    }
}
