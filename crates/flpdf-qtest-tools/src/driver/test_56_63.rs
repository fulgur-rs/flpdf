use std::ffi::OsStr;
use std::io::{Read, Seek, Write};

use flpdf::{EncryptParams, ObjectHandle, PageDocumentHelper, Pdf, PdfOpenOptions, PdfWriter};

use super::{emit_new_diagnostics, os_str_diagnostic_bytes};
use crate::output::write_bytes;

/// Open `path` as a secondary document the way qpdf's own single-argument
/// `QPDF::processFile(char const*)` does for every test in this file that
/// takes a foreign document via `arg2` (`oldpdf`/`pdf2` locals,
/// test_driver.cc:2087). Repair is enabled (qpdf's `processFile` always
/// attempts recovery on demand) and any repair diagnostic is printed
/// immediately, exactly once, using `path`'s own name — matching qpdf's
/// default warning callback, which prints straight to `std::cerr` for a
/// `QPDF` that never installs a custom handler. This is the same recipe as
/// `test_26_33.rs`'s private `open_secondary_pdf` (that file's copy is not
/// `pub(crate)`, so it cannot be imported here); no password parameter is
/// needed because every call site in this file uses the one-argument
/// `processFile` overload.
fn open_secondary_pdf(
    path: &OsStr,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> flpdf::Result<Pdf<std::fs::File>> {
    let file = std::fs::File::open(path)?;
    let options = PdfOpenOptions {
        repair: true,
        suppress_warnings: true,
        ..PdfOpenOptions::default()
    };
    let secondary = Pdf::open_with_options(file, options)?;
    let mut secondary_diagnostics_written = 0;
    let path_bytes = os_str_diagnostic_bytes(path);
    emit_new_diagnostics(
        &secondary,
        &mut secondary_diagnostics_written,
        &path_bytes,
        stdout,
        stderr,
    )?;
    Ok(secondary)
}

/// test_56_59 (test_driver.cc:2073-2113): overlay `pdf2`'s pages as Form
/// XObjects onto `pdf`'s own pages, one destination resource dictionary and
/// one placed content fragment per page, then write `a.pdf` in QDF mode with
/// a static `/ID`. `handle_from_transformation`/`invert_to_transformation`
/// select which of the four `getFormXObjectForPage`/`placeFormXObject`
/// rotation-handling combinations `test_56`..`test_59` each exercise
/// (test_driver.cc:2077-2082).
///
/// The loop's very first statement
/// (`pdf.copyForeignObject(ph2.getFormXObjectForPage(handle_from_transformation))`,
/// test_driver.cc:2096) already needs three primitives with no public flpdf
/// equivalent, so nothing past computing `pages1`/`pages2`/`npages` can be
/// attempted; see the `GAP` comment below for the full accounting.
#[allow(clippy::too_many_arguments)]
fn test_56_59_body<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    handle_from_transformation: bool,
    invert_to_transformation: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // qpdf: `assert(arg2);` (test_driver.cc:2085).
    let arg2 = arg2.expect("test 56-59 requires arg2, matching qpdf's own assert(arg2 != nullptr)");
    let mut pdf2 = open_secondary_pdf(arg2, stdout, stderr)?;

    // `QPDFPageDocumentHelper(pdf).getAllPages()` / `QPDFPageDocumentHelper(pdf2).getAllPages()`
    // (test_driver.cc:2089-2091). Computed for real — this reflects qpdf's own values and
    // triggers whatever page-tree-walk diagnostics either document would surface — but
    // `npages` itself is only ever consumed by the loop body the GAP below stops before.
    let pages1 = PageDocumentHelper::new(pdf).get_all_pages()?;
    let pages2 = PageDocumentHelper::new(&mut pdf2).get_all_pages()?;
    let npages = pages1.len().min(pages2.len());
    let _ = (
        handle_from_transformation,
        invert_to_transformation,
        npages,
        filename,
        &diagnostics_written,
    );

    // GAP(QPDFPageObjectHelper::getFormXObjectForPage / QPDFObjectHandle::getAttribute /
    // QPDFPageObjectHelper::placeFormXObject): the per-page loop's first statement
    // (test_driver.cc:2095-2107) needs the page-to-Form-XObject conversion (mirrored by
    // `flpdf::page_form_xobject::get_form_xobject_for_page`, `page_form_xobject.rs:72`), the
    // inheritable-attribute lookup-with-create-if-missing used for `ph1.getAttribute("/Resources",
    // true)`, and the content-fragment placement helper (mirrored by `flpdf::job::overlay::place_form_xobject`,
    // `job/overlay.rs:139`). `get_form_xobject_for_page` is `pub(crate)` and `place_form_xobject` is not
    // even `pub(crate)`-reachable outside `job/overlay.rs`; no `getAttribute`-with-create equivalent
    // exists at any visibility. None of the three is exposed to this separate crate, so no
    // iteration of the loop can be attempted, and the `QPDFWriter` write of `a.pdf` at the end of
    // qpdf's `test_56_59` (test_driver.cc:2109-2112) — which depends entirely on the loop's page
    // mutations — is not attempted either.
    Ok(())
}

