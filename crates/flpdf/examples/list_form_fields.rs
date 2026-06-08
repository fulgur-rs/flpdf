//! List every interactive form field with its type and value.
//!
//! Run with: `cargo run --example list_form_fields -p flpdf`

#[path = "common/mod.rs"]
mod common;

use std::fs::File;
use std::io::BufReader;

use flpdf::Pdf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let src_path = common::write_temp("forms-src", &common::build_acroform_pdf())?;
    let mut pdf = Pdf::open(BufReader::new(File::open(&src_path)?))?;

    // `field_infos` reconstructs each field's dotted full name and resolves
    // inherited `/FT` / `/V` (following indirect references when present).
    let infos = pdf.acroform().field_infos()?;
    for info in &infos {
        let ft = info
            .field_type
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "?".into());
        let value = match &info.value {
            Some(v) => format!("{v:?}"),
            None => "<none>".into(),
        };
        println!("  {} : /{} = {}", info.full_name, ft, value);
    }
    assert_eq!(
        infos.len(),
        2,
        "expected 2 form fields, got {}",
        infos.len()
    );

    // The fixture provides a /Tx field `FirstName` and a /Btn field `Agree`;
    // assert their field types (not just the count) so identity is verified.
    let find = |name: &str| {
        infos
            .iter()
            .find(|i| i.full_name == name)
            .unwrap_or_else(|| panic!("missing field {name:?}"))
    };
    assert_eq!(
        find("FirstName").field_type.as_deref(),
        Some(b"Tx".as_ref()),
        "FirstName should be a /Tx field"
    );
    assert_eq!(
        find("Agree").field_type.as_deref(),
        Some(b"Btn".as_ref()),
        "Agree should be a /Btn field"
    );

    println!("list_form_fields: {} field(s)", infos.len());

    drop(pdf);
    let _ = std::fs::remove_file(&src_path);
    Ok(())
}
