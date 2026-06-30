// ANSI color codes for terminal output

/// Yellow color for labels (DEBUG, WARNING, etc.)
pub const YELLOW: &str = "\x1b[33m";
/// Green color for commands and INFO
pub const GREEN: &str = "\x1b[32m";
/// Red color for ERROR
pub const RED: &str = "\x1b[31m";
/// Reset color to default
pub const RESET: &str = "\x1b[0m";

// ---- Const macros for use in const fn ----

/// Wrap text with yellow color (for use in const context)
#[macro_export]
macro_rules! yellow {
    ($text:expr) => {
        concat!("\x1b[33m", $text, "\x1b[0m")
    };
}

/// Wrap text with green color (for use in const context)
#[macro_export]
macro_rules! green {
    ($text:expr) => {
        concat!("\x1b[32m", $text, "\x1b[0m")
    };
}

// ---- Colored log macros (crate-level export) ----

/// Colored debug output: [DEBUG] in yellow
#[macro_export]
macro_rules! debug_log {
    ($debug:expr, $($arg:tt)*) => {
        if $debug { eprintln!("{}[DEBUG]{} {}", $crate::common::color::YELLOW, $crate::common::color::RESET, format!($($arg)*)); }
    };
}

/// Colored info output: [INFO] in green
#[macro_export]
macro_rules! info_log {
    ($($arg:tt)*) => {
        eprintln!("{}[INFO]{}  {}", $crate::common::color::GREEN, $crate::common::color::RESET, format!($($arg)*));
    };
}

/// Colored warning output: [WARNING] in yellow
#[macro_export]
macro_rules! warning_log {
    ($($arg:tt)*) => {
        eprintln!("{}[WARNING]{} {}", $crate::common::color::YELLOW, $crate::common::color::RESET, format!($($arg)*));
    };
}

/// Colored error output: [ERROR] in red
#[macro_export]
macro_rules! error_log {
    ($($arg:tt)*) => {
        eprintln!("{}[ERROR]{}  {}", $crate::common::color::RED, $crate::common::color::RESET, format!($($arg)*));
    };
}
