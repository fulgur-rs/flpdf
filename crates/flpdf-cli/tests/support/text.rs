#[cfg(windows)]
pub const EOL: &str = "\r\n";

#[cfg(not(windows))]
pub const EOL: &str = "\n";

#[allow(dead_code)]
pub fn platform_text(text: &str) -> String {
    text.replace('\n', EOL)
}
