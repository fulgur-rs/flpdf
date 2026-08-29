//! qpdf correspondence: `EmbeddedFileDocumentHelper` implements QPDFEmbeddedFileDocumentHelper.hh's public API (hasEmbeddedFiles, getEmbeddedFiles, getEmbeddedFile, replaceEmbeddedFile, removeEmbeddedFile).
//! Read/write access to the `/Names /EmbeddedFiles` name-tree.
//!
//! # Reader
//!
//! Walks the catalog's `/Names /EmbeddedFiles` name tree (ISO 32000-2 §7.9.6
//! + §7.11) and returns an ordered list of `(name_key, filespec_ref)` pairs.
//!
//! The result is in depth-first, key-ascending order as mandated by the spec
//! requirement that name trees be sorted by key.
//!
//! # Writer
//!
//! [`insert_embedded_file`] and [`delete_embedded_file`] delegate to the live
//! ObjectHandle name-tree helper. Unaffected nodes and the existing root
//! reference are retained; splits and `/Limits` repairs follow qpdf's NNTree
//! behavior.
//!
//! Insertion preserves a direct catalog `/Names` dictionary. Existing indirect
//! holders and tree roots are preserved.
//! Other keys in `/Names` (e.g. `/Dests`, `/JavaScript`) remain unchanged.
//!
//! Deletion preserves the `/Names /EmbeddedFiles` tree even when it becomes
//! empty, matching qpdf's `NNTreeIterator::remove` behavior.
//!
//! # Name-tree structure (ISO 32000-2 §7.9.6)
//!
//! A name tree node is a dictionary with either:
//! - `/Kids` — an array of indirect references to child nodes (intermediate),
//! - `/Names` — a flat array `[key₁, val₁, key₂, val₂, …]` (leaf).
//!
//! Intermediate and leaf nodes carry a `/Limits [least, greatest]` array
//! bounding the key range of their subtree; the root node omits it
//! (ISO 32000-2 §7.9.6).  For full enumeration (this module's purpose),
//! `/Limits` is informational: the tree is pre-sorted and DFS order already
//! yields keys in ascending order.  `/Limits` is *not* used to prune subtrees
//! here because we are collecting all entries, not searching for one.  Wherever
//! `/Limits` is present — including on a malformed root — it is simply skipped
//! without error.
//!
//! # Missing keys
//!
//! Any of `/Root`, `/Names`, `/EmbeddedFiles`, or the name-tree root being absent
//! results in an empty list (`Ok(vec![])`) rather than an error. I/O errors
//! propagate from [`Pdf::resolve`], [`crate::Error::Unsupported`] reports a
//! structural cycle from the name-tree walker, and [`crate::Error::Internal`]
//! reports an invalid first name-tree key, matching qpdf's iterator
//! dereference failure.
//!
//! # Value types
//!
//! Each name-tree value should be an indirect reference to a `/Filespec`
//! dictionary.  Values that are not [`crate::Object::Reference`] are skipped with a
//! diagnostic comment in source but no error; direct-dict filespecs embedded
//! directly in name arrays are exceedingly rare in practice and out of scope for
//! this read-only enumerator.
//!
//! # Examples
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{embedded_files, Pdf, ObjectRef};
//!
//! let mut pdf = Pdf::open(BufReader::new(File::open("with-attachments.pdf")?))?;
//! let entries = embedded_files::list_embedded_files(&mut pdf)?;
//! for (name, filespec_ref) in &entries {
//!     println!("{}: {}", String::from_utf8_lossy(name), filespec_ref);
//! }
//!
//! // Insert a new attachment key (the filespec object must already exist in pdf)
//! let filespec_ref = ObjectRef::new(42, 0);
//! embedded_files::insert_embedded_file(&mut pdf, b"report.pdf", filespec_ref)?;
//!
//! // Remove an entry
//! embedded_files::delete_embedded_file(&mut pdf, b"old-attachment.txt")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::nntree::NameTree;
use crate::{Error, ObjectHandle, ObjectRef, Pdf, Result};
use std::collections::BTreeMap;
use std::io::{Read, Seek};

/// High-level helper for a document's `/Names /EmbeddedFiles` name tree.
///
/// Construct with [`EmbeddedFileDocumentHelper::new`] or
/// [`Pdf::embedded_files`]. The helper does not cache name-tree state; each
/// method observes the document's current object graph.
pub struct EmbeddedFileDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
}

fn embedded_files_tree_with_options<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    auto_repair: bool,
    max_depth: Option<usize>,
) -> Result<Option<NameTree>> {
    // cov:ignore-start: helper methods normally receive a parsed catalog root
    let Some(catalog_ref) = pdf.root_ref() else {
        return Ok(None); // cov:ignore: helper methods normally receive a parsed catalog root
    };
    // cov:ignore-end
    let catalog = pdf.get_object_handle(catalog_ref);
    pdf.resolve(&catalog)?;
    if catalog.try_as_dictionary()?.is_none() {
        return Ok(None);
    }

    // Keep qpdf's getKey -> terminal-resolution order here. The public
    // has_key facade must resolve the child to test nullness, which would
    // move name-tree repair diagnostics before the tree walker sees the
    // malformed child.
    let names_seed = catalog.try_get_key(b"/Names")?;
    let names = pdf.resolve_to_terminal(&names_seed)?;
    if names.try_as_dictionary()?.is_none() {
        return Ok(None);
    }
    let root_seed = names.try_get_key(b"/EmbeddedFiles")?;
    let root = pdf.resolve_to_terminal(&root_seed)?;
    if root.try_as_dictionary()?.is_none() {
        return Ok(None);
    }

    let mut tree = NameTree::new(root, auto_repair);
    if let Some(max_depth) = max_depth {
        tree.set_max_depth(max_depth);
    }
    Ok(Some(tree))
}