/// test_56 (test_driver.cc:2115-2119): `test_56_59(pdf, arg2, false, false)`.
pub(crate) fn run_test_56<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        false,
        false,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// test_57 (test_driver.cc:2121-2125): `test_56_59(pdf, arg2, true, false)`.
pub(crate) fn run_test_57<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        true,
        false,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// test_58 (test_driver.cc:2127-2131): `test_56_59(pdf, arg2, false, true)`.
pub(crate) fn run_test_58<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        false,
        true,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// test_59 (test_driver.cc:2133-2137): `test_56_59(pdf, arg2, true, true)`.
pub(crate) fn run_test_59<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    test_56_59_body(
        pdf,
        filename,
        arg2,
        true,
        true,
        stdout,
        stderr,
        diagnostics_written,
    )
}

/// The `merge_resources` conflicts-tracking map: `rtype -> old_key ->
/// new_key`, structurally identical to (but not the same nominal path as)
/// `flpdf`'s own `object_handle::ResourceConflicts` — that type alias lives
/// in a private (`mod object_handle;`, not `pub mod`) module and is not
/// re-exported from the crate root, so it cannot be named from this crate.
/// Rust checks the parameter type
/// (`Option<&mut object_handle::ResourceConflicts>`) structurally for a type
/// alias, so a locally-declared alias with the identical expansion satisfies
/// [`ObjectHandle::merge_resources`]'s signature without needing to name the
/// inaccessible alias — a container substitution, not a missing primitive.
type MergeConflicts =
    std::collections::BTreeMap<Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>>;

/// qpdf's local `show_conflicts` lambda (test_driver.cc:2176-2184): print
/// `msg`, then each `rtype` (in sorted order, matching `std::map`'s own
/// ordering — `MergeConflicts` is a `BTreeMap` for the same reason) followed
/// by its `old_key -> new_key` pairs, two-space indented.
fn show_conflicts(
    msg: &str,
    conflicts: &MergeConflicts,
    stdout: &mut dyn Write,
) -> flpdf::Result<()> {
    writeln!(stdout, "{msg}")?;
    for (rtype, renames) in conflicts {
        write_bytes(stdout, rtype)?;
        writeln!(stdout, ":")?;
        for (old_key, new_key) in renames {
            write!(stdout, "  ")?;
            write_bytes(stdout, old_key)?;
            write!(stdout, " -> ")?;
            write_bytes(stdout, new_key)?;
            writeln!(stdout)?;
        }
    }
    Ok(())
}

/// qpdf's local `make_resource` lambda (test_driver.cc:2152-2157): build a
/// one-element array holding `QPDFObjectHandle::newString(text)`, make it an
/// indirect object of `pdf`, and install it at `dict[key]`.
fn make_resource<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &ObjectHandle,
    key: &[u8],
    text: &[u8],
) -> flpdf::Result<()> {
    let array = ObjectHandle::array(vec![ObjectHandle::string(text.to_vec())]);
    let indirect = pdf.make_indirect_object_handle(array)?;
    dict.replace_key(key, indirect)
}

