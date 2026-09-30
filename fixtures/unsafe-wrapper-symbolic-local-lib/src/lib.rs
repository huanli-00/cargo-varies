pub enum LocalPayload {
    Empty,
    Text(String),
    Combo(Box<String>, Option<String>),
    State { result: Result<String, bool> },
}

pub fn local_payload_wrap(value: LocalPayload) {
    unsafe {
        let ptr = &value as *const LocalPayload;
        let _ = match &*ptr {
            LocalPayload::Empty => 0,
            LocalPayload::Text(text) => text.len(),
            LocalPayload::Combo(label, note) => label.len() + note.as_ref().map_or(0, String::len),
            LocalPayload::State { result } => result.as_ref().ok().map_or(0, String::len),
        };
    }
}

pub fn option_local_payload_wrap(value: Option<LocalPayload>) {
    unsafe {
        let ptr = &value as *const Option<LocalPayload>;
        let _ = (&*ptr).as_ref().map(|payload| match payload {
            LocalPayload::Empty => 0,
            LocalPayload::Text(text) => text.len(),
            LocalPayload::Combo(label, note) => label.len() + note.as_ref().map_or(0, String::len),
            LocalPayload::State { result } => result.as_ref().ok().map_or(0, String::len),
        });
    }
}

pub fn result_local_payload_wrap(value: Result<LocalPayload, bool>) {
    unsafe {
        let ptr = &value as *const Result<LocalPayload, bool>;
        let _ = (&*ptr).as_ref().ok().map(|payload| match payload {
            LocalPayload::Empty => 0,
            LocalPayload::Text(text) => text.len(),
            LocalPayload::Combo(label, note) => label.len() + note.as_ref().map_or(0, String::len),
            LocalPayload::State { result } => result.as_ref().ok().map_or(0, String::len),
        });
    }
}

pub fn box_local_payload_wrap(value: Box<LocalPayload>) {
    unsafe {
        let ptr = &*value as *const LocalPayload;
        let _ = match &*ptr {
            LocalPayload::Empty => 0,
            LocalPayload::Text(text) => text.len(),
            LocalPayload::Combo(label, note) => label.len() + note.as_ref().map_or(0, String::len),
            LocalPayload::State { result } => result.as_ref().ok().map_or(0, String::len),
        };
    }
}

pub struct LocalRecord {
    pub label: Box<String>,
    pub note: Option<String>,
    pub result: Result<String, bool>,
}

pub struct LocalTuple(pub String, pub Option<String>);

pub fn local_record_wrap(value: LocalRecord) {
    unsafe {
        let ptr = &value as *const LocalRecord;
        let _ = (&*ptr).label.len()
            + (&*ptr).note.as_ref().map_or(0, String::len)
            + (&*ptr).result.as_ref().ok().map_or(0, String::len);
    }
}

pub fn box_local_record_wrap(value: Box<LocalRecord>) {
    unsafe {
        let ptr = &*value as *const LocalRecord;
        let _ = (&*ptr).label.len()
            + (&*ptr).note.as_ref().map_or(0, String::len)
            + (&*ptr).result.as_ref().ok().map_or(0, String::len);
    }
}

pub fn local_tuple_wrap(value: LocalTuple) {
    unsafe {
        let ptr = &value as *const LocalTuple;
        let _ = (&*ptr).0.len() + (&*ptr).1.as_ref().map_or(0, String::len);
    }
}

pub fn option_local_tuple_wrap(value: Option<LocalTuple>) {
    unsafe {
        let ptr = &value as *const Option<LocalTuple>;
        let _ = (&*ptr)
            .as_ref()
            .map(|tuple| tuple.0.len() + tuple.1.as_ref().map_or(0, String::len));
    }
}
