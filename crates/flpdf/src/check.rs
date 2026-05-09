use crate::{Diagnostic, Diagnostics, Pdf};
use std::io::{Read, Seek};

#[derive(Debug, Clone)]
pub struct CheckReport {
    pub valid: bool,
    pub diagnostics: Diagnostics,
}

pub fn check_reader<R: Read + Seek>(reader: R) -> crate::Result<CheckReport> {
    let mut pdf = match Pdf::open_with_repair(reader) {
        Ok(pdf) => pdf,
        Err(error) => {
            let mut diagnostics = Diagnostics::default();
            diagnostics.push(Diagnostic::error(error.to_string(), None));
            return Ok(CheckReport {
                valid: false,
                diagnostics,
            });
        }
    };

    let mut diagnostics = pdf.repair_diagnostics().clone();
    if pdf.trailer().get_ref("Root").is_none() {
        diagnostics.push(Diagnostic::error("trailer is missing /Root", None));
    }
    if is_linearized_pdf(&mut pdf)? {
        diagnostics.push(Diagnostic::warning(
            "linearized PDF detected: rewrite support preserves hint object but does not recompute linearization tables",
            None,
        ));
    }

    Ok(CheckReport {
        valid: !diagnostics.has_errors(),
        diagnostics,
    })
}

fn is_linearized_pdf<R: Read + Seek>(reader: &mut Pdf<R>) -> crate::Result<bool> {
    reader.linearized_hint_ref().map(|hint| hint.is_some())
}
