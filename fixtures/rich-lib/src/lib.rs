pub struct Session {
    total: u32,
    labels: Vec<String>,
}

pub struct OpenError;

pub fn bootstrap(seed: u32) -> Session {
    Session {
        total: seed,
        labels: Vec::new(),
    }
}

pub fn recover(seed: u32) -> Result<Session, OpenError> {
    Ok(Session {
        total: seed + 1,
        labels: vec!["recovered".to_owned()],
    })
}

pub fn restore(seed: u32) -> Option<Session> {
    Some(Session {
        total: seed + 2,
        labels: vec!["restored".to_owned()],
    })
}

pub fn advance(session: &mut Session, delta: u32) {
    session.total += delta;
}

pub fn relabel(session: &mut Session, label: String) {
    session.labels.push(label);
}

pub fn snapshot(session: &Session) -> u32 {
    session.total + session.labels.len() as u32
}

pub fn finish(session: Session) -> usize {
    session.total as usize + session.labels.len()
}
