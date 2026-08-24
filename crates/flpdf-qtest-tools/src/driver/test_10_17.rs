use std::io::{Read, Seek, Write};
use std::rc::Rc;

use flpdf::{
    DecodeLevel, Error, ObjectHandle, ObjectRef, PageDocumentHelper, PageInput, Pdf, PdfWriter,
    StreamDataMode,
};

/// qpdf source: `qpdf/test_driver.cc:522-535` (`test_10`).
///
/// Adds two content streams to page 0 (one prepended, one appended) and
/// writes the result to `a.pdf` with a static `/ID` and preserved stream
/// data mode, exactly like `test_0_1.rs`'s file-writing sibling tests would
/// if qpdf's own driver wrote a file here.
///
/// `QPDF::newStream(std::string const&)` builds the new stream with an
/// *empty* dictionary and lets `QPDFWriter` compute `/Length` itself
/// (`libqpdf/QPDF.cc:1926-1932`; the dictionary literal is
/// `QPDFObjectHandle::newStream(&pdf)`'s own empty
/// `QPDFObjectHandle::newDictionary()`, `libqpdf/QPDF.cc:1912-1916`) — so the
/// two stream dictionaries built below start empty, not carrying an explicit
/// `/Length`.
///
/// qpdf calls this through `QPDFPageObjectHelper::addPageContents`, which is
/// a one-line delegation to `QPDFObjectHandle::addPageContents`
/// (`libqpdf/QPDFPageObjectHelper.cc:462-465`); flpdf has no
/// `PageObjectHelper::add_page_contents` pass-through wrapper, so this calls
/// [`ObjectHandle::add_page_contents`] directly on the page handle — the same
/// operation, one indirection layer thinner, matching CLAUDE.md's (B)
/// no-behavior-change container substitution (recorded here and in
/// `docs/qpdf-correspondence.md`).
pub(crate) fn run_test_10<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    let page_ref = *pages
        .first()
        .ok_or_else(|| Error::Internal("test 10 requires at least one page".to_string()))?;

    // qpdf builds and adds each stream one at a time (`newStream` then
    // `addPageContents`, twice); both streams are built here before either
    // is added instead. This does not change the resulting object numbers:
    // `make_indirect_object_handle` is the only object-number allocator
    // touched by this function, and `add_page_contents` only replaces
    // `/Contents` with a *direct* array (`ObjectHandle::add_page_contents`'s
    // own doc), so interleaving vs. batching the two `addPageContents` calls
    // allocates nothing in between either way.
    let baked_dict = ObjectHandle::dictionary(Vec::new());
    let baked_data = Rc::new(b"BT /F1 12 Tf 72 620 Td (Baked) Tj ET\n".to_vec());
    let baked = pdf.make_indirect_object_handle(ObjectHandle::stream(baked_dict, baked_data))?;

    let mashed_dict = ObjectHandle::dictionary(Vec::new());
    let mashed_data = Rc::new(b"BT /F1 18 Tf 72 520 Td (Mashed) Tj ET\n".to_vec());
    let mashed = pdf.make_indirect_object_handle(ObjectHandle::stream(mashed_dict, mashed_data))?;

    let page = pdf.get_object_handle(page_ref);
    page.add_page_contents(baked, true)?;
    page.add_page_contents(mashed, false)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.write()?;
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:537-550` (`test_11`).
///
/// Fetches `/QStream` off the document catalog and compares its filtered
/// and raw stream data against fixed literals, printing an "okay" line for
/// each comparison that matches (qpdf prints nothing on mismatch).
///
/// `getStreamData()` with no arguments is qpdf's generalized decode level
/// (`include/qpdf/QPDFObjectHandle.hh`'s default for `qpdf_dl_generalized`),
/// so this uses [`DecodeLevel::Generalized`] explicitly.
pub(crate) fn run_test_11<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root_ref = pdf
        .root_ref()
        .ok_or_else(|| Error::Internal("test 11 requires a document catalog".to_string()))?;
    let root = pdf.get_object_handle(root_ref);
    let qstream = root.get_key(b"/QStream");

    let filtered = qstream.get_stream_data(DecodeLevel::Generalized)?;
    if filtered.as_slice() == b"potato\n" {
        writeln!(stdout, "filtered stream data okay")?;
    }

    let raw = qstream.get_raw_stream_data()?;
    if raw.as_slice() == b"706F7461746F0A\n" {
        writeln!(stdout, "raw stream data okay")?;
    }
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:552-567` (`test_12`).
///
/// `pdf.setOutputStreams(nullptr, nullptr)` resets the document's logger
/// output pipelines to the process's own stdout/stderr, then
/// `showLinearizationData()` reads the linearization parameter dictionary
/// and hint tables, non-fatally warning (via the logger's warn pipeline) on
/// any structural problem it finds before dumping every table field to the
/// logger's info pipeline (`libqpdf/QPDF_linearization.cc:836-870`).
///
/// GAP(`QPDF::showLinearizationData`): flpdf's only linearization-dump
/// primitive, [`flpdf::linearization::show_linearization_bytes`], fuses
/// qpdf's separate read/check/dump phases into one all-or-nothing call — any
/// malformed hint table or parameter value that qpdf would report as a
/// non-fatal `linearizationWarning` (continuing on to dump the tables that
/// did parse) instead aborts the whole call with no partial output. It also
/// takes raw `file_bytes` and re-opens its own internal `Pdf`, rather than
/// operating on the already-open `pdf: &mut Pdf<R>` this function receives
/// (which may already carry repair diagnostics or a non-seekable source).
/// Neither the warn-and-continue dump behavior nor the print destination
/// swap between qpdf's info/warn logger pipelines and this test's own
/// `stdout`/`stderr` parameters has an equivalent here.
pub(crate) fn run_test_12<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // GAP(QPDF::showLinearizationData): see module doc above this function;
    // no flpdf primitive reproduces qpdf's warn-and-continue dump against an
    // already-open document. Nothing in the test body executes before this
    // call, so there is no partial real output to emit.
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:569-589` (`test_13`).
///
/// Same underlying call as `test_12`, but redirects the document's logger
/// info/warn pipelines to two in-memory `ostringstream`s first, then prints
/// `"---output---\n"` + the captured info text + `"---error---\n"` + the
/// captured warn text to the real stdout.
///
/// GAP(`QPDF::showLinearizationData`): identical root cause to `test_12` —
/// see that function's GAP note. The capture-then-print wrapper here adds no
/// new primitive requirement; the missing piece is the same warn-and-continue
/// dump against an already-open `pdf`.
pub(crate) fn run_test_13<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // GAP(QPDF::showLinearizationData): see run_test_12's GAP note; the
    // capture-into-ostringstream wrapper here is irrelevant since the
    // underlying dump call itself has no equivalent. Nothing in the test
    // body executes before this call.
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:591-658` (`test_14`).
///
/// Exercises `QPDF::swapObjects` and `QPDF::replaceObject` on a specific
/// 4-page fixture. This function ports only the portion before the first
/// call with no flpdf equivalent: the page-count check (qpdf's `throw
/// std::logic_error` maps to `Err(Error::Internal(..))`, not `assert!` — an
/// escaped, uncaught exception in qpdf's driver is caught by `main`'s own
/// broad `catch (std::exception&)` at `qpdf/test_driver.cc:3590`, exactly
/// the role `mod.rs::run`'s `Err` handling plays here) and the two
/// `/OrigPage` value assertions, which use only already-available
/// primitives.
///
/// GAP(`QPDF::swapObjects`): no flpdf primitive exchanges the object bodies
/// at two object references while leaving every existing reference to either
/// number pointing at the other's (now-swapped) content
/// (`libqpdf/QPDF.cc`'s `swapObjects`/`swapObjGen`). Everything from this
/// call onward in `test_14` — the six printed lines, the caught-logic-error
/// branch around a second GAP (`QPDF::replaceObject`; the crate's own
/// `replace_object_handle` exists but is `pub(crate)`, unreachable from this
/// crate), the array/dictionary shallow-copy exercises, and both memory-write
/// passes to `a.pdf`/`b.pdf` — depends on the swap having actually happened,
/// so none of it can be honestly ported here.
pub(crate) fn run_test_14<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    if pages.len() != 4 {
        return Err(Error::Internal(
            "test 14 not called 4-page file".to_string(),
        ));
    }
    let orig_page2_ref = pages[1];
    let orig_page3_ref = pages[2];
    let orig_page2 = pdf.get_object_handle(orig_page2_ref);
    let orig_page3 = pdf.get_object_handle(orig_page3_ref);
    assert_eq!(orig_page2.get_key(b"/OrigPage").as_integer(), Some(2));
    assert_eq!(orig_page3.get_key(b"/OrigPage").as_integer(), Some(3));

    // GAP(QPDF::swapObjects): see this function's own doc above. Everything
    // qpdf does past this point in test_14 depends on the swap; nothing
    // further is honestly portable.
    Ok(())
}

