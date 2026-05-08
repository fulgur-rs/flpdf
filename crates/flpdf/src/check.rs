use crate::{Diagnostic, Diagnostics, Pdf};
use std::io::{Read, Seek};

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub valid: bool,
    pub diagnostics: Diagnostics,
}

pub fn check_reader<R: Read + Seek>(reader: R) -> crate::Result<CheckReport> {
    match Pdf::open(reader) {
        Ok(pdf) => {
            let mut diagnostics = Diagnostics::default();
            if pdf.trailer().get_ref("Root").is_none() {
                diagnostics.push(Diagnostic::error("trailer is missing /Root", None));
            }
            Ok(CheckReport {
                valid: !diagnostics.has_errors(),
                diagnostics,
            })
        }
        Err(error) => {
            let mut diagnostics = Diagnostics::default();
            diagnostics.push(Diagnostic::error(error.to_string(), None));
            Ok(CheckReport {
                valid: false,
                diagnostics,
            })
        }
    }
}