/// test_60 (test_driver.cc:2139-2213): boundary-condition testing for
/// `getUniqueResourceName` and conflict-detecting `mergeResources`.
///
/// The first two merges (`show_conflicts("first merge")`/`"second merge"`,
/// test_driver.cc:2186-2192) are real: every primitive they need
/// ([`ObjectHandle::merge_resources`], [`ObjectHandle::get_unique_resource_name`],
/// [`ObjectHandle::shallow_copy`], [`ObjectHandle::replace_key`],
/// [`Pdf::make_indirect_object_handle`]) is public. `r2.makeResourcesIndirect(pdf)`
/// (test_driver.cc:2197) has no flpdf equivalent at any visibility, so the
/// third/fourth merges and the final `a.pdf` write cannot be attempted; see
/// the `GAP` comment below.
pub(crate) fn run_test_60<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, stderr, diagnostics_written); // test_60 has no warning-producing operation on `pdf`.

    let r1 = ObjectHandle::dictionary(vec![]);
    let mut min_suffix: usize = 1;
    for _ in 1..3 {
        let name = r1.get_unique_resource_name(b"/Quack", &mut min_suffix, None)?;
        r1.merge_resources(&ObjectHandle::parse(b"<< /Z << >> >>")?, None)?;
        r1.get_key(b"/Z")
            .replace_key(&name, ObjectHandle::string(b"moo".to_vec()))?;
    }

    let z = r1.get_key(b"/Z");
    r1.replace_key(b"/Y", ObjectHandle::dictionary(vec![]))?;
    let y = r1.get_key(b"/Y");
    make_resource(pdf, &z, b"/F1", b"r1.Z.F1")?;
    make_resource(pdf, &z, b"/F2", b"r1.Z.F2")?;
    make_resource(pdf, &y, b"/F2", b"r1.Y.F2")?;
    make_resource(pdf, &y, b"/F3", b"r1.Y.F3")?;

    let r2 = ObjectHandle::parse(b"<< /Z << >> /Y << >> >>")?;
    let z = r2.get_key(b"/Z");
    let y = r2.get_key(b"/Y");
    make_resource(pdf, &z, b"/F2", b"r2.Z.F2")?;
    make_resource(pdf, &y, b"/F3", b"r2.Y.F3")?;
    make_resource(pdf, &y, b"/F4", b"r2.Y.F4")?;
    // qpdf: `y.replaceKey("/F5", QPDFObjectHandle::newString("direct r2.Y.F5"));`
    // (test_driver.cc:2173) — a direct object, unlike the four `make_resource` calls above.
    y.replace_key(b"/F5", ObjectHandle::string(b"direct r2.Y.F5".to_vec()))?;

    let mut conflicts: MergeConflicts = MergeConflicts::new();

    r1.merge_resources(&r2, Some(&mut conflicts))?;
    show_conflicts("first merge", &conflicts, stdout)?;
    let _r3 = r1.shallow_copy()?;
    // Merge again. The direct object gets recopied. Everything else is the same
    // (test_driver.cc:2189-2190).
    r1.merge_resources(&r2, Some(&mut conflicts))?;
    show_conflicts("second merge", &conflicts, stdout)?;

    // GAP(QPDFObjectHandle::makeResourcesIndirect): flpdf has no equivalent at any
    // visibility (searched `object_handle.rs`, `page_object_helper.rs`, and
    // `page_form_xobject.rs` for `make_resources_indirect`/`MakeResourcesIndirect` with no
    // match). qpdf's next step, `r2.makeResourcesIndirect(pdf)` (test_driver.cc:2197), makes
    // every resource in `r2` an indirect object before merging twice more so the previously
    // direct `/F5` value gets copied exactly once as an indirect object
    // (test_driver.cc:2194-2201). Even if this existed, the final step —
    // `pdf.getTrailer().replaceKey("/QTest1", r1)` / `"/QTest2"` / `"/QTest3"`
    // (test_driver.cc:2205-2208) — is the same missing primitive as `run_test_26`'s GAP in
    // `test_26_33.rs`: flpdf has no public API to mutate `Pdf::trailer()` after open. So the
    // third/fourth merges and the `a.pdf` write (test_driver.cc:2209-2212) are not attempted.
    Ok(())
}

/// test_61 (test_driver.cc:2215-2260): verify exception types and RTTI
/// (`dynamic_cast`) survive a shared-library boundary.
///
/// Every line of this test is specific to qpdf's own C++/shared-library
/// concerns with no Rust counterpart: `pdf.setAttemptRecovery(false)` — the
/// very first statement (test_driver.cc:2221) — needs a public mutator to
/// disable recovery on an *already-open* `Pdf`, but flpdf only accepts
/// `repair`/`PdfOpenOptions::repair` at open time (`reader.rs`'s
/// `PdfOpenOptions`); no `set_attempt_recovery` exists at any visibility
/// (`Resolver::attempt_recovery` is a private query, never a public setter).
/// `pdf.processMemoryFile(...)` two lines later reuses the same live `QPDF`
/// instance to reopen unrelated in-memory bytes in place — flpdf's `Pdf<R>`
/// has no such re-open operation; every open goes through a factory function
/// that returns a fresh value, and this function's own `R` is fixed by its
/// caller. The remaining lines (`QUtil::safe_fopen`,
/// `QUtil::int_to_string_base`, `QUtil::toUTF8`, `BufferInputSource`/
/// `Pl_Discard` `dynamic_cast` probes, and the `QPDFNameTreeObjectHelper`
/// mingw-vtable regression check) are all qpdf-internal-utility or C++-RTTI
/// concerns with no flpdf equivalent either. Since the very first statement
/// already has no public port, the entire test body — including the four
/// "Caught ... as expected" lines qpdf prints on its (expected) successful
/// path — is not attempted.
pub(crate) fn run_test_61<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (pdf, filename, arg2, stdout, stderr, diagnostics_written);
    // GAP(QPDF::setAttemptRecovery): see the function doc above — the entire test body
    // depends on this and the following missing primitives from its first statement.
    Ok(())
}

