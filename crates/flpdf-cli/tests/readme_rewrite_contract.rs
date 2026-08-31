//! Workspace-file contract: flpdf's `rewrite`/`write_pdf` output is a fresh
//! full rewrite, never a PDF incremental append (qpdf 11.9.0 has no such
//! writer; `QPDFWriter::write` dispatches only to `writeStandard`/
//! `writeLinearized`, both of which open the destination fresh with `wb+`
//! and drop `/Prev` via `getTrimmedTrailer`, `libqpdf/QPDFWriter.cc:65-98,
//! 2005-2031,2187-2205`). Reads workspace-relative paths not present in the
//! published crate archive; excluded from packaging in Cargo.toml, matching
//! `ci_workflow_contract.rs`/`release_workflow_contract.rs`.

const README: &str = include_str!("../../../README.md");
const SIGNATURES: &str = include_str!("../../flpdf/src/signatures.rs");
const CORRESPONDENCE: &str = include_str!("../../../docs/qpdf-correspondence.md");

#[test]
fn readme_command_table_describes_rewrite_as_a_fresh_full_rewrite() {
    assert!(
        README.contains("flpdf rewrite  input.pdf  out.pdf           # fresh full rewrite"),
        "README command table must describe `rewrite` as a fresh full rewrite"
    );
    assert!(
        !README.contains("flpdf rewrite --full-rewrite in.pdf out.pdf"),
        "README must not advertise the removed --full-rewrite flag"
    );
}

#[test]
fn readme_library_overview_describes_write_pdf_as_a_fresh_full_rewrite() {
    assert!(
        README
            .contains("- `write_pdf` / `write_qdf` — fresh full rewrite and qdf-style flat dump."),
        "README library overview must describe write_pdf as a fresh full rewrite"
    );
}

#[test]
fn signatures_module_states_the_full_rewrite_boundary() {
    assert!(
        SIGNATURES.contains(
            "The full rewrite cannot preserve signature validity through incremental updates;"
        ),
        "signature module docs must state the current full-rewrite boundary"
    );
}

#[test]
fn correspondence_matrix_distinguishes_reader_prev_history_from_pdf_append() {
    assert!(
        CORRESPONDENCE.contains(
            "| PDF incremental append: not applicable | qpdf 11.9.0 has no incremental append writer; `/Prev` is reader-side xref history | flpdf `PdfWriter` always emits a fresh full rewrite; reader-side `/Prev` parsing remains |"
        ),
        "compatibility matrix must distinguish reader /Prev history from PDF append output"
    );
}