impl<'a, R: Read + Seek> EmbeddedFileDocumentHelper<'a, R> {
    /// Create an embedded-files helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    fn embedded_files_tree(&mut self) -> Result<Option<NameTree>> {
        embedded_files_tree_with_options(self.pdf, true, None)
    }

    fn ensure_embedded_files_tree(&mut self) -> Result<Option<NameTree>> {
        if let Some(tree) = self.embedded_files_tree()? {
            return Ok(Some(tree));
        }

        // cov:ignore-start: qpdf always has a catalog root for this mutating helper
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None); // cov:ignore: qpdf always has a catalog root
        };
        // cov:ignore-end
        let catalog = self.pdf.get_object_handle(catalog_ref);
        self.pdf.resolve(&catalog)?;
        if catalog.try_as_dictionary()?.is_none() {
            return Ok(None);
        }

        let names = if catalog.has_key(b"/Names") {
            let candidate = catalog.get_key(b"/Names");
            let names = self.pdf.resolve_to_terminal(&candidate)?;
            if names.try_as_dictionary()?.is_some() {
                names
            } else {
                let names = ObjectHandle::dictionary(Vec::new());
                catalog.replace_key(b"/Names", names.clone())?;
                self.pdf.mark_object_handle_dirty(&catalog)?;
                names
            }
        } else {
            let names = ObjectHandle::dictionary(Vec::new());
            catalog.replace_key(b"/Names", names.clone())?;
            self.pdf.mark_object_handle_dirty(&catalog)?;
            names
        };

        let root = self.new_empty_embedded_files_root(&names)?;

        Ok(Some(NameTree::new(root, true)))
    }

    fn new_empty_embedded_files_root(&mut self, names: &ObjectHandle) -> Result<ObjectHandle> {
        let root = self
            .pdf
            .make_indirect_from_object_handle(ObjectHandle::dictionary(vec![(
                b"/Names".to_vec(),
                ObjectHandle::array(Vec::new()),
            )]))?;
        names.replace_key(b"/EmbeddedFiles", root.clone())?;
        self.pdf.mark_object_handle_dirty(names)?;
        Ok(root)
    }

    /// Return whether this document has an `/EmbeddedFiles` name tree.
    pub fn has_embedded_files(&mut self) -> Result<bool> {
        Ok(self.embedded_files_tree()?.is_some())
    }

    /// Return every embedded-files entry in key order.
    ///
    /// The values are qpdf-shaped Filespec object handles. Indirect values use
    /// the canonical handle of this document; direct Filespec dictionaries are
    /// returned as direct handles.
    pub fn get_embedded_files(&mut self) -> Result<BTreeMap<Vec<u8>, ObjectHandle>> {
        let Some(mut tree) = self.embedded_files_tree()? else {
            return Ok(BTreeMap::new());
        };
        tree.as_map(self.pdf)
    }

    /// Return the Filespec handle stored under `key`, if present.
    pub fn get_embedded_file(&mut self, key: &[u8]) -> Result<Option<ObjectHandle>> {
        let Some(mut tree) = self.embedded_files_tree()? else {
            return Ok(None);
        };
        tree.find_object(self.pdf, key)
    }

    /// Add or replace the Filespec stored under `key`.
    ///
    /// An indirect handle must be the canonical handle of this document;
    /// direct handles are stored directly in the name tree.
    pub fn replace_embedded_file(&mut self, key: &[u8], filespec: ObjectHandle) -> Result<()> {
        if let Some(object_ref) = filespec.object_ref() {
            if !self.pdf.is_canonical_object_handle(&filespec) {
                return Err(Error::Unsupported(
                    "filespec handle belongs to another Pdf".to_string(),
                ));
            }
            debug_assert_eq!(filespec.object_ref(), Some(object_ref));
        } else if !filespec.belongs_to_pdf(self.pdf.unique_id()) {
            return Err(Error::Unsupported(
                "filespec handle belongs to another Pdf".to_string(),
            ));
        }

        let Some(mut tree) = self.ensure_embedded_files_tree()? else {
            return Ok(());
        };
        tree.insert(self.pdf, key, filespec).map(|_| ())
    }

    /// Remove the Filespec stored under `key`.
    ///
    /// Returns `false` if the embedded-files tree or the named entry is
    /// absent. An indirect Filespec is replaced with `null`, matching qpdf's
    /// `removeEmbeddedFile`; direct Filespec values have no object slot to
    /// replace. This method intentionally does not perform `/AF` cleanup or
    /// reachability-based garbage collection.
    pub fn remove_embedded_file(&mut self, key: &[u8]) -> Result<bool> {
        let Some(mut tree) = self.embedded_files_tree()? else {
            return Ok(false);
        };
        let Some(removed) = tree.remove(self.pdf, key)? else {
            return Ok(false);
        };
        if let Some(object_ref) = removed.object_ref() {
            // qpdf's removeEmbeddedFile keeps the xref slot as a null object
            // (`QPDFEmbeddedFileDocumentHelper.cc:115-119`). The writer then
            // decides whether the detached Filespec and its streams are
            // emitted: ordinary rewrites garbage-collect them, while
            // preserve-unreferenced retains them.
            self.pdf
                .replace_object_handle(object_ref, ObjectHandle::null())?;
        }
        Ok(true)
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level embedded-files helper for this document.
    pub fn embedded_files(&mut self) -> EmbeddedFileDocumentHelper<'_, R> {
        EmbeddedFileDocumentHelper::new(self)
    }
}

// ── remove_attachment ─────────────────────────────────────────────────────────

/// Remove an attachment by name-tree key and garbage-collect unreachable
/// payload objects.
///
/// # Behaviour
///
/// 1. Looks up `key` in the catalog's `/Names /EmbeddedFiles` name tree.
///    Returns `Ok(false)` — without error — if the key is absent.
/// 2. Calls [`EmbeddedFileDocumentHelper::remove_embedded_file`], which
///    removes the name-tree entry and **unconditionally** replaces an
///    indirect Filespec with null, matching qpdf's `removeEmbeddedFile`
///    contract — regardless of any other live reference to that same object
///    (an `/AF` entry, a `/Dests` / `/JavaScript` name tree, another
///    Filespec). Associated-files (`/AF`) arrays are not modified, so an
///    `/AF` entry that pointed at the removed Filespec now points at null.
/// 3. **Mark-and-sweep GC** through the writer-owned reachability pass:
///    every indirect object no longer reachable from `/Root` or the trailer
///    is physically deleted. This always drops the Filespec's original
///    content — its `/EF` streams (including a filespec carrying distinct
///    streams under several `/EF` keys) and any sub-objects reachable only
///    through it become unreachable the instant step 2 nulls the Filespec,
///    so the sweep removes them regardless of what else is live. The null
///    object *slot* the Filespec occupied is a separate question: it
///    survives the sweep, still emitted as `null`, if something else (most
///    commonly `/AF`) still holds an indirect reference to that object
///    number; otherwise the slot itself is swept away too. The sweep also
///    drops the orphan ghost name-tree nodes left by the rebuild — all in
///    one pass, with no per-feature reachability heuristics.
///
/// The conservative-share semantics only protect content that was never
/// routed through the removed Filespec in the first place: an `/EmbeddedFile`
/// stream shared by a *different*, still-live Filespec object stays
/// reachable through that other Filespec and survives. A `/Dests` /
/// `/JavaScript` name tree entry that points at the *same* Filespec object
/// being removed does **not** preserve it — step 2 nulls that object
/// unconditionally, so the other name tree ends up pointing at null too.
///
/// # Blast radius
///
/// The sweep is **document-wide**, not scoped to the removed attachment: any
/// *pre-existing* object that was already unreachable from `/Root` is also
/// collected. This matches qpdf's complete-rewrite behaviour (its writer only
/// emits reachable objects) and flpdf's own page-subset pruning, so the
/// observable output is qpdf-aligned rather than a targeted in-place edit.
///
/// # Limitation
///
/// When the name-tree value is a *direct* `/Filespec` dictionary (not an
/// indirect reference), qpdf has no object slot to replace; the name-tree
/// entry is removed and the sweep still runs.
///
/// # Errors
///
/// Propagates any error from the canonical embedded-files helper or the sweep.
pub fn remove_attachment<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<bool> {
    let removed = pdf.embedded_files().remove_embedded_file(key)?;
    if !removed {
        return Ok(false);
    }

    // The null Filespec remains reachable through any existing `/AF` array,
    // while its embedded streams and name-tree ghosts become unreachable.
    crate::writer::reachability::sweep_unreachable_objects(pdf)?;

    Ok(true)
}

// ── helpers for remove_attachment ─────────────────────────────────────────────

/// Walk a `/Filespec` dict and return the first `/EmbeddedFile` stream `ObjectRef`
/// reachable via `/EF /UF`, `/EF /F`, `/EF /Unix`, `/EF /Mac`, `/EF /DOS` (in
/// that priority order).  Returns `None` if not found or on any soft error.
///
/// Test-only helper for single-stream fixtures. Production code no longer
/// resolves `/EF` streams explicitly: [`remove_attachment`] relies on the
/// `/Root` mark-and-sweep, which drops every `/EF` stream of a removed
/// filespec transitively.
#[cfg(test)]
fn embedded_file_stream_ref<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    filespec_ref: ObjectRef,
) -> Result<Option<ObjectRef>> {
    let candidate = {
        let filespec = pdf.get_object_handle(filespec_ref);
        let mut filespec = crate::filespec_helper::FileSpec::new(filespec, pdf)?;
        filespec.get_embedded_file_stream("")?
    };
    Ok(candidate.object_ref())
}

// ── Writer constants ──────────────────────────────────────────────────────────

/// Legacy maximum depth for callers that explicitly select bounded traversal.
///
/// qpdf's embedded-files helper does not use a numeric depth cap; its normal
/// read, replace, and remove paths rely on the name-tree walker's cycle
/// detection instead.
pub const DEFAULT_MAX_EMBEDDED_FILES_DEPTH: usize = 100;