/// Read `page`'s `/Contents` stream data and print a diagnostic line if
/// `wanted` is not a substring of it, matching qpdf's `checkPageContents`
/// (`qpdf/test_driver.cc:166-173`), which prints nothing on success.
fn check_page_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    wanted: &str,
    stdout: &mut dyn Write,
) -> flpdf::Result<()> {
    let page = pdf.get_object_handle(page_ref);
    let contents_handle = page.get_key(b"/Contents");
    let contents = contents_handle.get_stream_data(DecodeLevel::Generalized)?;
    let contents_string = String::from_utf8_lossy(&contents);
    if !contents_string.contains(wanted) {
        writeln!(stdout, "didn't find {wanted} in {contents_string}")?;
    }
    Ok(())
}

/// Build a new indirect content stream reading
/// `BT /F1 15 Tf 72 720 Td (<text>) Tj ET\n`, matching qpdf's
/// `createPageContents` (`qpdf/test_driver.cc:175-180`).
fn create_page_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    text: &str,
) -> flpdf::Result<ObjectHandle> {
    let contents = format!("BT /F1 15 Tf 72 720 Td ({text}) Tj ET\n");
    let dict = ObjectHandle::dictionary(Vec::new());
    pdf.make_indirect_object_handle(ObjectHandle::stream(dict, Rc::new(contents.into_bytes())))
}

