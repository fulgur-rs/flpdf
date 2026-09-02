#[cfg(windows)]
pub const EOL: &str = "\r\n";

#[cfg(not(windows))]
pub const EOL: &str = "\n";
