//! qpdf correspondence: `QPDFObjGen` identity and command-line object-reference parsing.

use std::fmt;
use std::str::FromStr;

/// Indirect-object identity (N G R in PDF syntax).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectRef {
    /// Object number.
    pub number: u32,
    /// Generation number.
    pub generation: u16,
}

impl ObjectRef {
    /// Construct an object identity from its number and generation.
    pub fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }

    /// Parse N G or N G R, matching the qpdf CLI spelling.
    pub fn parse(input: &str) -> std::result::Result<Self, ParseObjectRefError> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        if (parts.len() != 2 && parts.len() != 3) || (parts.len() == 3 && parts[2] != "R") {
            return Err(ParseObjectRefError::new(format!(
                "invalid object ref '{input}'"
            )));
        }
        let number = parts[0]
            .parse::<u32>()
            .map_err(|_| ParseObjectRefError::new(format!("invalid object number in '{input}'")))?;
        let generation = parts[1].parse::<u16>().map_err(|_| {
            ParseObjectRefError::new(format!("invalid object generation in '{input}'"))
        })?;
        Ok(Self::new(number, generation))
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {} R", self.number, self.generation)
    }
}

impl FromStr for ObjectRef {
    type Err = ParseObjectRefError;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(input)
    }
}

/// Error returned when an object-reference string is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseObjectRefError {
    message: String,
}

impl ParseObjectRefError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ParseObjectRefError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ParseObjectRefError {}

#[cfg(test)]
mod tests {
    use super::ObjectRef;

    #[test]
    fn malformed_reference_error_displays_its_qpdf_style_message() {
        let error = ObjectRef::parse("7 nope").expect_err("generation must be numeric");
        assert_eq!(error.to_string(), "invalid object generation in '7 nope'");
    }
}
