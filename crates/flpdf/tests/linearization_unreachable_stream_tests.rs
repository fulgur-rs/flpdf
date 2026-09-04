//! qpdf 11.9.0 parity for linearization's reachable-object prepass.

mod common;

use common::{build_pdf, write_linearized_with_settings, write_with_settings, WriterTestSettings};
use flpdf::{DecodeLevel, NewlineBeforeEndstream, ObjectStreamMode, Pdf};
use std::cell::Cell;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::process::Command;
use std::rc::Rc;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

/// A valid one-page document with an orphaned stream whose indirect `/Length`
/// holder is a genuinely malformed object on disk. qpdf's optimization walk
/// does not use that orphan as a reachable stream edge, while qpdf's general
/// writer setup may still diagnose it while resolving the xref universe.
fn pdf_with_unreachable_stream() -> (Vec<u8>, u64, u64) {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_owned()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_owned(),
            ),
            (4, "<< /Length 5 0 R >>\nstream\n\nendstream".to_owned()),
            (5, "<< /Broken [ >>".to_owned()),
        ],
        1,
    );
    let object_start = bytes
        .windows(b"5 0 obj\n".len())
        .position(|window| window == b"5 0 obj\n")
        .expect("orphan stream length holder header");
    let object_end = object_start
        + bytes[object_start..]
            .windows(b"\nendobj\n".len())
            .position(|window| window == b"\nendobj\n")
            .expect("orphan stream object terminator")
        + b"\nendobj\n".len();
    (bytes, object_start as u64, object_end as u64)
}

/// Inject an I/O failure only while the reader is asked for the orphaned
/// stream's indirect `/Length` holder. The xref/trailer remain readable, so
/// the document can be opened lazily and the failure proves whether
/// linearization touched the unreachable stream.
struct FailingObjectReader {
    inner: Cursor<Vec<u8>>,
    fail_start: u64,
    fail_end: u64,
    fail_enabled: Rc<Cell<bool>>,
}

impl FailingObjectReader {
    fn new(bytes: Vec<u8>, fail_start: u64, fail_end: u64) -> (Self, Rc<Cell<bool>>) {
        let fail_enabled = Rc::new(Cell::new(false));
        (
            Self {
                inner: Cursor::new(bytes),
                fail_start,
                fail_end,
                fail_enabled: Rc::clone(&fail_enabled),
            },
            fail_enabled,
        )
    }
}

impl Read for FailingObjectReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let start = self.inner.position();
        if self.fail_enabled.get() && start >= self.fail_start && start < self.fail_end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unreachable stream read injected by test",
            ));
        }
        self.inner.read(buffer)
    }
}

impl Seek for FailingObjectReader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[test]
fn linearization_does_not_resolve_an_unreachable_unreadable_stream() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("unreachable-unreadable.pdf");
    let flpdf_output = temp.path().join("flpdf-linearized.pdf");
    let (bytes, fail_start, fail_end) = pdf_with_unreachable_stream();
    std::fs::write(&input, &bytes).expect("write fixture");

    // The flpdf-only regression proof always runs: FailingObjectReader
    // injects a controlled I/O failure only while resolving the orphaned
    // stream, so a successful write proves flpdf's own reachability
    // scoping regardless of whether qpdf is installed here.
    let (reader, fail_enabled) = FailingObjectReader::new(bytes, fail_start, fail_end);
    let mut pdf = Pdf::open(reader).expect("flpdf should open the lazy fixture");
    fail_enabled.set(true);
    let settings = WriterTestSettings {
        decode_level: DecodeLevel::None,
        deterministic_id: true,
        object_streams: ObjectStreamMode::Disable,
        newline_before_endstream: NewlineBeforeEndstream::Never,
        ..WriterTestSettings::default()
    };
    let result = write_linearized_with_settings(&mut pdf, &settings);
    assert!(
        result.is_ok(),
        "flpdf must ignore the same unreachable unreadable stream: {result:?}"
    );
    let bytes = result.expect("successful linearization output");
    std::fs::write(&flpdf_output, &bytes).expect("write flpdf output for qpdf check");

    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for this parity test on CI");
        }
        eprintln!("skipping qpdf comparison: qpdf 11.9.0 is not available");
        return;
    }

    let qpdf_output = temp.path().join("qpdf-linearized.pdf");
    let qpdf = Command::new("qpdf")
        .args(["--warning-exit-0", "--linearize"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf 11.9.0 must run");
    assert_eq!(
        qpdf.status.code(),
        Some(0),
        "qpdf warning-exit-0 linearization must complete: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("object 5"),
        "qpdf must diagnose the genuinely malformed orphan object: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );
    assert!(qpdf_output.exists(), "qpdf must create linearized output");
    let qpdf_check = Command::new("qpdf")
        .args(["--check"])
        .arg(&qpdf_output)
        .output()
        .expect("qpdf 11.9.0 must check its output");
    assert_eq!(
        qpdf_check.status.code(),
        Some(0),
        "qpdf warning-exit-0 output must remain valid: {}",
        String::from_utf8_lossy(&qpdf_check.stderr)
    );

    let flpdf_check = Command::new("qpdf")
        .args(["--check"])
        .arg(&flpdf_output)
        .output()
        .expect("qpdf 11.9.0 must check flpdf output");
    assert_eq!(
        flpdf_check.status.code(),
        Some(0),
        "flpdf linearized output must remain valid: {}",
        String::from_utf8_lossy(&flpdf_check.stderr)
    );
}

#[test]
fn object_stream_planning_does_not_resolve_an_unreachable_unreadable_stream() {
    for (mode, linearized) in [
        (ObjectStreamMode::Preserve, false),
        (ObjectStreamMode::Generate, false),
        (ObjectStreamMode::Preserve, true),
        (ObjectStreamMode::Generate, true),
    ] {
        let (bytes, fail_start, fail_end) = pdf_with_unreachable_stream();
        let (reader, fail_enabled) = FailingObjectReader::new(bytes, fail_start, fail_end);
        let mut pdf = Pdf::open(reader).expect("flpdf should open the lazy fixture");
        fail_enabled.set(true);
        let settings = WriterTestSettings {
            decode_level: DecodeLevel::None,
            deterministic_id: true,
            object_streams: mode,
            // QDF selects the specialized non-linearized coordinator, whose
            // Preserve/Generate planner used to scan the whole xref universe.
            qdf: !linearized,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            ..WriterTestSettings::default()
        };
        let result = if linearized {
            write_linearized_with_settings(&mut pdf, &settings)
        } else {
            write_with_settings(&mut pdf, Vec::new(), &settings).map(|()| Vec::new())
        };
        assert!(
            result.is_ok(),
            "{mode:?} {} write must ignore the unreachable unreadable stream: {result:?}",
            if linearized { "linearized" } else { "standard" }
        );
    }
}
