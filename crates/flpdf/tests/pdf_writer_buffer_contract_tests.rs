#[test]
fn non_linearized_writer_uses_the_configured_sink_during_emission() {
    let source = include_str!("../src/writer.rs");
    let write = source
        .split("    pub fn write(&mut self) -> Result<()> {")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// Return the output identity").next())
        .expect("PdfWriter::write source");
    let standard_path = write
        .split_once("        } else {")
        .map(|(_, standard)| standard)
        .expect("non-linearized writer branch");

    assert!(
        source.contains("struct WriterOutputSink"),
        "non-linearized emission needs a sink-owned output adapter"
    );
    assert!(
        !standard_path.contains("write_complete(bytes)"),
        "non-linearized emission must not transfer a second complete Vec"
    );
}