/// Enumerate all `(name_key, filespec_ref)` entries in the catalog's
/// `/Names /EmbeddedFiles` name tree.
///
/// Returns entries in depth-first, key-ascending order (the order they appear
/// in the tree, which the spec requires to be sorted).  An empty list is
/// returned — without error — when any of `/Root`, `/Names`, or
/// `/EmbeddedFiles` is absent.
///
/// **Semantics:** name-tree values that are *direct* `/Filespec` dictionaries
/// (rather than indirect references) are intentionally **skipped** — this
/// reader only surfaces `(key, ObjectRef)` pairs. Mutation and copying use the
/// handle-valued [`EmbeddedFileDocumentHelper::get_embedded_files`] projection,
/// which preserves direct-dict values.
// TODO(flpdf-9hc.10.6): consider exposing direct-dict entries via the public
// list/show API (e.g. an `Object`-valued variant) once list/show land.
///
/// List every embedded file referenced by an indirect `/Filespec` entry,
/// returning each entry's name and the [`ObjectRef`] of its file-specification
/// dictionary. Name-tree entries whose value is a *direct* `/Filespec`
/// dictionary (rather than an indirect reference) are intentionally skipped.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`], and returns
/// [`crate::Error::Unsupported`] for an explicit bounded-traversal limit.
pub fn list_embedded_files<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<Vec<(Vec<u8>, ObjectRef)>> {
    list_embedded_files_with_max_depth(pdf, DEFAULT_MAX_EMBEDDED_FILES_DEPTH)
}

/// Like [`list_embedded_files`] but with a caller-supplied depth limit.
///
/// The depth limit guards against maliciously or accidentally cyclic `/Kids`
/// references.  Exceeding the limit returns an error rather than panicking.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`], and returns
/// [`crate::Error::Unsupported`] if a `/Kids` chain depth reaches `max_depth`.
pub fn list_embedded_files_with_max_depth<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    max_depth: usize,
) -> Result<Vec<(Vec<u8>, ObjectRef)>> {
    let Some(mut tree) = embedded_files_tree_with_options(pdf, true, Some(max_depth))? else {
        return Ok(vec![]);
    };
    Ok(tree
        .as_map(pdf)?
        .into_iter()
        .filter_map(|(key, value)| {
            value
                .object_ref()
                .or_else(|| value.as_reference())
                .map(|object_ref| (key, object_ref))
        })
        .collect())
}

// ── Writer ────────────────────────────────────────────────────────────────────

/// Insert or replace a `(key, filespec_ref)` entry in the catalog's
/// `/Names /EmbeddedFiles` name tree.
///
/// If `key` already exists its value is replaced with `filespec_ref`.
/// If the `/Names /EmbeddedFiles` path does not yet exist it is created.
///
/// The existing tree is mutated in place; an existing root reference is
/// retained.
///
/// # Errors
///
/// Propagates any error from [`Pdf::resolve`].
pub fn insert_embedded_file<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    key: &[u8],
    filespec_ref: ObjectRef,
) -> Result<()> {
    let filespec = pdf.get_object_handle(filespec_ref);
    pdf.embedded_files().replace_embedded_file(key, filespec)
}

/// Remove the entry with `key` from the catalog's `/Names /EmbeddedFiles`
/// name tree.
///
/// Returns `true` if the key was found and removed, `false` if it was absent.
///
/// When the last entry is removed, `/EmbeddedFiles` remains as an empty name
/// tree, as it does in qpdf.
///
/// # Errors
///
/// Propagates any error from the canonical embedded-files helper.
pub fn delete_embedded_file<R: Read + Seek>(pdf: &mut Pdf<R>, key: &[u8]) -> Result<bool> {
    pdf.embedded_files().remove_embedded_file(key)
}