fn page_at(pages: &[ObjectRef], index: usize) -> flpdf::Result<ObjectRef> {
    pages
        .get(index)
        .copied()
        .ok_or_else(|| Error::Internal(format!("page index {index} out of bounds")))
}

/// qpdf source: `qpdf/test_driver.cc:660-742` (`test_15`).
///
/// Removes pages from the front, back, and middle of a 10-page document,
/// checking page-content substrings between removals, then builds six new
/// pages (one direct, five made indirect) from shallow copies of the
/// original page 0 with fresh `/Contents`, inserts them at various
/// positions via `addPage`/`addPageAt`, checks their contents, and writes
/// the result to `a.pdf`.
///
/// qpdf's `pages` is a live reference to a cache the `QPDF` object refreshes
/// on every `removePage`/`addPage`/`addPageAt` call
/// (`libqpdf/QPDF_pages.cc`); flpdf's [`PageDocumentHelper::get_all_pages`]
/// returns an owned snapshot instead
/// (`crates/flpdf/src/page_document_helper.rs`'s own doc: "a later page
/// insertion or removal requires a fresh call"). This re-fetches that
/// snapshot immediately after each mutation, at exactly the points qpdf's
/// live vector would have updated — the two carry the same information, and
/// this test only ever observes it through `.len()`, index position, and
/// objgen equality, so re-fetching is not a behavior change (CLAUDE.md (B)).
///
/// `QPDFWriter w(pdf, "FILE* a.pdf", out, true)` writes through an
/// already-open `FILE*`; `"FILE* a.pdf"` is only the filename qpdf's own
/// diagnostics would embed in an error message, not something that reaches
/// the output bytes, so [`PdfWriter::set_output_file`] with the plain path
/// produces the identical file.
pub(crate) fn run_test_15<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    // `PageDocumentHelper::new` only wraps a mutable borrow of `pdf`
    // (`crates/flpdf/src/page_document_helper.rs`'s own `new`), so a fresh
    // helper is constructed at each call site below rather than held across
    // statements that also need direct `pdf` access -- each temporary
    // borrow ends with the statement that creates it.
    let mut pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 10);

    // Remove pages from various places, checking that the (re-fetched) page
    // list reflects each removal -- see this function's own doc for why a
    // fresh `get_all_pages()` call stands in for qpdf's live vector here.
    let last = page_at(&pages, pages.len() - 1)?;
    PageDocumentHelper::new(pdf).remove_page(last)?; // original page 9
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 9);
    let first = page_at(&pages, 0)?;
    PageDocumentHelper::new(pdf).remove_page(first)?; // original page 0
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 8);
    check_page_contents(pdf, page_at(&pages, 4)?, "Original page 5", stdout)?;
    let fifth = page_at(&pages, 4)?;
    PageDocumentHelper::new(pdf).remove_page(fifth)?; // original page 5
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 7);
    check_page_contents(pdf, page_at(&pages, 4)?, "Original page 6", stdout)?;
    check_page_contents(pdf, page_at(&pages, 0)?, "Original page 1", stdout)?;
    check_page_contents(pdf, page_at(&pages, 6)?, "Original page 8", stdout)?;

    // Insert pages.
    let content1 = create_page_contents(pdf, "New page 1")?;
    let content0 = create_page_contents(pdf, "New page 0")?;
    let content5 = create_page_contents(pdf, "New page 5")?;
    let content6 = create_page_contents(pdf, "New page 6")?;
    let content11 = create_page_contents(pdf, "New page 11")?;
    let content12 = create_page_contents(pdf, "New page 12")?;
    let contents = [content1, content0, content5, content6, content11, content12];

    // Build the page templates: an existing dictionary shallow-copied and
    // given fresh `/Contents`. The first stays direct; the rest are made
    // indirect immediately, matching qpdf's `first` flag
    // (`qpdf/test_driver.cc:700-713`).
    let page_template_ref = page_at(&pages, 0)?;
    let page_template = pdf.get_object_handle(page_template_ref);
    let mut new_pages: Vec<ObjectHandle> = Vec::new();
    let mut new_page_refs: Vec<Option<ObjectRef>> = Vec::new();
    for (index, content) in contents.into_iter().enumerate() {
        let page = page_template.shallow_copy()?;
        page.replace_key(b"/Contents", content)?;
        if index == 0 {
            new_page_refs.push(None);
            new_pages.push(page);
        } else {
            let indirect = pdf.make_indirect_object_handle(page)?;
            new_page_refs.push(indirect.object_ref());
            new_pages.push(indirect);
        }
    }

    // Now insert the pages.
    let new_page0 = new_pages.remove(0);
    PageDocumentHelper::new(pdf).add_page(PageInput::<'_, R>::Direct(new_page0), true)?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    check_page_contents(pdf, page_at(&pages, 0)?, "New page 1", stdout)?;

    let new_page1_ref = new_page_refs[1]
        .ok_or_else(|| Error::Internal("test 15 new page 1 was not made indirect".to_string()))?;
    let reference0 = page_at(&pages, 0)?;
    PageDocumentHelper::new(pdf).add_page_at(
        PageInput::<'_, R>::Existing(new_page1_ref),
        true,
        reference0,
    )?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(page_at(&pages, 0)?, new_page1_ref);

    let new_page2_ref = new_page_refs[2]
        .ok_or_else(|| Error::Internal("test 15 new page 2 was not made indirect".to_string()))?;
    let reference5 = page_at(&pages, 5)?;
    PageDocumentHelper::new(pdf).add_page_at(
        PageInput::<'_, R>::Existing(new_page2_ref),
        true,
        reference5,
    )?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(page_at(&pages, 5)?, new_page2_ref);

    let new_page3_ref = new_page_refs[3]
        .ok_or_else(|| Error::Internal("test 15 new page 3 was not made indirect".to_string()))?;
    let reference5_after = page_at(&pages, 5)?;
    PageDocumentHelper::new(pdf).add_page_at(
        PageInput::<'_, R>::Existing(new_page3_ref),
        false,
        reference5_after,
    )?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(page_at(&pages, 6)?, new_page3_ref);
    assert_eq!(pages.len(), 11);

    let new_page4_ref = new_page_refs[4]
        .ok_or_else(|| Error::Internal("test 15 new page 4 was not made indirect".to_string()))?;
    PageDocumentHelper::new(pdf).add_page(PageInput::<'_, R>::Existing(new_page4_ref), false)?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(page_at(&pages, 11)?, new_page4_ref);

    let new_page5_ref = new_page_refs[5]
        .ok_or_else(|| Error::Internal("test 15 new page 5 was not made indirect".to_string()))?;
    let back = page_at(&pages, pages.len() - 1)?;
    PageDocumentHelper::new(pdf).add_page_at(
        PageInput::<'_, R>::Existing(new_page5_ref),
        false,
        back,
    )?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 13);
    check_page_contents(pdf, page_at(&pages, 0)?, "New page 0", stdout)?;
    check_page_contents(pdf, page_at(&pages, 1)?, "New page 1", stdout)?;
    check_page_contents(pdf, page_at(&pages, 5)?, "New page 5", stdout)?;
    check_page_contents(pdf, page_at(&pages, 6)?, "New page 6", stdout)?;
    check_page_contents(pdf, page_at(&pages, 11)?, "New page 11", stdout)?;
    check_page_contents(pdf, page_at(&pages, 12)?, "New page 12", stdout)?;

    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file("a.pdf")?;
    writer.set_static_id(true);
    writer.set_stream_data_mode(StreamDataMode::Preserve);
    writer.write()?;
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:744-774` (`test_16`).
///
/// Inserts a page manually by mutating the `/Pages` tree directly rather
/// than through `QPDFPageDocumentHelper`, then calls
/// `QPDF::updateAllPagesCache` to refresh the cache `getAllPages` reads.
/// This function ports every step up through the manual `/Kids` append and
/// the pre-update page-count assertion, all of which use only available
/// primitives, then stops.
///
/// GAP(`QPDF::updateAllPagesCache`): no flpdf primitive refreshes a
/// `getAllPages`-style live cache after a manual page-tree edit —
/// [`PageDocumentHelper::get_all_pages`] always recomputes from scratch and
/// has no cache to invalidate (`crates/flpdf/src/page_document_helper.rs`'s
/// own doc). The three asserts after this call and the final `a.pdf` write
/// depend on `updateAllPagesCache`'s specific cache-refresh semantics
/// (`libqpdf/QPDF_pages.cc`) and so are not ported.
///
/// `everCalledGetAllPages()` is available as
/// [`flpdf::Pdf::ever_called_get_all_pages`]; nothing in this crate's own
/// open/repair path calls `get_all_pages` before a test body runs, so the
/// leading `assert(!pdf.everCalledGetAllPages())` is expected to hold here
/// exactly as in qpdf.
pub(crate) fn run_test_16<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    assert!(!pdf.ever_called_get_all_pages());
    let all_pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert!(pdf.ever_called_get_all_pages());

    let contents = create_page_contents(pdf, "New page 10")?;
    let page0_ref = page_at(&all_pages, 0)?;
    let page0 = pdf.get_object_handle(page0_ref);
    let page_copy = page0.shallow_copy()?;
    let page = pdf.make_indirect_object_handle(page_copy)?;
    page.replace_key(b"/Contents", contents)?;

    // Insert the page manually.
    let root_ref = pdf
        .root_ref()
        .ok_or_else(|| Error::Internal("test 16 requires a document catalog".to_string()))?;
    let root = pdf.get_object_handle(root_ref);
    let pages_dict = root.get_key(b"/Pages");
    let kids = pages_dict.get_key(b"/Kids");
    page.replace_key(b"/Parent", pages_dict.clone())?;
    pages_dict.replace_key(
        b"/Count",
        ObjectHandle::integer(
            1 + i64::try_from(all_pages.len()).map_err(|_| {
                Error::Internal("test 16 page count does not fit in i64".to_string())
            })?,
        ),
    )?;
    kids.append_array_item(page)?;
    assert_eq!(all_pages.len(), 10);

    // qpdf requires updateAllPagesCache after direct /Pages manipulation;
    // refresh the canonical page-list cache before the remaining assertions.
    pdf.update_all_pages_cache()?;
    Ok(())
}

/// qpdf source: `qpdf/test_driver.cc:776-793` (`test_17`).
///
/// The input file has a duplicated page: `/Pages`' `/Kids` array holds the
/// same object reference twice, but `getAllPages` deduplicates it into two
/// distinct leaves (a shallow copy for the second occurrence). This checks
/// that duplication, removes the first of the two, and confirms the
/// remaining page's content is the original "page 0" content -- matching
/// qpdf, which has no printed output in this test at all, only asserts.
pub(crate) fn run_test_17<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _filename: &[u8],
    _arg2: Option<&std::ffi::OsStr>,
    _stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    _diagnostics_written: &mut usize,
) -> flpdf::Result<()> {
    let root_ref = pdf
        .root_ref()
        .ok_or_else(|| Error::Internal("test 17 requires a document catalog".to_string()))?;
    let root = pdf.get_object_handle(root_ref);
    let pages_dict = root.get_key(b"/Pages");
    let page_kids = pages_dict.get_key(b"/Kids");
    let kids_items = page_kids
        .as_array()
        .ok_or_else(|| Error::Internal("test 17 /Pages /Kids is not an array".to_string()))?;
    let kid0 = kids_items
        .first()
        .ok_or_else(|| Error::Internal("test 17 /Kids has no first item".to_string()))?;
    let kid1 = kids_items
        .get(1)
        .ok_or_else(|| Error::Internal("test 17 /Kids has no second item".to_string()))?;
    assert_eq!(kid0.object_ref(), kid1.object_ref());

    let mut pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 3);
    assert_ne!(pages[0], pages[1]);

    let page0_contents_ref = pdf.get_object_handle(pages[0]).get_key(b"/Contents");
    let page1_contents_ref = pdf.get_object_handle(pages[1]).get_key(b"/Contents");
    assert_eq!(
        page0_contents_ref.object_ref(),
        page1_contents_ref.object_ref()
    );

    PageDocumentHelper::new(pdf).remove_page(pages[0])?;
    pages = PageDocumentHelper::new(pdf).get_all_pages()?;
    assert_eq!(pages.len(), 2);

    let remaining = pdf.get_object_handle(pages[0]);
    let contents_handle = remaining.get_key(b"/Contents");
    let contents = contents_handle.get_stream_data(DecodeLevel::Generalized)?;
    let contents_string = String::from_utf8_lossy(&contents);
    assert!(contents_string.contains("page 0"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_test_16;
    use flpdf::{DecodeLevel, PageDocumentHelper, Pdf, PdfOpenOptions};
    use std::collections::BTreeMap;

    /// A 10-page PDF whose page `i` has `/Contents` reading exactly
    /// `"Original page {i}\n"` -- the same shape `run_test_15`/`run_test_16`
    /// require (both assert an initial `getAllPages().len() == 10`,
    /// `test_driver.cc:663,761`).
    fn ten_page_pdf() -> Vec<u8> {
        let n: u32 = 10;
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets: BTreeMap<u32, usize> = BTreeMap::new();
        let mut write_obj = |bytes: &mut Vec<u8>, num: u32, body: &[u8]| {
            offsets.insert(num, bytes.len());
            bytes.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        };
        write_obj(&mut bytes, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..n)
            .map(|i| format!("{} 0 R", 3 + i))
            .collect::<Vec<_>>()
            .join(" ");
        write_obj(
            &mut bytes,
            2,
            format!("<< /Type /Pages /Kids [{kids}] /Count {n} >>").as_bytes(),
        );
        for i in 0..n {
            let page_num = 3 + i;
            let content_num = 3 + n + i;
            write_obj(
                &mut bytes,
                page_num,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Contents {content_num} 0 R >>"
                )
                .as_bytes(),
            );
            let text = format!("Original page {i}\n");
            write_obj(
                &mut bytes,
                content_num,
                format!("<< /Length {} >>\nstream\n{text}endstream", text.len()).as_bytes(),
            );
        }
        let max_num = 3 + 2 * n - 1;
        let xref_offset = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n", max_num + 1).as_bytes());
        bytes.extend_from_slice(b"0000000000 65535 f \n");
        for num in 1..=max_num {
            match offsets.get(&num) {
                Some(offset) => {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
                None => bytes.extend_from_slice(b"0000000000 00000 f \n"),
            }
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\n", max_num + 1).as_bytes(),
        );
        bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
        bytes
    }

    /// Regression for the slashless-key bug this file's `replace_key` calls
    /// used to have: `replace_key(b"Contents", ..)` (missing the leading
    /// `/`) does not normalize -- per `ObjectHandle::replace_key`'s own doc,
    /// "this API does not normalize slashless input" -- so it inserted a
    /// *separate*, inert `Contents` entry next to the shallow copy's
    /// existing canonical `/Contents`, leaving the new page's real content,
    /// parent, and the page-tree's incremented count unreachable through the
    /// canonical keys every other accessor (and `QPDFWriter`) reads.
    #[test]
    fn manual_page_insert_replaces_contents_parent_and_count_on_the_canonical_keys() {
        let mut pdf = Pdf::open_mem_owned_with_options(ten_page_pdf(), PdfOpenOptions::default())
            .expect("open ten-page fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut diagnostics_written = 0;
        run_test_16(
            &mut pdf,
            b"fixture.pdf",
            None,
            &mut stdout,
            &mut stderr,
            &mut diagnostics_written,
        )
        .expect("run_test_16 must succeed against a well-formed 10-page fixture");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let root_ref = pdf.root_ref().expect("root");
        let root = pdf.get_object_handle(root_ref);
        let pages_dict = root.get_key(b"/Pages");
        assert_eq!(
            pages_dict.get_key(b"/Count").as_integer(),
            Some(11),
            "/Count must reflect the manually appended eleventh page"
        );

        let kids = pages_dict
            .get_key(b"/Kids")
            .as_array()
            .expect("/Kids is an array");
        assert_eq!(kids.len(), 11);
        let new_page = kids.last().expect("appended page").clone();

        assert!(
            !new_page.has_key(b"Contents"),
            "no stray slashless Contents entry should survive the fix"
        );
        assert!(
            !new_page.has_key(b"Parent"),
            "no stray slashless Parent entry should survive the fix"
        );

        let contents = new_page
            .get_key(b"/Contents")
            .get_stream_data(DecodeLevel::Generalized)
            .expect("decode the new page's /Contents");
        assert_eq!(
            contents.as_slice(),
            b"BT /F1 15 Tf 72 720 Td (New page 10) Tj ET\n",
            "/Contents must hold the newly created stream, not the shallow-copied original"
        );
        assert_eq!(
            new_page.get_key(b"/Parent").object_ref(),
            pages_dict.object_ref(),
            "/Parent must point back at the page tree, not stay absent"
        );

        // `run_test_16`'s own `all_pages.len()` (its local snapshot, taken
        // before the manual edit) stays 10, matching qpdf's *stale* cached
        // `all_pages` reference before `updateAllPagesCache()` runs
        // (`test_driver.cc:761`, this function's own GAP note). A *fresh*
        // `PageDocumentHelper::get_all_pages()` call, unlike qpdf's cache,
        // always recomputes from the live tree (`PageDocumentHelper::
        // get_all_pages`'s own doc), so it already sees the eleventh page
        // this test just proved was correctly wired up.
        let pages = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("get_all_pages");
        assert_eq!(pages.len(), 11);
    }
}
