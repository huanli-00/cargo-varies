pub struct Session {
    total: u32,
}

pub struct OpenError;

pub fn open(seed: u32) -> Result<Session, OpenError> {
    Ok(Session { total: seed })
}

pub fn open_optional(seed: u32) -> Option<Session> {
    Some(Session { total: seed })
}

pub fn bump(session: &mut Session, delta: u32) {
    session.total += delta;
}

pub fn inspect(session: &Session) -> u32 {
    session.total
}
