#[derive(PartialEq, PartialOrd, Debug, Default)]
pub enum LogLevel {
    #[default]
    NONE,
    INFO,
    DEBUG,
}

impl LogLevel {
    fn to_string(&self) -> &str {
        match self {
            LogLevel::NONE => "NONE",
            LogLevel::INFO => "INFO",
            LogLevel::DEBUG => "DEBUG",
        }
    }
}

pub fn to_loglevel(ulevel: u64) -> LogLevel {
    match ulevel {
        0 => LogLevel::NONE,
        1 => LogLevel::INFO,
        2 => LogLevel::DEBUG,
        _ => LogLevel::NONE,
    }
}

pub struct Logger {
    level: LogLevel,
}

impl Logger {
    pub fn new(ulevel: u64) -> Self {
        let level = to_loglevel(ulevel);
        Logger { level }
    }

    pub fn silent() -> Self {
        Logger { level: LogLevel::NONE }
    }

    pub fn log(&self, level: LogLevel, args: std::fmt::Arguments<'_>) {
        if level > self.level {
            return;
        }
        // TODO: cycle
        println!("[{}] {}", level.to_string(), args);
    }
}

#[macro_export]
macro_rules! log {
    // usage: log!(logger, "a {} event", "clock")
    ($logger:expr, $level:expr, $($arg:tt)+) => {{
        $logger.log($level, format_args!($($arg)+));
    }};
}
#[macro_export]
macro_rules! info {
    ($logger:expr, $($arg:tt)+) => ( $crate::log!($logger, $crate::sim::log::LogLevel::INFO, $($arg)+); )
}
#[macro_export]
macro_rules! debug {
    ($logger:expr, $($arg:tt)+) => ( $crate::log!($logger, $crate::sim::log::LogLevel::DEBUG, $($arg)+); )
}