/// test_62 (test_driver.cc:2262-2287): int/unsigned-int size-boundary
/// checks on trailer values written via `QPDFObjectHandle::newInteger` and
/// read back through `getIntValue`/`getUIntValue`/`getIntValueAsInt`/
/// `getUIntValueAsUInt`.
///
/// `t.replaceKey(...)` here is real, unlike the `run_test_26`/`run_test_60`
/// GAP: `Pdf::trailer()` is memoized (`pdf.rs:286-296`, "Repeated
/// calls return the same shared handle"), so `ObjectHandle::replace_key` on
/// it mutates the one shared handle every later `trailer()`/
/// `get_key` call observes — the missing piece in the other tests'
/// GAP is specifically that `PdfWriter` never reads that bridge back, which
/// is irrelevant here since this test never writes a file.
///
/// `getIntValue` (unclamped `i64`, no warning path exercised for a plain
/// integer) is real via [`ObjectHandle::as_integer`]. `getUIntValue` (`u64`,
/// negative-clamps-to-0) has no flpdf equivalent at any visibility — not
/// even searchable by name anywhere in `object_handle.rs`. `getIntValueAsInt`
/// (`i32`, saturating) exists only as `ObjectHandle::try_get_int_value_as_int`
/// (`object_handle.rs:2456`), `pub(crate)`-only. `getUIntValueAsUInt` (`u32`,
/// saturating) has no equivalent at any visibility either.
pub(crate) fn run_test_62<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, stdout, stderr, diagnostics_written); // test_62 prints nothing and triggers no new diagnostics.

    let t = pdf.trailer();
    // `QIntC::to_ulonglong(INT_MAX)`/`to_longlong(INT_MIN)`/`to_longlong(UINT_MAX)`
    // (test_driver.cc:2268-2273) are lossless casts here: every product below fits
    // comfortably inside i64/u64, so no `QIntC` narrowing check can fire.
    let q1_l: u64 = 3_u64 * u64::from(u32::try_from(i32::MAX).expect("i32::MAX fits in u32"));
    let q1: i64 = i64::try_from(q1_l).expect("q1_l fits in i64");
    let q2_l: i64 = 3_i64 * i64::from(i32::MIN);
    let q2: i64 = q2_l;
    let q3_i: u32 = u32::MAX;
    let q3: i64 = i64::from(q3_i);

    t.replace_key(b"/Q1", ObjectHandle::integer(q1))?;
    t.replace_key(b"/Q2", ObjectHandle::integer(q2))?;
    t.replace_key(b"/Q3", ObjectHandle::integer(q3))?;

    // qpdf: `assert_compare_numbers(q1, t.getKey("/Q1").getIntValue());` (test_driver.cc:2277).
    assert_eq!(t.get_key(b"/Q1").as_integer(), Some(q1));

    // GAP(QPDFObjectHandle::getUIntValue / getIntValueAsInt / getUIntValueAsUInt): see the
    // function doc above. qpdf's remaining eight assertions on `/Q1`, `/Q2`, `/Q3`
    // (test_driver.cc:2278-2286) each need one of these three accessors, so they are not
    // attempted.
    Ok(())
}

/// test_63 (test_driver.cc:2289-2301): set R6 (AES-256) encryption
/// parameters on a `QPDFWriter` *before* setting its output filename,
/// regression-testing a qpdf bug where the filename was (incorrectly) part
/// of the `/ID` input data. flpdf's `/ID` generation does not consult the
/// output filename at all (`PdfWriter::set_output_file` only names where
/// bytes go), so this specific bug class cannot recur here — the test is
/// ported for its literal call sequence, mirroring qpdf's own operation
/// order exactly, not because flpdf shares the underlying risk.
///
/// `setR6EncryptionParameters("u", "o", true, true, true, true, true, true,
/// qpdf_r3p_full, true)` (test_driver.cc:2298) requests every capability bit
/// granted and full-quality printing — the same permission set
/// [`flpdf::PermissionsConfig::default`] encodes (`permissions.rs`'s own bit
/// table matches `interpretR3EncryptionParameters`'s `P` bit assignments
/// one-to-one, and every `allow_*` argument here is `true` with
/// `print = qpdf_r3p_full`, so no bit gets cleared either way) — so
/// [`EncryptParams::v5_r6`]'s default permissions and `encrypt_metadata =
/// true` are exactly this call's parameters, without needing to name each
/// bit individually.
pub(crate) fn run_test_63<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    filename: &[u8],
    _arg2: Option<&OsStr>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let _ = (filename, stdout, stderr, diagnostics_written); // test_63 prints nothing and triggers no new diagnostics.

    let mut w = PdfWriter::new(pdf);
    w.set_encryption_parameters(EncryptParams::v5_r6("u", "o"));
    w.set_output_file("a.pdf")?;
    w.write()?;
    Ok(())
}
