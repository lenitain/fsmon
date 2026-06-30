// ANSI color codes for terminal output

/// Yellow color for labels (DEBUG, INFO, etc.)
pub const YELLOW: &str = "\x1b[33m";
/// Green color for commands
pub const GREEN: &str = "\x1b[32m";
/// Reset color to default
pub const RESET: &str = "\x1b[0m";

/// Helper macro for colored debug output
#[macro_export]
macro_rules! debug_log_color {
    ($($arg:tt)*) => {
        eprintln!("{}[DEBUG]{} {}", $crate::common::color::YELLOW, $crate::common::color::RESET, format!($($arg)*));
    };
}

/// Helper macro for colored info output
#[macro_export]
macro_rules! info_log_color {
    ($($arg:tt)*) => {
        eprintln!("{}[INFO]{}  {}", $crate::common::color::GREEN, $crate::common::color::RESET, format!($($arg)*));
    };
}
