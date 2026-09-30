use crate::cli::LogLevel;
use log::{LevelFilter, Log, Metadata, Record};
use std::io::{self, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

static LOGGER: VariesLogger = VariesLogger;
static INIT_LOGGER: Once = Once::new();
static ACTIVE_LEVEL: AtomicUsize = AtomicUsize::new(LogLevel::Info as usize);

struct VariesLogger;

pub(crate) fn init(level: LogLevel) {
    ACTIVE_LEVEL.store(level as usize, Ordering::Relaxed);
    INIT_LOGGER.call_once(|| {
        log::set_logger(&LOGGER).expect("varies logger should install once");
    });
    log::set_max_level(level.to_level_filter());
}

impl Log for VariesLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        if !is_varies_target(metadata.target()) {
            return false;
        }
        metadata.level() <= active_level().to_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let mut stderr = io::stderr().lock();
        let _ = writeln!(
            stderr,
            "[varies][{}][{}] {}",
            record.level().as_str().to_ascii_lowercase(),
            format_target(record.target()),
            record.args()
        );
    }

    fn flush(&self) {}
}

fn active_level() -> LogLevel {
    match ACTIVE_LEVEL.load(Ordering::Relaxed) {
        0 => LogLevel::Error,
        1 => LogLevel::Warn,
        2 => LogLevel::Info,
        3 => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

fn is_varies_target(target: &str) -> bool {
    target == "varies" || target.starts_with("varies::")
}

fn format_target(target: &str) -> &str {
    target.strip_prefix("varies::").unwrap_or(target)
}

impl LogLevel {
    pub(crate) fn to_level_filter(self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::Error,
            Self::Warn => LevelFilter::Warn,
            Self::Info => LevelFilter::Info,
            Self::Debug => LevelFilter::Debug,
            Self::Trace => LevelFilter::Trace,
        }
    }

    fn to_level(self) -> log::Level {
        match self {
            Self::Error => log::Level::Error,
            Self::Warn => log::Level::Warn,
            Self::Info => log::Level::Info,
            Self::Debug => log::Level::Debug,
            Self::Trace => log::Level::Trace,
        }
    }
}