// ── Tests for remove_attachment ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filespec_helper::FileSpecBuilder;
    use crate::job::add_attachment_from_path;

    // ── Minimal PDF fixture (same as filespec_helper tests) ───────────────────

    fn minimal_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_minimal() -> Pdf<std::io::Cursor<Vec<u8>>> {
        Pdf::open(std::io::Cursor::new(minimal_pdf_bytes())).expect("open minimal PDF")
    }

    fn next_object_number(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> u32 {
        pdf.next_available_object_ref()
            .expect("object-number space must have room in the test fixture")
            .number
            .checked_sub(1)
            .expect("minimal test fixture has object 1 as its first object")
    }

    fn handle_array(items: Vec<ObjectHandle>) -> ObjectHandle {
        ObjectHandle::array(items)
    }

    fn handle_dictionary(entries: Vec<(&[u8], ObjectHandle)>) -> ObjectHandle {
        ObjectHandle::dictionary(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_vec(), value))
                .collect(),
        )
    }

    fn set_test_object(
        pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>,
        object_ref: ObjectRef,
        value: ObjectHandle,
    ) -> ObjectHandle {
        pdf.set_object_handle(object_ref, value)
            .expect("set canonical test object");
        pdf.get_object_handle(object_ref)
    }

    fn resolved_handle(
        pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>,
        object_ref: ObjectRef,
    ) -> ObjectHandle {
        let handle = pdf.get_object_handle(object_ref);
        pdf.resolve(&handle).expect("resolve canonical test object");
        handle
    }

    fn catalog_handle(pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>) -> ObjectHandle {
        let catalog_ref = pdf.root_ref().expect("root");
        resolved_handle(pdf, catalog_ref)
    }

    fn replace_catalog_key(
        pdf: &mut Pdf<std::io::Cursor<Vec<u8>>>,
        key: &[u8],
        value: ObjectHandle,
    ) {
        let catalog = catalog_handle(pdf);
        catalog
            .replace_key(key, value)
            .expect("replace catalog key");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");
    }

    fn indirect_names_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names 4 0 R >>\nendobj\n");
        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /EmbeddedFiles << /Names [ (entry) 5 0 R ] >> >>\nendobj\n",
        );
        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Type /Filespec /F (entry.txt) >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 6\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3, off4, off5,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer =
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    // ── Test: add 2, remove 1, check list has 1 ──────────────────────────────

    #[test]
    fn remove_one_of_two_leaves_other_intact() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");

        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, b"content A").unwrap();
        std::fs::write(&file_b, b"content B").unwrap();

        add_attachment_from_path(&mut pdf, b"a.txt", &file_a).expect("add a");
        let fs_b = add_attachment_from_path(&mut pdf, b"b.txt", &file_b).expect("add b");

        let removed = remove_attachment(&mut pdf, b"a.txt").expect("remove a");
        assert!(
            removed,
            "remove_attachment must return true for existing key"
        );

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1, "exactly one attachment must remain");
        assert_eq!(entries[0].0, b"b.txt", "b.txt must survive");
        assert_eq!(entries[0].1, fs_b, "surviving filespec ref must match");

        // Deleted key must not appear
        let keys: Vec<&[u8]> = entries.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(!keys.contains(&b"a.txt".as_ref()), "a.txt must be gone");
    }

    #[test]
    fn helper_reads_do_not_dirty_an_unchanged_indirect_names_dictionary() {
        let mut pdf = Pdf::open(std::io::Cursor::new(indirect_names_pdf_bytes())).expect("open");
        let names_ref = ObjectRef::new(4, 0);
        assert!(!pdf.is_dirty(names_ref));

        assert_eq!(
            pdf.embedded_files()
                .get_embedded_files()
                .expect("list")
                .len(),
            1
        );
        assert!(!pdf.is_dirty(names_ref));

        assert!(pdf
            .embedded_files()
            .get_embedded_file(b"entry")
            .expect("lookup")
            .is_some());
        assert!(!pdf.is_dirty(names_ref));
    }

    #[test]
    fn helper_absent_removal_does_not_dirty_an_unchanged_indirect_names_dictionary() {
        let mut pdf = Pdf::open(std::io::Cursor::new(indirect_names_pdf_bytes())).expect("open");
        let names_ref = ObjectRef::new(4, 0);
        assert!(!pdf.is_dirty(names_ref));

        assert!(!pdf
            .embedded_files()
            .remove_embedded_file(b"missing")
            .expect("absent removal"));

        assert!(
            !pdf.is_dirty(names_ref),
            "an absent removal must not rewrite the unchanged /Names dictionary"
        );
    }

    #[test]
    fn legacy_insert_updates_retained_embedded_files_root() {
        let mut pdf = Pdf::open(std::io::Cursor::new(indirect_names_pdf_bytes())).expect("open");
        let catalog_ref = pdf.root_ref().expect("root");
        let catalog = pdf.get_object_handle(catalog_ref);
        pdf.resolve(&catalog).expect("resolve catalog");
        let retained_root = catalog.get_key(b"/Names").get_key(b"/EmbeddedFiles");
        pdf.resolve(&retained_root)
            .expect("resolve embedded-files root");

        let filespec_ref = ObjectRef::new(90, 0);
        set_test_object(&mut pdf, filespec_ref, handle_dictionary(Vec::new()));
        insert_embedded_file(&mut pdf, b"new.txt", filespec_ref).expect("insert");

        let pairs = retained_root
            .get_key(b"/Names")
            .as_array()
            .expect("retained names array");
        assert_eq!(pairs.len(), 4, "the retained root must observe insertion");
        assert_eq!(pairs[2].as_string(), Some(b"new.txt".to_vec()));
    }

    #[test]
    fn remove_attachment_preserves_retained_af_array_handle_and_nulls_filespec() {
        let mut pdf = open_minimal();
        let filespec_ref = FileSpecBuilder::new("retained-af.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"retained-af.txt", filespec_ref).expect("insert");

        let af_ref = ObjectRef::new(next_object_number(&mut pdf) + 1, 0);
        let filespec_handle = pdf.get_object_handle(filespec_ref);
        set_test_object(&mut pdf, af_ref, handle_array(vec![filespec_handle]));
        let af_handle = pdf.get_object_handle(af_ref);
        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/AF", af_handle.clone())
            .expect("replace catalog /AF");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");
        let page_ref = crate::pages::page_refs(&mut pdf)
            .expect("page refs")
            .into_iter()
            .next()
            .expect("page");
        let page = resolved_handle(&mut pdf, page_ref);
        page.replace_key(b"/AF", af_handle)
            .expect("replace page /AF");
        pdf.mark_object_handle_dirty(&page)
            .expect("mark page dirty");

        let retained_af = pdf.get_object_handle(af_ref);
        pdf.resolve(&retained_af).expect("resolve AF array");

        assert!(
            remove_attachment(&mut pdf, b"retained-af.txt").expect("remove"),
            "attachment must be found"
        );

        assert_eq!(
            retained_af.as_array().expect("retained AF array").len(),
            1,
            "qpdf keeps the retained AF array element"
        );
        assert!(
            resolved_handle(&mut pdf, filespec_ref).is_null(),
            "qpdf replaces the removed Filespec with null"
        );
    }

    #[test]
    fn helper_mutates_a_retained_direct_embedded_files_root() {
        let mut pdf = open_minimal();
        let root = handle_dictionary(vec![(b"/Names", handle_array(Vec::new()))]);
        let names = handle_dictionary(vec![(b"/EmbeddedFiles", root)]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        let catalog_handle = catalog_handle(&mut pdf);
        let retained_root = catalog_handle.get_key(b"/Names").get_key(b"/EmbeddedFiles");
        let retained_pairs = retained_root.get_key(b"/Names");

        let filespec_ref = ObjectRef::new(90, 0);
        set_test_object(&mut pdf, filespec_ref, handle_dictionary(Vec::new()));
        let filespec = pdf.get_object_handle(filespec_ref);
        pdf.embedded_files()
            .replace_embedded_file(b"entry", filespec)
            .expect("replace");

        assert!(
            retained_root
                .get_key(b"/Names")
                .as_array()
                .is_some_and(|pairs| pairs.len() == 2),
            "a retained direct root must observe helper replacement"
        );
        assert_eq!(
            retained_pairs
                .as_array()
                .expect("retained names array")
                .len(),
            2,
            "a retained direct leaf array must observe helper replacement"
        );

        assert!(pdf
            .embedded_files()
            .remove_embedded_file(b"entry")
            .expect("remove"));
        assert_eq!(
            retained_root
                .get_key(b"/Names")
                .as_array()
                .expect("names array")
                .len(),
            0,
            "a retained direct root must observe helper removal"
        );
        assert_eq!(
            retained_pairs
                .as_array()
                .expect("retained names array")
                .len(),
            0,
            "a retained direct leaf array must observe helper removal"
        );
        assert!(pdf.embedded_files().has_embedded_files().expect("has tree"));
        assert!(!pdf
            .embedded_files()
            .remove_embedded_file(b"missing")
            .expect("missing removal"));
    }

    #[test]
    fn helper_mutates_an_indirect_root_without_detaching_direct_filespec_handle() {
        let mut pdf = open_minimal();
        let root_ref = ObjectRef::new(90, 0);
        let added_ref = ObjectRef::new(91, 0);

        let existing_filespec =
            handle_dictionary(vec![(b"/F", ObjectHandle::string(b"old.txt".to_vec()))]);
        let root = handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![ObjectHandle::string(b"a".to_vec()), existing_filespec]),
        )]);
        let root_handle = set_test_object(&mut pdf, root_ref, root);
        set_test_object(&mut pdf, added_ref, handle_dictionary(Vec::new()));
        let names = handle_dictionary(vec![(b"/EmbeddedFiles", root_handle)]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        let catalog_handle = catalog_handle(&mut pdf);
        let retained_root = catalog_handle.get_key(b"/Names").get_key(b"/EmbeddedFiles");
        pdf.resolve(&retained_root)
            .expect("resolve embedded-files root");
        let retained_filespec = retained_root
            .get_key(b"/Names")
            .as_array()
            .expect("names array")[1]
            .clone();

        let added = pdf.get_object_handle(added_ref);
        pdf.embedded_files()
            .replace_embedded_file(b"b", added)
            .expect("replace");

        pdf.embedded_files()
            .replace_embedded_file(b"c", retained_filespec.clone())
            .expect("insert direct filespec");
        let inserted_direct = pdf
            .embedded_files()
            .get_embedded_file(b"c")
            .expect("lookup direct filespec")
            .expect("direct filespec");
        assert!(
            inserted_direct.is_same_object_as(&retained_filespec),
            "qpdf stores the direct Filespec handle itself in the name tree"
        );

        assert!(pdf
            .embedded_files()
            .remove_embedded_file(b"b")
            .expect("remove indirect filespec"));

        retained_filespec
            .replace_key(b"/F", ObjectHandle::string(b"new.txt".to_vec()))
            .unwrap();
        let current_filespec = retained_root
            .get_key(b"/Names")
            .as_array()
            .expect("updated names array")[1]
            .clone();
        assert_eq!(
            current_filespec.get_key(b"/F").as_string(),
            Some(b"new.txt".to_vec())
        );
    }

    #[test]
    fn helper_mutates_a_retained_direct_kids_root() {
        let mut pdf = open_minimal();
        let leaf = handle_dictionary(vec![
            (
                b"/Names",
                handle_array(vec![
                    ObjectHandle::string(b"a".to_vec()),
                    ObjectHandle::integer(1),
                ]),
            ),
            (
                b"/Limits",
                handle_array(vec![
                    ObjectHandle::string(b"a".to_vec()),
                    ObjectHandle::string(b"a".to_vec()),
                ]),
            ),
        ]);
        let root = handle_dictionary(vec![(b"/Kids", handle_array(vec![leaf]))]);
        let names = handle_dictionary(vec![(b"/EmbeddedFiles", root)]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        let catalog_handle = catalog_handle(&mut pdf);
        let retained_root = catalog_handle.get_key(b"/Names").get_key(b"/EmbeddedFiles");

        let filespec_ref = ObjectRef::new(90, 0);
        set_test_object(&mut pdf, filespec_ref, handle_dictionary(Vec::new()));
        let filespec = pdf.get_object_handle(filespec_ref);
        pdf.embedded_files()
            .replace_embedded_file(b"b", filespec)
            .expect("replace");

        assert!(
            retained_root
                .get_key(b"/Kids")
                .as_array()
                .and_then(|kids| kids.first().cloned())
                .is_some_and(|kid| kid.is_indirect()),
            "a retained direct /Kids root must observe direct-kid repair"
        );
    }

    #[test]
    fn helper_preserves_an_untouched_direct_filespec_handle_during_root_update() {
        let mut pdf = open_minimal();
        let filespec = handle_dictionary(vec![
            (b"/Type", ObjectHandle::name(b"Filespec".to_vec())),
            (b"/F", ObjectHandle::string(b"old.txt".to_vec())),
        ]);
        let root = handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![ObjectHandle::string(b"a".to_vec()), filespec]),
        )]);
        let names = handle_dictionary(vec![(b"/EmbeddedFiles", root)]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        let catalog_handle = catalog_handle(&mut pdf);
        let retained_root = catalog_handle.get_key(b"/Names").get_key(b"/EmbeddedFiles");
        let retained_filespec = retained_root
            .get_key(b"/Names")
            .as_array()
            .expect("names array")[1]
            .clone();

        let added_ref = ObjectRef::new(90, 0);
        set_test_object(&mut pdf, added_ref, handle_dictionary(Vec::new()));
        let added = pdf.get_object_handle(added_ref);
        pdf.embedded_files()
            .replace_embedded_file(b"b", added)
            .expect("replace");

        retained_filespec
            .replace_key(b"/F", ObjectHandle::string(b"new.txt".to_vec()))
            .unwrap();
        let current_filespec = retained_root
            .get_key(b"/Names")
            .as_array()
            .expect("updated names array")[1]
            .clone();
        assert_eq!(
            current_filespec.get_key(b"/F").as_string(),
            Some(b"new.txt".to_vec())
        );
    }

    // ── Test: transitively-unreachable subgraph is swept (flpdf-eg3) ─────────
    //
    // The old ad-hoc GC only ever considered the filespec ref and its `/EF`
    // streams, so an object reachable *only* through the filespec dictionary
    // (e.g. an indirect `/CI` collection-item stream) was left behind as an
    // orphan after removal. A proper mark-and-sweep from `/Root` + trailer —
    // the qpdf rewrite model — drops the whole now-unreachable subgraph.
    #[test]
    fn remove_attachment_sweeps_transitively_unreachable_subgraph() {
        let mut pdf = open_minimal();

        // A side-car stream that will be reachable ONLY via the filespec dict.
        let next = next_object_number(&mut pdf);
        let sidecar_ref = ObjectRef::new(next + 1, 0);
        let sidecar = ObjectHandle::stream(
            ObjectHandle::dictionary(Vec::new()),
            std::rc::Rc::new(b"sidecar".to_vec()),
        );
        set_test_object(&mut pdf, sidecar_ref, sidecar);

        // Build a filespec, then point an indirect key at the side-car so the
        // side-car is reachable exclusively through the filespec.
        let fs_ref = FileSpecBuilder::new("trans.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        let filespec = resolved_handle(&mut pdf, fs_ref);
        filespec
            .replace_key(b"/CI", pdf.get_object_handle(sidecar_ref))
            .expect("add side-car reference");
        pdf.mark_object_handle_dirty(&filespec)
            .expect("mark filespec dirty");
        insert_embedded_file(&mut pdf, b"trans.txt", fs_ref).expect("insert");

        remove_attachment(&mut pdf, b"trans.txt").expect("remove");

        let live = pdf.live_object_refs();
        assert!(!live.contains(&fs_ref), "filespec must be swept");
        assert!(
            !live.contains(&sidecar_ref),
            "object reachable only via the filespec must be transitively swept (mark-and-sweep)"
        );
    }

    #[test]
    fn embedded_file_stream_ref_accepts_indirect_ef_dict() {
        let mut pdf = open_minimal();
        let fs_ref = FileSpecBuilder::new("indirect-ef.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        let filespec = resolved_handle(&mut pdf, fs_ref);
        let ef_dict = filespec.get_key(b"/EF").as_dictionary().expect("/EF dict");
        let ef_ref = ObjectRef::new(fs_ref.number + 100, 0);
        let ef_handle = ObjectHandle::dictionary(ef_dict.into_iter().collect());
        set_test_object(&mut pdf, ef_ref, ef_handle);
        filespec
            .replace_key(b"/EF", pdf.get_object_handle(ef_ref))
            .expect("replace /EF with indirect dictionary");
        pdf.mark_object_handle_dirty(&filespec)
            .expect("mark filespec dirty");

        let stream_ref = embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream")
            .expect("stream ref");

        assert!(pdf.object_refs().contains(&stream_ref));
    }

    // ── Test: removed filespec and stream are no longer live ─────────────────

    #[test]
    fn remove_attachment_gc_deletes_filespec_and_stream() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("gc.txt");
        std::fs::write(&file, b"gc test").unwrap();

        let fs_ref = add_attachment_from_path(&mut pdf, b"gc.txt", &file).expect("add");

        // Resolve the stream ref before removal.
        let stream_ref = embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve_stream_ref")
            .expect("stream ref must exist");

        remove_attachment(&mut pdf, b"gc.txt").expect("remove");

        // Both filespec and stream must be absent from live objects.
        let live = pdf.live_object_refs();
        assert!(
            !live.contains(&fs_ref),
            "filespec ref must not be in live_object_refs after GC"
        );
        assert!(
            !live.contains(&stream_ref),
            "stream ref must not be in live_object_refs after GC"
        );
    }

    // ── Test: indirect /AF array retains the null Filespec reference ─────────
    //
    // qpdf keeps an indirect /AF array and its reference to the nulled
    // Filespec, while the embedded stream becomes unreachable and is swept.
    #[test]
    fn remove_attachment_with_indirect_af_array_gcs_filespec_and_stream() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("idx.txt", b"indirect-af payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"idx.txt", fs_ref).expect("insert");

        let stream_ref = embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream ref")
            .expect("stream ref must exist");

        // Allocate a standalone array object [fs_ref] and point catalog /AF at
        // it *indirectly* (the only reference to this array object).
        let next = next_object_number(&mut pdf);
        let af_array_ref = ObjectRef::new(next + 1, 0);
        let filespec_handle = pdf.get_object_handle(fs_ref);
        set_test_object(&mut pdf, af_array_ref, handle_array(vec![filespec_handle]));

        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/AF", pdf.get_object_handle(af_array_ref))
            .expect("replace catalog /AF");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        let removed = remove_attachment(&mut pdf, b"idx.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&fs_ref),
            "null Filespec must remain reachable through the indirect /AF array"
        );
        assert!(
            !live.contains(&stream_ref),
            "embedded stream must be GC-deleted alongside the filespec"
        );
        assert!(
            live.contains(&af_array_ref),
            "qpdf keeps the indirect /AF array referenced by the catalog"
        );

        // Catalog /AF and its null Filespec reference remain.
        let catalog2 = catalog_handle(&mut pdf);
        assert_eq!(
            catalog2.get_key(b"/AF").object_ref(),
            Some(af_array_ref),
            "catalog /AF must remain in place"
        );
        assert!(
            resolved_handle(&mut pdf, fs_ref).is_null(),
            "Filespec must be nulled, not removed"
        );
    }

    // ── Test: indirect /AF shared by catalog + page retains null Filespec ────
    //
    // The same indirect /AF array object is referenced by both catalog and
    // page. qpdf keeps both parent references and the null Filespec element.
    #[test]
    fn remove_attachment_shared_indirect_af_across_catalog_and_page_not_dangled() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("sh.txt", b"shared-af payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"sh.txt", fs_ref).expect("insert");

        // One indirect /AF array object [fs_ref], referenced by BOTH the
        // catalog and the page.
        let next = next_object_number(&mut pdf);
        let af_array_ref = ObjectRef::new(next + 1, 0);
        let filespec_handle = pdf.get_object_handle(fs_ref);
        set_test_object(&mut pdf, af_array_ref, handle_array(vec![filespec_handle]));

        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/AF", pdf.get_object_handle(af_array_ref))
            .expect("replace catalog /AF");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        let page_refs = crate::pages::page_refs(&mut pdf).expect("page_refs");
        assert_eq!(page_refs.len(), 1, "fixture has one page");
        let page_ref = page_refs[0];
        let page = resolved_handle(&mut pdf, page_ref);
        page.replace_key(b"/AF", pdf.get_object_handle(af_array_ref))
            .expect("replace page /AF");
        pdf.mark_object_handle_dirty(&page)
            .expect("mark page dirty");

        // Removal walks catalog then every page, calling the helper once per
        // parent against the SAME shared array object.
        let removed = remove_attachment(&mut pdf, b"sh.txt").expect("remove");
        assert!(removed);

        // The shared array object must still resolve for every parent and keep
        // the removed Filespec reference.
        let af_after = resolved_handle(&mut pdf, af_array_ref)
            .as_array()
            .expect("shared indirect /AF array must still resolve (not deleted)");
        assert_eq!(
            af_after.len(),
            1,
            "shared /AF array must retain the nulled Filespec reference"
        );
        assert_eq!(af_after[0].object_ref(), Some(fs_ref));

        // The null Filespec remains reachable through the shared array.
        let live = pdf.live_object_refs();
        assert!(
            live.contains(&fs_ref),
            "null Filespec must remain reachable through the shared /AF array"
        );

        // Page /AF must still point at the surviving shared array.
        let page_after = resolved_handle(&mut pdf, page_ref);
        assert_eq!(
            page_after.get_key(b"/AF").object_ref(),
            Some(af_array_ref),
            "page /AF must still point at the surviving shared array"
        );
        assert!(
            resolved_handle(&mut pdf, fs_ref).is_null(),
            "Filespec must be nulled, not removed"
        );
    }

    // ── Test: another live name tree retains the null Filespec reference ────
    //
    // qpdf replaces the Filespec object with null even when a live /Dests
    // name tree still retains its object reference.
    #[test]
    fn remove_attachment_preserves_filespec_referenced_by_other_name_tree() {
        let mut pdf = open_minimal();

        // Register a filespec under /Names /EmbeddedFiles.
        let fs_ref = FileSpecBuilder::new("shared.txt", b"shared payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"shared.txt", fs_ref).expect("insert");

        let stream_ref = embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream ref")
            .expect("stream ref must exist");

        // A separate, type-less name-tree leaf (models a /Dests name tree) that
        // legitimately references the SAME filespec.
        let next = next_object_number(&mut pdf);
        let dests_leaf_ref = ObjectRef::new(next + 1, 0);
        let dests_leaf = handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![
                ObjectHandle::string(b"shared-dest".to_vec()),
                pdf.get_object_handle(fs_ref),
            ]),
        )]);
        set_test_object(&mut pdf, dests_leaf_ref, dests_leaf);

        // Hang it off the catalog's /Dests so it is reachable from the catalog
        // (a legitimate live name tree, not a dead ghost).
        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/Dests", pdf.get_object_handle(dests_leaf_ref))
            .expect("replace catalog /Dests");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        // Remove the embedded-files attachment. qpdf nulls the Filespec
        // object even when another name tree still references its object ref.
        let removed = remove_attachment(&mut pdf, b"shared.txt").expect("remove");
        assert!(removed, "existing key must report removed");

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&fs_ref),
            "the null Filespec ref remains reachable through /Dests"
        );
        assert!(
            resolved_handle(&mut pdf, fs_ref).is_null(),
            "qpdf replaces the shared Filespec with null"
        );

        // The /Dests reference itself must remain intact.
        let leaf = resolved_handle(&mut pdf, dests_leaf_ref);
        let names = leaf
            .get_key(b"/Names")
            .as_array()
            .expect("dests leaf names");
        assert!(
            names.iter().any(|value| value.object_ref() == Some(fs_ref)),
            "/Dests leaf must still reference the filespec"
        );

        // The Filespec is null, so its embedded stream is no longer reachable
        // through that object and is swept.
        assert!(
            !live.contains(&stream_ref),
            "embedded stream must be GC-deleted after the Filespec is nulled"
        );
    }

    // ── Test: live object referencing the stream (with stream back-ref) ──────
    //
    // qpdf replaces the removed Filespec with null. The externally referenced
    // stream survives, but its dictionary's back-reference resolves to null
    // and is not a writer reachability edge (`QPDFWriter.cc:1131-1135`), so the
    // Filespec itself is still collected.
    #[test]
    fn remove_attachment_keeps_external_stream_but_collects_nulled_filespec() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("paired.txt", b"paired payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"paired.txt", fs_ref).expect("insert");

        let stream_ref = embedded_file_stream_ref(&mut pdf, fs_ref)
            .expect("resolve stream ref")
            .expect("stream ref must exist");

        // Make the stream dictionary back-reference the filespec (pathological
        // but legal) and have a live, catalog-reachable object reference the
        // stream so conservative GC must preserve it.
        let stream = resolved_handle(&mut pdf, stream_ref);
        let stream_dict = stream.as_stream_dict().expect("stream dictionary");
        stream_dict
            .replace_key(b"/RelatedFS", pdf.get_object_handle(fs_ref))
            .expect("add stream back-reference");
        pdf.mark_object_handle_dirty(&stream)
            .expect("mark stream dirty");

        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/ExtraStreamRef", pdf.get_object_handle(stream_ref))
            .expect("replace catalog stream reference");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        let removed = remove_attachment(&mut pdf, b"paired.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&stream_ref),
            "externally-referenced stream must be preserved"
        );
        assert!(!live.contains(&fs_ref), "nulled filespec must be collected");
    }

    // ── Test: shared embedded stream is preserved, removed filespec GC'd ──────
    //
    // Two filespecs share one /EmbeddedFile stream.  Removing one attachment
    // must GC its (otherwise-unreferenced) filespec but keep the shared stream
    // and the other filespec intact.  Guards against an over-conservative
    // "pair-or-nothing" regression of the roborev #949 fix.
    #[test]
    fn remove_attachment_with_shared_stream_keeps_stream_and_other_filespec() {
        let mut pdf = open_minimal();

        let fs_a = FileSpecBuilder::new("a.txt", b"shared body")
            .build(&mut pdf)
            .expect("build a");
        insert_embedded_file(&mut pdf, b"a.txt", fs_a).expect("insert a");
        let shared_stream = embedded_file_stream_ref(&mut pdf, fs_a)
            .expect("resolve stream a")
            .expect("stream a exists");

        // Build a second filespec whose /EF points at the SAME stream object.
        let next = next_object_number(&mut pdf);
        let fs_b = ObjectRef::new(next + 1, 0);
        let ef = handle_dictionary(vec![
            (b"/F", pdf.get_object_handle(shared_stream)),
            (b"/UF", pdf.get_object_handle(shared_stream)),
        ]);
        let fs_b_handle = handle_dictionary(vec![
            (b"/Type", ObjectHandle::name(b"Filespec".to_vec())),
            (b"/F", ObjectHandle::string(b"b.txt".to_vec())),
            (b"/UF", ObjectHandle::string(b"b.txt".to_vec())),
            (b"/EF", ef),
        ]);
        set_test_object(&mut pdf, fs_b, fs_b_handle);
        insert_embedded_file(&mut pdf, b"b.txt", fs_b).expect("insert b");

        // Remove attachment "a": its filespec is otherwise unreferenced and
        // must be GC'd; the stream is still used by fs_b and must survive,
        // and fs_b itself must remain intact.
        let removed = remove_attachment(&mut pdf, b"a.txt").expect("remove a");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            !live.contains(&fs_a),
            "removed attachment's filespec must be GC-deleted"
        );
        assert!(
            live.contains(&shared_stream),
            "stream shared with another filespec must be preserved"
        );
        assert!(
            live.contains(&fs_b),
            "the other filespec sharing the stream must remain intact"
        );
    }

    // ── Test: filespec with distinct /EF streams GCs all of them ─────────────
    //
    // Regression for roborev #950-1: only the first /EF stream was resolved,
    // so sibling streams under other /EF keys were orphaned (left live) once
    // the filespec was GC-deleted.
    #[test]
    fn remove_attachment_gcs_all_distinct_ef_streams() {
        let mut pdf = open_minimal();

        let fs_ref = FileSpecBuilder::new("multi.txt", b"primary stream")
            .build(&mut pdf)
            .expect("build filespec");

        // The builder points /EF /F and /EF /UF at one stream; capture it.
        let filespec = resolved_handle(&mut pdf, fs_ref);
        let ef = filespec.get_key(b"/EF");
        assert!(ef.as_dictionary().is_some(), "expected inline /EF dict");
        let stream_f = ef
            .get_key(b"/F")
            .object_ref()
            .expect("expected /EF /F indirect stream");

        // Add a *distinct* second stream object under /EF /UF.
        let next = next_object_number(&mut pdf);
        let stream_uf = ObjectRef::new(next + 1, 0);
        let stream = ObjectHandle::stream(
            handle_dictionary(vec![(
                b"/Type",
                ObjectHandle::name(b"EmbeddedFile".to_vec()),
            )]),
            std::rc::Rc::new(b"sibling stream".to_vec()),
        );
        set_test_object(&mut pdf, stream_uf, stream);
        ef.replace_key(b"/UF", pdf.get_object_handle(stream_uf))
            .expect("replace /EF /UF");
        pdf.mark_object_handle_dirty(&filespec)
            .expect("mark filespec dirty");

        insert_embedded_file(&mut pdf, b"multi.txt", fs_ref).expect("insert");

        let removed = remove_attachment(&mut pdf, b"multi.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(!live.contains(&fs_ref), "filespec must be GC-deleted");
        assert!(
            !live.contains(&stream_f),
            "primary /EF /F stream must be GC-deleted"
        );
        assert!(
            !live.contains(&stream_uf),
            "distinct /EF /UF sibling stream must also be GC-deleted (not orphaned)"
        );
    }

    // ── Test: empty/target-absent indirect /AF array is left untouched ───────
    //
    // Regression for roborev #950-2: an *empty* (or target-absent) indirect
    // /AF array used to be deleted and its parent /AF key removed even though
    // the target ref was never present — dangling the array if it is shared.
    #[test]
    fn remove_attachment_leaves_empty_indirect_af_array_intact() {
        let mut pdf = open_minimal();

        let next = next_object_number(&mut pdf);

        // An empty indirect /AF array object, shared by the catalog *and* a
        // second dictionary so wrongly deleting it would dangle a live ref.
        let af_array_ref = ObjectRef::new(next + 1, 0);
        set_test_object(&mut pdf, af_array_ref, handle_array(Vec::new()));

        let sharer_ref = ObjectRef::new(next + 2, 0);
        let sharer = handle_dictionary(vec![(b"/AF", pdf.get_object_handle(af_array_ref))]);
        set_test_object(&mut pdf, sharer_ref, sharer);

        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/AF", pdf.get_object_handle(af_array_ref))
            .expect("replace catalog /AF");
        catalog
            .replace_key(b"/Sharer", pdf.get_object_handle(sharer_ref))
            .expect("replace catalog /Sharer");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        // Add and remove an unrelated attachment.  Its filespec is NOT in the
        // empty indirect /AF array, so the array and parent key must survive.
        let fs_ref = FileSpecBuilder::new("x.txt", b"x")
            .build(&mut pdf)
            .expect("build");
        insert_embedded_file(&mut pdf, b"x.txt", fs_ref).expect("insert");

        let removed = remove_attachment(&mut pdf, b"x.txt").expect("remove");
        assert!(removed);

        let live = pdf.live_object_refs();
        assert!(
            live.contains(&af_array_ref),
            "empty indirect /AF array (target absent) must NOT be deleted"
        );
        let catalog2 = catalog_handle(&mut pdf);
        assert!(
            catalog2.get_key(b"/AF").object_ref() == Some(af_array_ref),
            "catalog /AF must still point at the untouched indirect array"
        );
    }

    // ── Test: missing key returns false, document unchanged ──────────────────

    #[test]
    fn remove_nonexistent_key_returns_false() {
        let mut pdf = open_minimal();
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("keep.txt");
        std::fs::write(&file, b"keep me").unwrap();
        add_attachment_from_path(&mut pdf, b"keep.txt", &file).expect("add");

        let result = remove_attachment(&mut pdf, b"no-such-key.txt").expect("no error");
        assert!(!result, "must return false for absent key");

        // Document must still contain the original attachment.
        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1, "document must be unchanged");
        assert_eq!(entries[0].0, b"keep.txt");
    }

    // ── Test: /AF on catalog and page is cleared after remove ─────────────────

    #[test]
    fn remove_attachment_preserves_af_and_nulls_filespec_on_catalog_and_page() {
        let mut pdf = open_minimal();

        // Build a filespec manually so we control the ref.
        let fs_ref = FileSpecBuilder::new("af-test.txt", b"payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"af-test.txt", fs_ref).expect("insert");

        // Add /AF to catalog pointing at fs_ref.
        let catalog = catalog_handle(&mut pdf);
        catalog
            .replace_key(b"/AF", handle_array(vec![pdf.get_object_handle(fs_ref)]))
            .expect("replace catalog /AF");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        // Add /AF to the single page as well.
        let page_refs = crate::pages::page_refs(&mut pdf).expect("page_refs");
        assert_eq!(page_refs.len(), 1, "fixture has one page");
        let page_ref = page_refs[0];
        let page = resolved_handle(&mut pdf, page_ref);
        page.replace_key(b"/AF", handle_array(vec![pdf.get_object_handle(fs_ref)]))
            .expect("replace page /AF");
        pdf.mark_object_handle_dirty(&page)
            .expect("mark page dirty");

        // Remove the attachment.
        let removed = remove_attachment(&mut pdf, b"af-test.txt").expect("remove");
        assert!(removed);

        // qpdf leaves the associated-files reference in place; the Filespec
        // object itself is replaced with null by removeEmbeddedFile.
        let catalog2 = catalog_handle(&mut pdf);
        let catalog_af = catalog2.get_key(b"/AF").as_array().expect("catalog /AF");
        assert_eq!(
            catalog_af.len(),
            1,
            "qpdf keeps catalog /AF pointing at the nulled Filespec"
        );
        assert_eq!(catalog_af[0].object_ref(), Some(fs_ref));

        assert!(
            resolved_handle(&mut pdf, fs_ref).is_null(),
            "qpdf replaces the removed Filespec with null"
        );

        // The page's /AF reference is retained too.
        let page_dict2 = resolved_handle(&mut pdf, page_ref);
        let page_af = page_dict2.get_key(b"/AF").as_array().expect("page /AF");
        assert_eq!(
            page_af.len(),
            1,
            "qpdf keeps page /AF pointing at the nulled Filespec"
        );
        assert_eq!(page_af[0].object_ref(), Some(fs_ref));
    }

    // ── Test: shared stream is preserved under conservative GC ───────────────

    #[test]
    fn conservative_gc_preserves_shared_stream() {
        // Build two /Filespec dicts that share the same /EmbeddedFile stream.
        // When one filespec is removed, the shared stream must NOT be GC'd.
        let mut pdf = open_minimal();

        // Allocate the shared EmbeddedFile stream object.
        let next = next_object_number(&mut pdf);
        let stream_ref = ObjectRef::new(next + 1, 0);
        let fs_ref1 = ObjectRef::new(next + 2, 0);
        let fs_ref2 = ObjectRef::new(next + 3, 0);

        // Shared EmbeddedFile stream.
        let ef_dict = handle_dictionary(vec![
            (b"/Type", ObjectHandle::name(b"EmbeddedFile".to_vec())),
            (b"/Length", ObjectHandle::integer(7)),
        ]);
        set_test_object(
            &mut pdf,
            stream_ref,
            ObjectHandle::stream(ef_dict, std::rc::Rc::new(b"payload".to_vec())),
        );

        // /EF sub-dict pointing both filespecs at the same stream.
        let ef_sub = handle_dictionary(vec![
            (b"/F", pdf.get_object_handle(stream_ref)),
            (b"/UF", pdf.get_object_handle(stream_ref)),
        ]);

        // Filespec 1.
        let fs1 = handle_dictionary(vec![
            (b"/Type", ObjectHandle::name(b"Filespec".to_vec())),
            (b"/F", ObjectHandle::string(b"shared1.txt".to_vec())),
            (b"/EF", ef_sub.clone()),
        ]);
        set_test_object(&mut pdf, fs_ref1, fs1);

        // Filespec 2.
        let fs2 = handle_dictionary(vec![
            (b"/Type", ObjectHandle::name(b"Filespec".to_vec())),
            (b"/F", ObjectHandle::string(b"shared2.txt".to_vec())),
            (b"/EF", ef_sub),
        ]);
        set_test_object(&mut pdf, fs_ref2, fs2);

        // Insert both into the name tree.
        insert_embedded_file(&mut pdf, b"shared1.txt", fs_ref1).expect("insert 1");
        insert_embedded_file(&mut pdf, b"shared2.txt", fs_ref2).expect("insert 2");

        // Remove only the first attachment.
        let removed = remove_attachment(&mut pdf, b"shared1.txt").expect("remove");
        assert!(removed);

        // The shared stream must still be alive (fs_ref2 still references it).
        let live = pdf.live_object_refs();
        assert!(
            live.contains(&stream_ref),
            "shared stream must NOT be GC'd while fs_ref2 still references it"
        );

        // fs_ref1 itself should be gone (it is no longer referenced).
        assert!(
            !live.contains(&fs_ref1),
            "removed filespec ref must be GC'd"
        );

        // fs_ref2 must still be alive.
        assert!(
            live.contains(&fs_ref2),
            "surviving filespec ref must remain alive"
        );
    }

    // ── Test: /Names present but /EmbeddedFiles absent → empty ────────────────
    //
    // Covers the `None => Ok(vec![])` branch in both readers when the catalog
    // has a /Names dictionary that simply does not carry an /EmbeddedFiles key.
    #[test]
    fn readers_return_empty_when_names_has_no_embedded_files() {
        let mut pdf = open_minimal();

        // Attach a /Names dict carrying only an unrelated key (no /EmbeddedFiles).
        let names = handle_dictionary(vec![(b"/Dests", handle_dictionary(Vec::new()))]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        assert!(
            list_embedded_files(&mut pdf).expect("list").is_empty(),
            "list_embedded_files must be empty when /EmbeddedFiles is absent"
        );
        assert!(
            pdf.embedded_files()
                .get_embedded_files()
                .expect("handle-valued entries")
                .is_empty(),
            "handle-valued entries must be empty when /EmbeddedFiles is absent"
        );
    }

    #[test]
    fn handle_entries_preserve_direct_filespec_handles() {
        let mut pdf = open_minimal();
        let filespec = handle_dictionary(vec![
            (b"/Type", ObjectHandle::name(b"Filespec".to_vec())),
            (b"/F", ObjectHandle::string(b"direct.txt".to_vec())),
        ]);
        let root = handle_dictionary(vec![(
            b"/Names",
            handle_array(vec![ObjectHandle::string(b"direct".to_vec()), filespec]),
        )]);
        let names = handle_dictionary(vec![(b"/EmbeddedFiles", root)]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        let entries = pdf
            .embedded_files()
            .get_embedded_files()
            .expect("collect direct filespec handle");
        let filespec = entries
            .get(b"direct".as_slice())
            .expect("direct Filespec entry");
        assert!(filespec.as_dictionary().is_some());
        assert_eq!(
            filespec.get_key(b"/F").as_string(),
            Some(b"direct.txt".to_vec())
        );
    }

    // ── Test: empty removal preserves an indirect /Names dict ────────────────
    //
    // When the last /EmbeddedFiles entry is removed and the /Names dict still
    // carries a surviving sibling (here /Dests), the empty-rebuild path rewrites
    // the indirect names dict in place. The qpdf-shaped catalog reaches the
    // terminal directly through its indirect /Names child.
    #[test]
    fn empty_remove_preserves_indirect_names_with_surviving_sibling() {
        let mut pdf = open_minimal();

        // Register an attachment so /Names /EmbeddedFiles holds a real entry.
        let fs_ref = FileSpecBuilder::new("only.txt", b"only payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"only.txt", fs_ref).expect("insert");

        // Materialize qpdf's direct names dictionary as an indirect terminal
        // and add a surviving sibling key.
        let catalog = catalog_handle(&mut pdf);
        let terminal = catalog
            .get_key(b"/Names")
            .shallow_copy()
            .expect("copy direct names");
        // /Dests as a small inline dict: survives the post-removal sweep because
        // it is owned by the (still-reachable) terminal names dict.
        terminal
            .replace_key(
                b"/Dests",
                handle_dictionary(vec![(b"/X", pdf.get_object_handle(fs_ref))]),
            )
            .expect("add /Dests sibling");
        let terminal_ref = ObjectRef::new(next_object_number(&mut pdf) + 1, 0);
        set_test_object(&mut pdf, terminal_ref, terminal);

        catalog
            .replace_key(b"/Names", pdf.get_object_handle(terminal_ref))
            .expect("install indirect Names");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        // Remove the last (only) embedded file → empty rebuild with a surviving
        // /Dests sibling.
        let removed = remove_attachment(&mut pdf, b"only.txt").expect("remove only");
        assert!(removed, "remove_attachment must return true");

        // qpdf modifies the indirect names dictionary in place.
        let catalog_after = catalog_handle(&mut pdf);
        let names_after = catalog_after
            .get_key(b"/Names")
            .object_ref()
            .expect("catalog /Names must still be indirect");
        assert_eq!(names_after, terminal_ref);

        // The terminal retains its sibling and an empty EmbeddedFiles tree.
        let names_dict = resolved_handle(&mut pdf, terminal_ref);
        assert!(
            names_dict.has_key(b"/Dests"),
            "terminal /Names dict must retain the surviving /Dests sibling"
        );
        assert!(names_dict.has_key(b"/EmbeddedFiles"));
    }

    // ── Test: non-empty rebuild with a *direct* (inline) /Names dict ──────────
    //
    // Catalog /Names may be stored inline as a dictionary rather than indirectly.
    // The qpdf path preserves its direct representation while writing the
    // /EmbeddedFiles tree and retaining siblings.
    #[test]
    fn non_empty_rebuild_with_direct_names_dict() {
        let mut pdf = open_minimal();

        // Seed catalog /Names as a *direct* dict carrying only an unrelated key.
        let names = handle_dictionary(vec![(b"/Dests", handle_dictionary(Vec::new()))]);
        replace_catalog_key(&mut pdf, b"/Names", names);

        // Adding an attachment drives the non-empty rebuild over the direct dict.
        let fs_ref = FileSpecBuilder::new("direct.txt", b"direct payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"direct.txt", fs_ref).expect("insert");

        let entries = list_embedded_files(&mut pdf).expect("list");
        assert_eq!(entries.len(), 1, "attachment must be registered");
        assert_eq!(entries[0].0, b"direct.txt");

        // /Names stays direct and retains /Dests.
        let catalog_after = catalog_handle(&mut pdf);
        let names_after = catalog_after
            .get_key(b"/Names")
            .as_dictionary()
            .expect("/Names direct after insert");
        assert!(
            names_after.contains_key(b"/Dests".as_slice()),
            "the inline /Dests sibling must survive the rebuild"
        );
        assert!(
            names_after.contains_key(b"/EmbeddedFiles".as_slice()),
            "/EmbeddedFiles must be written into the /Names dict"
        );
    }

    // ── Test: empty rebuild with a *direct* (inline) /Names dict + sibling ────
    //
    // When /Names is an inline dict that directly holds both /EmbeddedFiles and a
    // surviving sibling (/Dests), removing the last attachment must drop
    // /EmbeddedFiles and rewrite the catalog with the trimmed inline dict.
    #[test]
    fn empty_rebuild_with_direct_names_dict_and_sibling() {
        let mut pdf = open_minimal();

        // First build a real /EmbeddedFiles tree via the helper.
        let fs_ref = FileSpecBuilder::new("only2.txt", b"only2 payload")
            .build(&mut pdf)
            .expect("build filespec");
        insert_embedded_file(&mut pdf, b"only2.txt", fs_ref).expect("insert");

        // Inline that names dict directly into the catalog (and add /Dests), so
        // the catalog reaches /EmbeddedFiles through a *direct* /Names dict.
        let catalog = catalog_handle(&mut pdf);
        let names = catalog
            .get_key(b"/Names")
            .shallow_copy()
            .expect("copy direct Names");
        names
            .replace_key(b"/Dests", handle_dictionary(Vec::new()))
            .expect("add /Dests sibling");
        catalog
            .replace_key(b"/Names", names)
            .expect("install direct Names");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");

        // Remove the only attachment → empty rebuild over the direct /Names dict.
        let removed = remove_attachment(&mut pdf, b"only2.txt").expect("remove only2");
        assert!(removed, "remove_attachment must return true");

        // /Names stays inline on the catalog; /Dests and the empty
        // /EmbeddedFiles tree are preserved.
        let catalog_after = catalog_handle(&mut pdf);
        let names_after = catalog_after
            .get_key(b"/Names")
            .as_dictionary()
            .expect("/Names must remain a direct dict");
        assert!(
            names_after.contains_key(b"/Dests".as_slice()),
            "the /Dests sibling must survive the empty rebuild"
        );
        assert!(names_after.contains_key(b"/EmbeddedFiles".as_slice()));
    }
}
