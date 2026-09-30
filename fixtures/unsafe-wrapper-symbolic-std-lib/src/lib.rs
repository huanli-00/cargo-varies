pub fn tuple_wrap(parts: (u8, u8)) {
    unsafe {
        let mut value = u32::from(parts.0) + u32::from(parts.1);
        let ptr = &mut value as *mut u32;
        *ptr += 1;
    }
}

pub fn array_wrap(bytes: [u8; 4]) {
    unsafe {
        let mut value = u32::from(bytes[0]) + u32::from(bytes[1]);
        let ptr = &mut value as *mut u32;
        *ptr += 1;
    }
}

pub fn option_wrap(value: Option<u8>) {
    unsafe {
        let mut raw = u32::from(value.unwrap_or(0));
        let ptr = &mut raw as *mut u32;
        *ptr += 1;
    }
}

pub fn result_wrap(value: Result<u8, bool>) {
    unsafe {
        let mut raw = u32::from(value.unwrap_or(0));
        let ptr = &mut raw as *mut u32;
        *ptr += 1;
    }
}

pub fn string_wrap(text: String) {
    unsafe {
        let mut value = text.len() as u32;
        let ptr = &mut value as *mut u32;
        *ptr += 1;
    }
}

pub fn vec_wrap(bytes: Vec<u8>) {
    unsafe {
        let mut value = bytes.len() as u32;
        let ptr = &mut value as *mut u32;
        *ptr += 1;
    }
}

pub fn borrow_string_wrap(text: &String) {
    unsafe {
        let ptr = text as *const String;
        let _ = (&*ptr).len();
    }
}

pub fn borrow_vec_wrap(bytes: &Vec<u8>) {
    unsafe {
        let ptr = bytes as *const Vec<u8>;
        let _ = (&*ptr).len();
    }
}

pub fn borrow_pair_vec_wrap(pairs: &Vec<(u8, bool)>) {
    unsafe {
        let ptr = pairs as *const Vec<(u8, bool)>;
        let _ = (&*ptr).len();
    }
}

pub fn mutate_string_wrap(text: &mut String) {
    unsafe {
        let ptr = text as *mut String;
        (*ptr).push('!');
    }
}

pub fn mutate_vec_wrap(bytes: &mut Vec<u8>) {
    unsafe {
        let ptr = bytes as *mut Vec<u8>;
        (*ptr).push(1);
    }
}

pub fn box_wrap(mut value: Box<u32>) {
    unsafe {
        let ptr = (&mut *value) as *mut u32;
        *ptr += 1;
    }
}

pub fn borrow_box_string_wrap(text: &Box<String>) {
    unsafe {
        let ptr = text as *const Box<String>;
        let _ = (&**ptr).len();
    }
}

pub fn option_box_string_wrap(value: Option<Box<String>>) {
    unsafe {
        let ptr = &value as *const Option<Box<String>>;
        let _ = (&*ptr).as_ref().map(|text| text.len());
    }
}

pub fn result_box_string_wrap(value: Result<Box<String>, bool>) {
    unsafe {
        let ptr = &value as *const Result<Box<String>, bool>;
        let _ = (&*ptr).as_ref().ok().map(|text| text.len());
    }
}

pub fn box_option_string_wrap(value: Box<Option<String>>) {
    unsafe {
        let ptr = &*value as *const Option<String>;
        let _ = (&*ptr).as_ref().map(|text| text.len());
    }
}

pub fn box_result_string_wrap(value: Box<Result<String, bool>>) {
    unsafe {
        let ptr = &*value as *const Result<String, bool>;
        let _ = (&*ptr).as_ref().ok().map(|text| text.len());
    }
}

pub fn vec_box_string_wrap(values: Vec<Box<String>>) {
    unsafe {
        let ptr = &values as *const Vec<Box<String>>;
        let _ = (&*ptr).iter().map(|text| text.len()).sum::<usize>();
    }
}

pub fn box_vec_option_string_wrap(values: Box<Vec<Option<String>>>) {
    unsafe {
        let ptr = &*values as *const Vec<Option<String>>;
        let _ = (&*ptr)
            .iter()
            .filter_map(|value| value.as_ref())
            .map(|text| text.len())
            .sum::<usize>();
    }
}

pub fn nested_tuple_wrap(values: (Box<String>, Option<String>)) {
    unsafe {
        let ptr = &values as *const (Box<String>, Option<String>);
        let _ = (&*ptr).0.len() + (&*ptr).1.as_ref().map(|text| text.len()).unwrap_or(0);
    }
}

pub fn nested_array_wrap(values: [Box<String>; 2]) {
    unsafe {
        let ptr = &values as *const [Box<String>; 2];
        let _ = (&*ptr)[0].len() + (&*ptr)[1].len();
    }
}

pub struct Counter {
    value: u32,
}

impl Counter {
    pub fn new(seed: u32) -> Self {
        Self { value: seed }
    }
}

pub fn write_counter(counter: *mut Counter) {
    unsafe {
        (*counter).value += 1;
    }
}

pub fn bump_scalar(value: *mut u32) {
    unsafe {
        *value += 1;
    }
}
