//! qpdf correspondence: QPDFAcroFormDocumentHelper.cc responsibilities shared with overlay and signature modules.
//! High-level AcroForm document helper.
//!
//! [`AcroFormDocumentHelper`] wraps a `&mut Pdf<R>` and exposes document-level
//! operations for interactive form fields. It builds on
//! [`crate::FormFieldObjectHelper`] for inherited value lookup. qpdf's
//! page-based AcroForm copy responsibilities use the canonical annotation
//! transform path in this module and its job consumers.

use crate::form_field_object_helper::FormFieldObjectHelper;
use crate::object::MAX_INLINE_DEPTH;
use crate::object_handle::{ObjectHandle, ObjectHandleIdentity, ResourceConflicts};
use crate::page_object_helper::PageObjectHelper;
use crate::pdf_string::utf8_value;
use crate::resource_replacer::{replace_resource_names, ResourceRenames};
use crate::{
    Dictionary, Error, Matrix, Object, ObjectRef, Pdf, Rectangle, Result,
    DEFAULT_MAX_ACROFORM_DEPTH,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Seek};
use std::rc::Rc;

fn record_association(cache: &mut AcroFormCache, annotation: ObjectHandle, field: ObjectHandle) {
    let annotation_identity = annotation.identity_key();
    cache
        .annotation_handles
        .entry(annotation_identity.clone())
        .or_insert_with(|| annotation.clone());
    cache
        .annotation_to_field
        .insert(annotation_identity, field.clone());

    let field_identity = field.identity_key();
    cache
        .field_handles
        .entry(field_identity.clone())
        .or_insert_with(|| field.clone());
    let annotations = cache
        .field_to_annotations
        .entry(field_identity)
        .or_default();
    if !annotations
        .iter()
        .any(|candidate| candidate.is_same_object_as(&annotation))
    {
        annotations.push(annotation);
    }
}

// qpdf's traverseField (QPDFAcroFormDocumentHelper.cc:349-361) never drops an
// emptied name_to_fields bucket on rename -- only removeFormFields does that
// cleanup. Pruning it here anyway is required, not cosmetic: the collision
// check in add_and_rename_form_fields tests existing_names via key presence
// (name_to_fields.keys()), not qpdf's emptiness check
// (!getFieldsWithQualifiedName(name).empty(), :89). A stale empty key left
// behind by the literal qpdf behavior would poison that presence check and
// rename fields qpdf would leave alone. Do not remove this pruning to chase
// closer C++ literalism.
fn record_field_name(cache: &mut AcroFormCache, field: ObjectHandle, name: String) {
    let identity = field.identity_key();
    if let Some(old_name) = cache.field_to_name.insert(identity, name.clone()) {
        if old_name != name {
            let mut remove_name = false;
            if let Some(fields) = cache.name_to_fields.get_mut(&old_name) {
                fields.retain(|candidate| !candidate.is_same_object_as(&field));
                remove_name = fields.is_empty();
            } // cov:ignore: structural close of the qpdf old-name entry branch
            if remove_name {
                cache.name_to_fields.remove(&old_name);
            }
        } // cov:ignore: structural close of the qpdf old-name change branch
    }

    let fields = cache.name_to_fields.entry(name).or_default();
    if !fields
        .iter()
        .any(|candidate| candidate.is_same_object_as(&field))
    {
        fields.push(field);
    }
}

/// Effective metadata for one AcroForm field-tree node.
///
/// Values are retained as live handles from the current node plus inherited
/// field-tree state. `/DA`, `/Q`, and `/MaxLen` may inherit from `/AcroForm`
/// defaults.
#[derive(Debug, Clone)]
pub struct AcroFormFieldInfo {
    /// The field dictionary object.
    pub object_ref: ObjectRef,
    /// Direct `/T` partial name bytes, when present.
    pub partial_name: Option<Vec<u8>>,
    /// Dot-joined field name path reconstructed from ancestor `/T` entries.
    pub full_name: String,
    /// Effective `/FT` field type.
    pub field_type: Option<Vec<u8>>,
    /// Effective `/V` field value.
    pub value: Option<ObjectHandle>,
    /// Effective `/DV` default field value.
    pub default_value: Option<ObjectHandle>,
    /// Effective `/Ff` field flags.
    pub field_flags: Option<i64>,
    /// Effective `/DA` default appearance.
    pub default_appearance: Option<ObjectHandle>,
    /// Effective `/Q` quadding value.
    pub quadding: Option<i64>,
    /// Effective `/MaxLen` text-field maximum length.
    pub max_len: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct FieldInheritance {
    full_name: String,
    field_type: Option<Vec<u8>>,
    value: Option<ObjectHandle>,
    default_value: Option<ObjectHandle>,
    field_flags: Option<i64>,
    default_appearance: Option<ObjectHandle>,
    quadding: Option<i64>,
    max_len: Option<i64>,
}

/// The live association cache built by qpdf's
/// `QPDFAcroFormDocumentHelper::analyze`.
///
/// The maps are keyed by [`ObjectHandleIdentity`] rather than by a
/// materialized [`Object`] or by [`ObjectRef`]. This preserves qpdf's shared
/// object identity for both indirect fields and direct page annotations. The
/// handle maps retain the canonical values needed to project the cache to
/// legacy ref-valued APIs.
#[derive(Default)]
pub(crate) struct AcroFormCache {
    annotation_to_field: HashMap<ObjectHandleIdentity, ObjectHandle>,
    annotation_handles: HashMap<ObjectHandleIdentity, ObjectHandle>,
    field_to_annotations: HashMap<ObjectHandleIdentity, Vec<ObjectHandle>>,
    field_handles: HashMap<ObjectHandleIdentity, ObjectHandle>,
    field_to_name: HashMap<ObjectHandleIdentity, String>,
    name_to_fields: BTreeMap<String, Vec<ObjectHandle>>,
}

/// The objects produced by qpdf's
/// `QPDFAcroFormDocumentHelper::transformAnnotations`
/// (`libqpdf/QPDFAcroFormDocumentHelper.cc:699-1014`).
///
/// `old_fields` identifies source top-level fields that must be removed before
/// the transformed copies are installed. `new_fields` contains the copied
/// top-level fields that the caller adds to the destination AcroForm. The
/// annotation vector is kept separate because qpdf installs it on the page
/// independently of the field-tree update.
#[allow(dead_code)] // consumed by the follow-up PageObjectHelper facade slice
#[derive(Debug, Default)]
pub(crate) struct AnnotationTransformResult {
    pub(crate) new_annotations: Vec<ObjectHandle>,
    pub(crate) new_fields: Vec<ObjectHandle>,
    pub(crate) old_fields: BTreeSet<ObjectRef>,
}

#[derive(Clone, Debug, Default)]
struct AcroFormDefaults {
    default_appearance: Vec<u8>,
    quadding: i64,
    resources: Option<ObjectHandle>,
}

#[derive(Clone, Debug)]
struct InheritedFieldOverrides {
    override_da: bool,
    source_default_da: Vec<u8>,
    override_q: bool,
    source_default_q: i64,
}

struct ForeignResourcePlan {
    destination_resources: ObjectHandle,
    renames: ResourceRenames,
}

/// High-level helper for a document's `/AcroForm`.
///
/// Construct with [`AcroFormDocumentHelper::new`] or [`Pdf::acroform`]. The
/// helper eagerly analyzes once per source `Pdf`, matching qpdf's
/// `QPDFJob::get_afdh_for_qpdf` memoization (`QPDFJob.cc:1847-1856`), and
/// lazily reuses that association cache on later facades. The cache retains
/// live [`ObjectHandle`] identities, so association consumers do not fall
/// back to stale materialized objects. Call [`Self::invalidate_cache`] after
/// manually changing the field tree, AcroForm dictionary, or page
/// annotations, matching qpdf's cache contract.
///
/// For a runnable walkthrough see `examples/list_form_fields.rs`.
pub struct AcroFormDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
    cache: Rc<RefCell<Option<AcroFormCache>>>,
}

impl<'a, R: Read + Seek> AcroFormDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably and eagerly build qpdf's
    /// annotation/field cache.
    ///
    /// qpdf's `QPDFAcroFormDocumentHelper` constructor calls `analyze()`
    /// before returning (`QPDFAcroFormDocumentHelper.cc:14-21`) so callers
    /// observe a stable snapshot until [`Self::invalidate_cache`] is called.
    /// Rust exposes the same fallible boundary as `Result` because lazy
    /// resolver and page-walk failures cannot be represented by an infallible
    /// constructor without a compatibility sentinel or panic.
    pub fn new(pdf: &'a mut Pdf<R>) -> Result<Self> {
        let cache = Rc::clone(&pdf.acroform_cache);
        let mut helper = Self { pdf, cache };
        helper.analyze()?;
        Ok(helper)
    }

    /// Create a helper whose cache contains only the `/AcroForm` field tree.
    ///
    /// The page-selection merge uses this for its transient, freshly copied
    /// target while it is still replaying repeated and foreign page
    /// annotations. qpdf constructs its source-document helpers before those
    /// copy events (`QPDFJob.cc:2517-2584`), so running the orphan-widget page
    /// scan against that incomplete target would report a warning for a
    /// repeated annotation that qpdf has not associated yet. This is an
    /// internal construction boundary, not an alternate public compatibility
    /// route; [`Self::new`] remains the qpdf `analyze()` constructor for
    /// complete documents.
    pub(crate) fn new_for_field_tree(pdf: &'a mut Pdf<R>) -> Result<Self> {
        let cache = Rc::new(RefCell::new(None));
        let mut helper = Self { pdf, cache };
        *helper.cache.borrow_mut() = Some(helper.analyze_field_tree()?.unwrap_or_default());
        Ok(helper)
    }

    /// Invalidate the cached field/annotation associations.
    ///
    /// This mirrors `QPDFAcroFormDocumentHelper::invalidateCache`
    /// (`include/qpdf/QPDFAcroFormDocumentHelper.hh:72-78`). It is required
    /// after external mutation of `/AcroForm`, the field tree, or page
    /// annotations when the mutation can change their association.
    pub fn invalidate_cache(&mut self) {
        *self.cache.borrow_mut() = None;
    }

    /// Return all field-tree object refs in preorder.
    ///
    /// Missing `/AcroForm` or missing/malformed `/Fields` returns an empty list.
    /// Cycles are ignored after the first visit.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the catalog or a field-tree node is not a
    ///   dictionary, when an indirect `/AcroForm` reference does not resolve to a
    ///   dictionary, or when the field-tree depth limit is exceeded. A direct
    ///   non-dictionary `/AcroForm` value is ignored, not rejected.
    /// - Any error from [`Pdf::resolve`].
    pub fn fields(&mut self) -> Result<Vec<ObjectRef>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.try_get_key(b"/Fields")?)? else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for item in fields {
            if let Some(field_ref) = item.object_ref().or_else(|| item.as_reference()) {
                self.walk_field_tree(field_ref, &mut seen, &mut out)?;
            }
        }
        Ok(out)
    }

    /// Return all field-tree nodes with effective inherited metadata.
    ///
    /// Missing `/AcroForm` or missing/malformed `/Fields` returns an empty
    /// list. Cycles are ignored after the first visit.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when the catalog or a field-tree node is not a
    ///   dictionary, when an indirect `/AcroForm` reference does not resolve to a
    ///   dictionary, or when the field-tree depth limit is exceeded. A direct
    ///   non-dictionary `/AcroForm` value is ignored, not rejected.
    /// - Any error from [`Pdf::resolve`].
    pub fn field_infos(&mut self) -> Result<Vec<AcroFormFieldInfo>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.try_get_key(b"/Fields")?)? else {
            return Ok(Vec::new());
        };

        let default_appearance = deref_leaf_handle(self.pdf, acroform.try_get_key(b"/DA")?)?;
        let quadding = inherited_integer(self.pdf, &acroform, b"/Q")?;
        let max_len = inherited_integer(self.pdf, &acroform, b"/MaxLen")?;
        let inherited = FieldInheritance {
            default_appearance,
            quadding,
            max_len,
            ..FieldInheritance::default()
        };

        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for item in fields {
            if let Some(field_ref) = item.object_ref().or_else(|| item.as_reference()) {
                self.walk_field_info_tree(field_ref, inherited.clone(), &mut seen, &mut out, 0)?;
            }
        }
        Ok(out)
    }

    /// Build the qpdf `analyze()` annotation→field map: for every widget
    /// annotation reachable from `/AcroForm/Fields` (recursively via
    /// `/Kids`), the field dictionary that owns it, plus every widget
    /// annotation on any page not reachable that way, self-mapped as its own
    /// field (the "orphan widget" fallback).
    ///
    /// Mirrors `QPDFAcroFormDocumentHelper::analyze`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:235-286`), specifically the
    /// `annotation_to_field` half of its cache. The same analysis also fills
    /// qpdf's reverse annotation and qualified-name indexes for later
    /// consumers.
    ///
    /// qpdf caches this on the helper instance (`Members::cache_valid`) so
    /// repeated per-widget lookups are O(1) amortized. This method is the
    /// ref-valued projection for existing callers; crate consumers use the
    /// canonical handle cache directly.
    ///
    /// Returns an empty map when the catalog `/AcroForm` is absent, is not a
    /// dictionary, or carries no `/Fields` key — in which case qpdf's
    /// `analyze` returns before its page `/Annots` orphan-widget pass, so
    /// that pass is skipped here too. A present-but-non-array `/Fields` is
    /// treated as empty (qpdf warns and uses an empty array), but the orphan
    /// pass below still runs in that case.
    ///
    /// # Errors
    ///
    /// - Any error while resolving the live object graph or enumerating pages.
    pub fn annotation_to_field_map(&mut self) -> Result<BTreeMap<ObjectRef, ObjectRef>> {
        Ok(self
            .canonical_annotation_to_field_handles()?
            .into_iter()
            .filter_map(|(annotation, field)| Some((annotation.object_ref()?, field.object_ref()?)))
            .collect())
    }

    /// Return the field that owns `annot_ref`, mirroring
    /// `QPDFAcroFormDocumentHelper::getFieldForAnnotation`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:218-232`).
    ///
    /// Returns `Ok(None)` immediately, without building
    /// [`Self::annotation_to_field_map`], when `annot_ref` does not resolve
    /// to a dictionary with `/Subtype /Widget`
    /// (`QPDFObjectHandle::isDictionaryOfType("", "/Widget")`).
    ///
    /// For repeated per-widget lookups (e.g. enumerating every widget on a
    /// page or in a document), prefer building
    /// [`Self::annotation_to_field_map`] once — see its doc for why a
    /// per-widget call to this method would be O(n²) instead of qpdf's
    /// cached O(1) amortized.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::annotation_to_field_map`]'s errors.
    pub fn get_field_for_annotation(&mut self, annot_ref: ObjectRef) -> Result<Option<ObjectRef>> {
        let annotation = self.pdf.get_object_handle(annot_ref);
        Ok(self
            .canonical_field_for_annotation(annotation)?
            .and_then(|field| field.object_ref()))
    }

    /// Return the Widget annotations listed by a page, preserving their live
    /// handles for qpdf-shaped consumers.
    ///
    /// This is the handle-native counterpart of
    /// `QPDFAcroFormDocumentHelper::getWidgetAnnotationsForPage`, which is a
    /// thin delegation to `QPDFPageObjectHelper::getAnnotations("/Widget")`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:197-201`).
    pub(crate) fn get_widget_annotations_for_page(
        &mut self,
        page_ref: ObjectRef,
    ) -> Result<Vec<ObjectHandle>> {
        let mut page = PageObjectHelper::new(page_ref, self.pdf);
        page.get_annotation_handles(Some(b"/Widget"))
    }

    /// Return the live field handle associated with a Widget annotation.
    ///
    /// A missing association returns qpdf's null helper, matching
    /// `QPDFAcroFormDocumentHelper::getFieldForAnnotation` for a Widget that
    /// cannot be associated with a field. The analysis itself supplies qpdf's
    /// orphan-Widget self-association when possible.
    pub(crate) fn get_field_for_annotation_handle(
        &mut self,
        annotation: ObjectHandle,
    ) -> Result<ObjectHandle> {
        Ok(self
            .canonical_field_for_annotation(annotation)?
            .unwrap_or_else(ObjectHandle::null))
    }

    /// Build the qpdf `analyze()` associations while retaining live canonical
    /// [`ObjectHandle`] identity for both indirect and direct annotations.
    ///
    /// The public ref-valued [`Self::annotation_to_field_map`] method is kept
    /// for existing callers, but it is now a projection of this route. This
    /// is important for qpdf parity: `QPDFObjectHandle::replaceKey` mutates a
    /// shared object allocation, so a later helper lookup must not consult a
    /// stale `Object` materialization of the same object.
    pub(crate) fn canonical_annotation_to_field_handles(
        &mut self,
    ) -> Result<Vec<(ObjectHandle, ObjectHandle)>> {
        self.analyze()?;
        let cache = self.cache.borrow();
        let cache = cache
            .as_ref()
            .expect("analyze always installs an AcroForm cache");
        let mut associations: Vec<_> = cache
            .annotation_to_field
            .iter()
            .filter_map(|(identity, field)| {
                cache
                    .annotation_handles
                    .get(identity)
                    .map(|annotation| (annotation.clone(), field.clone()))
            })
            .collect();
        associations.sort_by_key(|(annotation, _)| annotation.object_ref());
        Ok(associations)
    }

    fn analyze(&mut self) -> Result<()> {
        if self.cache.borrow().is_some() {
            return Ok(());
        }

        let Some(mut cache) = self.analyze_field_tree()? else {
            // qpdf returns before both field traversal and the orphan-widget
            // fallback when there is no dictionary `/AcroForm` or no
            // `/Fields` key at all.
            *self.cache.borrow_mut() = Some(AcroFormCache::default());
            return Ok(());
        };
        // qpdf's orphan-widget fallback walks the canonical page annotation
        // route and associates an otherwise-unreachable widget with itself.
        // QPDF::getAllPages returns an empty vector when the catalog has no
        // `/Pages` entry, so do not route that malformed-but-readable shape
        // through flpdf's stricter public `page_refs` missing-key error.
        let pages = self
            .pdf
            .root_ref()
            .map(|root_ref| self.pdf.get_object_handle(root_ref))
            .map(|root| root.try_get_key(b"/Pages"))
            .transpose()?;
        if let Some(pages) = pages {
            // `QPDF::getAllPages` only enters its page walk when `/Pages`
            // has `/Kids`; `try_has_key` also preserves qpdf's type warning
            // and empty-result behavior for a non-dictionary `/Pages` value.
            if pages.try_has_key(b"/Kids")? {
                // `crate::pages::page_refs` requires `/Pages` to be an
                // indirect reference (`PageWalk::with_max_depth`); qpdf's own
                // `getAllPages` has no such requirement. A malformed-but-
                // readable catalog that embeds `/Pages` directly must not
                // fail this eager `analyze()` (and so take down unrelated
                // AcroForm operations like `fields`/`has_acro_form`) just
                // because the stricter public helper can't walk it -- treat
                // it the same as the "no `/Pages`" case above and skip the
                // orphan-widget fallback.
                let page_refs = match crate::pages::page_refs(self.pdf) {
                    Ok(page_refs) => page_refs,
                    Err(Error::Missing("/Pages")) => Vec::new(),
                    Err(err) => return Err(err),
                };
                for page_ref in page_refs {
                    let widgets = {
                        let mut page = PageObjectHelper::new(page_ref, self.pdf);
                        page.get_annotation_handles(Some(b"/Widget"))?
                    };
                    for annotation in widgets {
                        let annotation = self.pdf.resolve_to_terminal(&annotation)?;
                        let identity = annotation.identity_key();
                        if !cache.annotation_to_field.contains_key(&identity) {
                            annotation.warn_if_possible(
                                "this widget annotation is not reachable from /AcroForm in the document catalog",
                            )?;
                            record_association(&mut cache, annotation.clone(), annotation);
                        }
                    }
                }
            }
        }

        *self.cache.borrow_mut() = Some(cache);
        Ok(())
    }

    /// Build the field/annotation associations reachable from `/Fields`.
    ///
    /// The page-based orphan pass is deliberately kept in [`Self::analyze`]
    /// because it belongs to qpdf's complete-document constructor, while the
    /// page-selection merge needs this field-tree portion before its copied
    /// pages have reached their final association state.
    fn analyze_field_tree(&mut self) -> Result<Option<AcroFormCache>> {
        let mut cache = AcroFormCache::default();
        let Some(acroform) = self.canonical_acroform()? else {
            return Ok(None);
        };
        // Mirrors qpdf's combined `acroform.isDictionary() && acroform.hasKey("/Fields")`
        // guard (`QPDFAcroFormDocumentHelper.cc:241-243`): an `/AcroForm` dictionary
        // without a `/Fields` key skips both the field traversal and the
        // orphan-widget fallback below, not just the traversal.
        if !acroform.try_has_key(b"/Fields")? {
            return Ok(None);
        }

        let fields = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/Fields")?)?;
        if let Some(fields) = fields.as_array() {
            let mut visited = BTreeSet::new();
            for field in fields {
                self.traverse_field_handles(field, None, 0, &mut visited, &mut cache)?;
            }
        }
        Ok(Some(cache))
    }

    /// Return the distinct live handles represented by qpdf's
    /// `field_to_annotations` map, which is the source of
    /// `QPDFAcroFormDocumentHelper::getFormFields`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:163-174`).
    ///
    /// The ref-valued public map is only a projection for legacy callers. A
    /// mutating consumer must retain these canonical handles so a later edit
    /// cannot fall back to a stale materialized [`Object`].
    pub(crate) fn form_field_handles(&mut self) -> Result<BTreeMap<ObjectRef, ObjectHandle>> {
        self.analyze()?;
        let cache = self.cache.borrow();
        let cache = cache
            .as_ref()
            .expect("analyze always installs an AcroForm cache");
        let mut fields = BTreeMap::new();
        for identity in cache.field_to_annotations.keys() {
            if let Some(field) = cache.field_handles.get(identity) {
                let Some(field_ref) = field.object_ref() else {
                    continue;
                };
                fields.entry(field_ref).or_insert_with(|| field.clone());
            }
        }
        Ok(fields)
    }

    /// Return the field references with the given fully qualified name.
    ///
    /// This mirrors `QPDFAcroFormDocumentHelper::getFieldsWithQualifiedName`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:173-183`). The result is
    /// derived from the live-handle cache, so a name changed through
    /// [`Self::set_form_field_name`] is reflected without rebuilding the
    /// document graph.
    pub fn get_fields_with_qualified_name(&mut self, name: &str) -> Result<BTreeSet<ObjectRef>> {
        self.analyze()?;
        let cache = self.cache.borrow();
        Ok(cache
            .as_ref()
            .expect("analyze always installs an AcroForm cache")
            .name_to_fields
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(ObjectHandle::object_ref)
            .collect())
    }

    /// Set a field's partial name and update the warm qualified-name cache.
    ///
    /// This mirrors `QPDFAcroFormDocumentHelper::setFormFieldName`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:153-160`). The supplied name
    /// is encoded as a PDF Unicode string by
    /// [`FormFieldObjectHelper::set_field_attribute_string`].
    pub fn set_form_field_name(&mut self, field_ref: ObjectRef, name: &str) -> Result<()> {
        self.analyze()?;
        let field = self.pdf.get_object_handle(field_ref);
        {
            let mut field_helper =
                FormFieldObjectHelper::from_object_handle(field.clone(), self.pdf);
            field_helper.set_field_attribute_string(b"/T", name)?;
        }
        self.update_cached_field(field)
    }

    fn remove_cached_fields(&mut self, to_remove: &BTreeSet<ObjectRef>) {
        let mut cache_store = self.cache.borrow_mut();
        let Some(cache) = cache_store.as_mut() else {
            return;
        };

        let mut removed = Vec::<ObjectHandleIdentity>::new();
        for field in cache
            .field_handles
            .values()
            .chain(cache.name_to_fields.values().flatten())
        {
            if field
                .object_ref()
                .is_some_and(|field_ref| to_remove.contains(&field_ref))
            {
                let identity = field.identity_key();
                if !removed.iter().any(|candidate| candidate == &identity) {
                    removed.push(identity);
                }
            }
        }
        if removed.is_empty() {
            return;
        }

        // qpdf's removeFormFields walks the forward association list, not the
        // reverse map. The forward list can be stale after a warm-cache
        // reassociation, and that stale entry is intentionally observable in
        // the cache cleanup semantics (QPDFAcroFormDocumentHelper.cc:124-131).
        let mut annotation_ids = Vec::new();
        for field in &removed {
            if let Some(annotations) = cache.field_to_annotations.remove(field) {
                annotation_ids.extend(
                    annotations
                        .into_iter()
                        .map(|annotation| annotation.identity_key()),
                );
            } // cov:ignore: successful association branch closing brace has no llvm-cov region
        }
        for annotation in annotation_ids {
            cache.annotation_to_field.remove(&annotation);
            cache.annotation_handles.remove(&annotation);
        }
        cache
            .field_handles
            .retain(|field, _| !removed.contains(field));
        cache
            .field_to_name
            .retain(|field, _| !removed.contains(field));
        cache.name_to_fields.retain(|_, fields| {
            fields.retain(|field| !removed.contains(&field.identity_key()));
            !fields.is_empty()
        });
    }

    /// Remove selected top-level fields from the live `/AcroForm /Fields`
    /// array, mirroring qpdf's `removeFormFields`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:112-151`).
    ///
    /// The array handle is mutated in place. This preserves an indirect
    /// `/Fields` holder and lets owner-aware dirty marking handle a direct
    /// array nested in an indirect `/AcroForm` object.
    pub(crate) fn remove_form_fields(&mut self, to_remove: &BTreeSet<ObjectRef>) -> Result<bool> {
        let Some(acroform) = self.canonical_acroform()? else {
            return Ok(false);
        };
        let fields = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/Fields")?)?;
        let Some(items) = fields.as_array() else {
            return Ok(false);
        };

        let indexes: Vec<usize> = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item.object_ref()
                    .filter(|field_ref| to_remove.contains(field_ref))
                    .map(|_| index)
            })
            .collect();
        self.remove_cached_fields(to_remove);
        if indexes.is_empty() {
            return Ok(false);
        }

        for index in indexes.iter().rev().copied() {
            fields.erase_array_item(index)?;
        }
        self.pdf.mark_object_handle_dirty(&fields)?;
        Ok(true)
    }

    /// Copy annotations into fresh indirect objects and transform their
    /// rectangles, mirroring qpdf's same-document annotation route.
    ///
    /// This slice covers same-document field-tree and appearance-stream
    /// copying. Foreign-document defaults, `/DR` resource remapping, and the
    /// public page-level facade remain in subsequent `flpdf-2tfv` slices; no
    /// page-level caller switches from the legacy bridge until those slices
    /// are complete.
    #[allow(dead_code, clippy::mutable_key_type)] // caller cutover lands with the A3 facade
    pub(crate) fn transform_annotations(
        &mut self,
        old_annots: ObjectHandle,
        cm: Matrix,
    ) -> Result<AnnotationTransformResult> {
        let mut transformed = AnnotationTransformResult::default();
        let mut orig_to_copy = HashMap::<ObjectHandleIdentity, ObjectHandle>::new();
        let mut copied_field_trees = HashSet::<ObjectHandleIdentity>::new();
        let mut added_new_fields = BTreeSet::new();
        let Some(annotations) = old_annots.try_as_array()? else {
            return Ok(transformed);
        };

        for annotation in annotations {
            let annotation = self.pdf.resolve_to_terminal(&annotation)?;
            if annotation.as_stream_dict().is_some() {
                annotation.warn_if_possible("ignoring annotation that's a stream")?;
                continue;
            }

            if let Some(field) = self.canonical_field_for_annotation(annotation.clone())? {
                let top_field = self.canonical_top_level_field(field.clone())?;
                if let Some(top_ref) = top_field.object_ref() {
                    transformed.old_fields.insert(top_ref);
                }
                if copied_field_trees.insert(top_field.identity_key()) {
                    let copied_top = self.copy_field_tree(&top_field, &mut orig_to_copy)?;
                    if let Some(copied_ref) = copied_top.object_ref() {
                        if added_new_fields.insert(copied_ref) {
                            transformed.new_fields.push(copied_top);
                        }
                        // cov:ignore-start: inner-if closing braces are llvm-cov region artifacts; insertion path is exercised above.
                    }
                }
                // cov:ignore-end
                // The field walk normally copied this already. This lookup
                // also covers a merged field whose widget is the top node.
                let _ = self.copy_transform_object(&field, &mut orig_to_copy)?;
            }

            let copied = self
                .copy_transform_object(&annotation, &mut orig_to_copy)?
                .expect("stream annotations are filtered before copying");
            copy_and_transform_appearance_streams(self.pdf, &copied, cm)?;
            let rect = transformed_annotation_rectangle(self.pdf, &copied, cm)?;
            copied.replace_key(b"/Rect", rect)?;
            self.pdf.mark_object_handle_dirty(&copied)?;
            transformed.new_annotations.push(copied);
        }

        Ok(transformed)
    }

    /// Copy annotations from a different PDF, then apply the same per-place
    /// duplication and geometry transform as [`Self::transform_annotations`].
    ///
    /// `copy_foreign_object` owns the source-to-target graph copy and keeps
    /// source identity stable across the field-tree and annotation work in
    /// the same per-annotation transform loop.
    /// Source/destination `/DA` and `/Q` defaults are reconciled by pinning
    /// the source value onto a copied field that has no explicit value of
    /// its own, matching qpdf's `adjustInheritedFields`. This slice also
    /// reconciles foreign field `/DA`, `/DR`, and destination `/AcroForm/DR`
    /// resource-name conflicts; appearance-stream resource privatization
    /// remains a subsequent slice.
    #[allow(dead_code, clippy::mutable_key_type)] // caller cutover lands with the A3 facade
    pub(crate) fn transform_annotations_from<RS: Read + Seek>(
        &mut self,
        old_annots: ObjectHandle,
        cm: Matrix,
        source: &mut Pdf<RS>,
    ) -> Result<AnnotationTransformResult> {
        let mut source_helper = AcroFormDocumentHelper::new(source)?;
        self.transform_annotations_from_with_source_helper(old_annots, cm, &mut source_helper)
    }

    #[allow(clippy::mutable_key_type)]
    fn transform_annotations_from_with_source_helper<RS: Read + Seek>(
        &mut self,
        old_annots: ObjectHandle,
        cm: Matrix,
        source_helper: &mut AcroFormDocumentHelper<'_, RS>,
    ) -> Result<AnnotationTransformResult> {
        let mut transformed = AnnotationTransformResult::default();
        let source_defaults = source_helper.canonical_acroform_defaults()?;
        let old_annots = source_helper.pdf.resolve_to_terminal(&old_annots)?;
        let Some(annotations) = old_annots.try_as_array()? else {
            return Ok(transformed);
        };
        // Snapshot only the annotation array handles. qpdf resolves the
        // field association and starts copying each annotation in one loop;
        // do not pre-survey all top fields in a separate pass.
        let annotations = annotations.to_vec();
        let target_defaults = self.canonical_acroform_defaults()?;
        let inherited_overrides = InheritedFieldOverrides {
            override_da: source_defaults.default_appearance != target_defaults.default_appearance,
            source_default_da: source_defaults.default_appearance,
            override_q: source_defaults.quadding != target_defaults.quadding,
            source_default_q: source_defaults.quadding,
        };
        // qpdf copies the source `/DR` unconditionally up front, before the
        // annotation loop, whenever the source is foreign
        // (`QPDFAcroFormDocumentHelper.cc:729-737`); only the *merge* into
        // the destination `/AcroForm`/`/DR` (`init_dr_map`,
        // `QPDFAcroFormDocumentHelper.cc:772-800`) is lazy, deferred until
        // the first field is actually copied, so an annotation-only
        // (no-field) transform never creates a destination `/AcroForm`.
        // flpdf defers the whole resource plan -- including the `/DR` copy
        // -- to that same first-field point instead of copying `/DR` eagerly
        // like qpdf does; this is an object-allocation-order divergence
        // (qpdf allocates the copied `/DR` before any field, flpdf after),
        // not a byte-identical one: object numbers are reassigned by the
        // writer's own BFS-from-root traversal (`QPDFWriter.cc:1097-1119`),
        // not by allocation order, so this timing difference does not change
        // written output.
        let mut foreign_resources = None;

        let mut orig_to_copy = HashMap::<ObjectHandleIdentity, ObjectHandle>::new();
        let mut copied_field_trees = HashSet::<ObjectHandleIdentity>::new();
        let mut added_new_fields = BTreeSet::new();
        for annotation in annotations {
            let source_annotation = source_helper.pdf.resolve_to_terminal(&annotation)?;
            if source_annotation.as_stream_dict().is_some() {
                source_annotation.warn_if_possible("ignoring annotation that's a stream")?;
                continue;
            }
            let source_top_field = source_helper
                .canonical_field_for_annotation(source_annotation.clone())?
                .map(|field| source_helper.canonical_top_level_field(field))
                .transpose()?;
            let source_annotation = ensure_foreign_indirect(source_helper.pdf, source_annotation)?;

            if let Some(source_top_field) = source_top_field {
                let source_top_field =
                    ensure_foreign_indirect(source_helper.pdf, source_top_field)?;
                let copied_source_top = self.pdf.copy_foreign_object(&source_top_field)?;
                if copied_field_trees.insert(copied_source_top.identity_key()) {
                    if foreign_resources.is_none() {
                        foreign_resources = Some(self.prepare_foreign_resource_plan(
                            source_defaults.resources.clone(),
                            source_helper.pdf,
                        )?); // cov:ignore: LLVM maps this multiline resource-plan call to a defensive continuation edge
                    }
                    let copied_top = self.copy_field_tree_with_overrides(
                        &copied_source_top,
                        &mut orig_to_copy,
                        Some(&inherited_overrides),
                        foreign_resources.as_ref(),
                    )?; // cov:ignore: LLVM maps this multiline field-tree call to a defensive continuation edge
                    if let Some(copied_ref) = copied_top.object_ref() {
                        if added_new_fields.insert(copied_ref) {
                            transformed.new_fields.push(copied_top);
                        }
                        // cov:ignore-start: inner-if closing braces are llvm-cov region artifacts; insertion path is exercised above.
                    }
                }
                // cov:ignore-end
            }

            // qpdf copies the foreign annotation only after the field tree has
            // been copied (`QPDFAcroFormDocumentHelper.cc:955-963`). Keeping
            // this order is observable in QDF object numbering and also lets
            // the field/annotation identity map reuse the copied widget.
            let copied_source_annotation = self.pdf.copy_foreign_object(&source_annotation)?;
            let copied = self
                .copy_transform_object(&copied_source_annotation, &mut orig_to_copy)?
                .expect("stream annotations are filtered before copying");
            let appearance_renames = foreign_resources.as_ref().map(|plan| &plan.renames);
            copy_and_transform_appearance_streams_with_renames(
                self.pdf,
                &copied,
                cm,
                appearance_renames,
            )?; // cov:ignore: LLVM maps this multiline appearance-transform call to a defensive continuation edge
            let rect = transformed_annotation_rectangle(self.pdf, &copied, cm)?;
            copied.replace_key(b"/Rect", rect)?;
            self.pdf.mark_object_handle_dirty(&copied)?;
            transformed.new_annotations.push(copied);
        }

        Ok(transformed)
    }

    #[allow(dead_code, clippy::mutable_key_type)]
    fn copy_transform_object(
        &mut self,
        source: &ObjectHandle,
        orig_to_copy: &mut HashMap<ObjectHandleIdentity, ObjectHandle>,
    ) -> Result<Option<ObjectHandle>> {
        let source = self.pdf.resolve_to_terminal(source)?;
        let identity = source.identity_key();
        if let Some(copied) = orig_to_copy.get(&identity) {
            return Ok(Some(copied.clone()));
        }
        let copied = self
            .pdf
            .make_indirect_object_handle(source.shallow_copy()?)?;
        orig_to_copy.insert(identity, copied.clone());
        Ok(Some(copied))
    }

    #[allow(dead_code, clippy::mutable_key_type)]
    fn canonical_top_level_field(&mut self, start: ObjectHandle) -> Result<ObjectHandle> {
        let mut current = self.pdf.resolve_to_terminal(&start)?;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.identity_key()) {
                return Ok(current);
            }
            let parent = self
                .pdf
                .resolve_to_terminal(&current.try_get_key(b"/Parent")?)?;
            if parent.is_null() {
                return Ok(current);
            }
            current = parent;
            // qpdf's `getKeyIfDict` checks only whether the receiver is null;
            // a non-dictionary parent is followed once, emits the normal
            // dictionary type warning from `getKey`, and is returned as the
            // top-level handle (`QPDFFormFieldObjectHelper.cc:36-47`).
            if current.as_dictionary().is_none() {
                let _ = current.try_get_key(b"/Parent")?;
                return Ok(current);
            }
        }
    }

    #[allow(dead_code, clippy::mutable_key_type)]
    fn copy_field_tree(
        &mut self,
        top_field: &ObjectHandle,
        orig_to_copy: &mut HashMap<ObjectHandleIdentity, ObjectHandle>,
    ) -> Result<ObjectHandle> {
        self.copy_field_tree_with_overrides(top_field, orig_to_copy, None, None)
    }

    #[allow(dead_code, clippy::mutable_key_type)]
    fn copy_field_tree_with_overrides(
        &mut self,
        top_field: &ObjectHandle,
        orig_to_copy: &mut HashMap<ObjectHandleIdentity, ObjectHandle>,
        inherited_overrides: Option<&InheritedFieldOverrides>,
        foreign_resources: Option<&ForeignResourcePlan>,
    ) -> Result<ObjectHandle> {
        let copied_top = self
            .copy_transform_object(top_field, orig_to_copy)?
            .ok_or_else(|| Error::Unsupported("AcroForm top-level field is a stream".into()))?;
        let mut queue = VecDeque::from([(top_field.clone(), copied_top.clone())]);
        let mut seen = HashSet::new();

        while let Some((source, copied)) = queue.pop_front() {
            let source = self.pdf.resolve_to_terminal(&source)?;
            if !seen.insert(source.identity_key()) {
                continue;
            }

            let parent = self
                .pdf
                .resolve_to_terminal(&copied.try_get_key(b"/Parent")?)?;
            if !parent.is_null() {
                if let Some(parent_copy) = orig_to_copy.get(&parent.identity_key()) {
                    copied.replace_key(b"/Parent", parent_copy.clone())?;
                    self.pdf.mark_object_handle_dirty(&copied)?;
                } else {
                    parent.warn_if_possible(
                        "while traversing an AcroForm field, found a parent that had not been seen",
                    )?; // cov:ignore: warning continuation is an llvm-cov defensive error-edge artifact
                }
            }

            let kids_holder = self
                .pdf
                .resolve_to_terminal(&copied.try_get_key(b"/Kids")?)?;
            // qpdf's `if (kids.isArray()) { ... }` (`QPDFAcroFormDocumentHelper.cc:900-909`)
            // is a plain conditional, not an early exit: a terminal field with
            // no `/Kids` still falls through to the unconditional
            // `adjustInheritedFields` call below (`:914-917`).
            if let Some(kids) = kids_holder.try_as_array()? {
                for (index, kid) in kids.into_iter().enumerate() {
                    let kid = self.pdf.resolve_to_terminal(&kid)?;
                    let Some(copied_kid) = self.copy_transform_object(&kid, orig_to_copy)? else {
                        continue; // cov:ignore: defensive compatibility arm; stream copies now propagate qpdf's clone error
                    };
                    kids_holder.set_array_item(index, copied_kid.clone())?;
                    self.pdf.mark_object_handle_dirty(&kids_holder)?;
                    queue.push_back((kid, copied_kid));
                }
            }
            if let Some(inherited_overrides) = inherited_overrides {
                self.adjust_inherited_field(&copied, inherited_overrides)?;
            }
            if let Some(foreign_resources) = foreign_resources {
                self.adjust_foreign_field_resources(&copied, foreign_resources)?;
            }
        }

        Ok(copied_top)
    }

    #[allow(clippy::mutable_key_type)]
    fn adjust_foreign_field_resources(
        &mut self,
        field: &ObjectHandle,
        resources: &ForeignResourcePlan,
    ) -> Result<()> {
        if field.try_has_key(b"/DR")? {
            field.replace_key(b"/DR", resources.destination_resources.clone())?;
            self.pdf.mark_object_handle_dirty(field)?;
        }
        let default_appearance = self.pdf.resolve_to_terminal(&field.try_get_key(b"/DA")?)?;
        let Some(default_appearance) = default_appearance.as_string() else {
            return Ok(());
        };
        if resources.renames.is_empty() {
            return Ok(());
        }
        // qpdf's `adjustDefaultAppearances` decodes `/DA` via `getUTF8Value()`
        // before tokenizing it as content-stream syntax
        // (`QPDFAcroFormDocumentHelper.cc:596`), rather than tokenizing the
        // raw stored bytes; the filtered result is then written back as raw
        // bytes via `newString`, not re-encoded through `newUnicodeString`
        // (`:609`).
        let default_appearance = decode_field_name(&default_appearance).into_bytes();
        let Some(rewritten) = replace_resource_names(&default_appearance, &resources.renames)?
        else {
            let warning = "Unable to parse /DA while remapping foreign AcroForm resources";
            field.warn_if_possible(warning)?;
            return Ok(());
        };
        if rewritten != default_appearance {
            field.replace_key(b"/DA", ObjectHandle::string(rewritten))?;
            self.pdf.mark_object_handle_dirty(field)?;
        } // cov:ignore: LLVM maps this replacement-branch closing edge separately
        Ok(())
    }

    /// Pin foreign-copied inherited `/DA` and `/Q` values when the source and
    /// destination document defaults differ, matching qpdf's
    /// `adjustInheritedFields` (`QPDFAcroFormDocumentHelper.cc:442-484`).
    #[allow(clippy::mutable_key_type)]
    fn adjust_inherited_field(
        &mut self,
        field: &ObjectHandle,
        overrides: &InheritedFieldOverrides,
    ) -> Result<()> {
        if overrides.override_da && !self.field_has_explicit_value(field, b"/DA")? {
            let current = self.effective_field_appearance(field)?;
            if current != overrides.source_default_da {
                // cov:ignore-start: LLVM maps this multiline default-appearance replacement to a defensive continuation edge.
                field.replace_key(
                    b"/DA",
                    ObjectHandle::string(crate::pdf_string::new_unicode_string(
                        &overrides.source_default_da,
                    )),
                )?;
                // cov:ignore-end
                self.pdf.mark_object_handle_dirty(field)?;
            } // cov:ignore: default-appearance branch join is an llvm-cov region artifact
        } // cov:ignore: default-appearance branch join is an llvm-cov region artifact
        if overrides.override_q && !self.field_has_explicit_value(field, b"/Q")? {
            let current = self.effective_field_quadding(field)?;
            if current != overrides.source_default_q {
                field.replace_key(b"/Q", ObjectHandle::integer(overrides.source_default_q))?;
                self.pdf.mark_object_handle_dirty(field)?;
            } // cov:ignore: quadding branch join is an llvm-cov region artifact
        } // cov:ignore: quadding branch join is an llvm-cov region artifact
        Ok(())
    }

    #[allow(clippy::mutable_key_type)]
    fn field_has_explicit_value(&mut self, start: &ObjectHandle, key: &[u8]) -> Result<bool> {
        let mut current = self.pdf.resolve_to_terminal(start)?;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.identity_key()) {
                return Ok(false);
            }
            if current.try_has_key(key)? {
                return Ok(true);
            }
            let parent = self
                .pdf
                .resolve_to_terminal(&current.try_get_key(b"/Parent")?)?;
            if parent.is_null() || parent.as_dictionary().is_none() {
                return Ok(false);
            }
            current = parent;
        }
    }

    #[allow(clippy::mutable_key_type)]
    fn effective_field_appearance(&mut self, start: &ObjectHandle) -> Result<Vec<u8>> {
        let mut current = self.pdf.resolve_to_terminal(start)?;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.identity_key()) {
                break;
            }
            let appearance = self
                .pdf
                .resolve_to_terminal(&current.try_get_key(b"/DA")?)?;
            if let Some(value) = appearance.as_string() {
                return Ok(decode_field_name(&value).into_bytes());
            }
            if !appearance.is_null() {
                break;
            }
            let parent = self
                .pdf
                .resolve_to_terminal(&current.try_get_key(b"/Parent")?)?;
            if parent.is_null() || parent.as_dictionary().is_none() {
                break;
            }
            current = parent;
        }
        Ok(self.canonical_acroform_defaults()?.default_appearance)
    }

    #[allow(clippy::mutable_key_type)]
    fn effective_field_quadding(&mut self, start: &ObjectHandle) -> Result<i64> {
        let mut current = self.pdf.resolve_to_terminal(start)?;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.identity_key()) {
                break;
            }
            let quadding = self.pdf.resolve_to_terminal(&current.try_get_key(b"/Q")?)?;
            if let Some(value) = quadding.as_integer() {
                return Ok(value);
            }
            if !quadding.is_null() {
                break;
            }
            let parent = self
                .pdf
                .resolve_to_terminal(&current.try_get_key(b"/Parent")?)?;
            if parent.is_null() || parent.as_dictionary().is_none() {
                break;
            }
            current = parent;
        }
        Ok(self.canonical_acroform_defaults()?.quadding)
    }

    /// Append copied top-level fields to `/AcroForm/Fields`, reusing qpdf's
    /// suffix for every copied field that has the same colliding fully
    /// qualified name. The analyzed `name_to_fields` cache is deliberately
    /// frozen until all copied fields have been renamed and only then added
    /// to `/Fields`, matching qpdf's `addAndRenameFormFields`
    /// (`QPDFAcroFormDocumentHelper.cc:62-110`, cache construction at
    /// `:235-362`). Fully-qualified names follow
    /// `QPDFFormFieldObjectHelper::getFullyQualifiedName`
    /// (`QPDFFormFieldObjectHelper.cc:104-127`).
    #[cfg(test)]
    #[allow(clippy::mutable_key_type)]
    pub(crate) fn add_and_rename_form_fields(&mut self, fields: Vec<ObjectHandle>) -> Result<()> {
        self.add_and_rename_form_fields_with_reserved_names(fields, &BTreeSet::new())
    }

    /// Append copied fields while reserving primary names that qpdf keeps in
    /// its live collision index during `--pages` selection.
    #[allow(clippy::mutable_key_type)]
    pub(crate) fn add_and_rename_form_fields_with_reserved_names(
        &mut self,
        fields: Vec<ObjectHandle>,
        reserved_names: &BTreeSet<Vec<u8>>,
    ) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }

        self.analyze()?;
        let mut existing_names: BTreeSet<String> = {
            let cache = self.cache.borrow();
            cache
                .as_ref()
                .expect("analyze always installs an AcroForm cache")
                .name_to_fields
                .keys()
                .cloned()
                .collect()
        };
        existing_names.extend(reserved_names.iter().map(|name| decode_field_name(name)));
        let mut renames = BTreeMap::<String, Vec<u8>>::new();
        let mut seen = HashSet::new();
        let mut queue: VecDeque<ObjectHandle> = fields.iter().cloned().collect();

        while let Some(field) = queue.pop_front() {
            let field = self.pdf.resolve_to_terminal(&field)?;
            if !seen.insert(field.identity_key()) {
                continue;
            }

            let kids = self
                .pdf
                .resolve_to_terminal(&field.try_get_key(b"/Kids")?)?;
            if let Some(kids) = kids.try_as_array()? {
                queue.extend(kids);
            }

            if !field.try_has_key(b"/T")? {
                continue;
            }
            let old_name = self.canonical_fully_qualified_name(field.clone())?;
            let append = if let Some(append) = renames.get(&old_name) {
                append.clone()
            } else {
                let mut append = Vec::new();
                let mut candidate = old_name.clone();
                let mut suffix = 0_u32;
                while existing_names.contains(&candidate) {
                    suffix = suffix.checked_add(1).ok_or_else(|| {
                        // cov:ignore-start: exhausting the u32 suffix space requires over four billion colliding fields
                        Error::Unsupported("field name suffix space exhausted".into())
                        // cov:ignore-end
                    })?; // cov:ignore: exhausting the u32 suffix space is infeasible for a PDF fixture
                    append = format!("+{suffix}").into_bytes();
                    candidate = old_name.clone();
                    candidate.push_str(std::str::from_utf8(&append).unwrap_or_default());
                }
                renames.insert(old_name.clone(), append.clone());
                append
            };

            if !append.is_empty() {
                let current_name = self.pdf.resolve_to_terminal(&field.try_get_key(b"/T")?)?;
                // qpdf appends to the *decoded* name (`getUTF8Value() + append`,
                // `QPDFAcroFormDocumentHelper.cc:99-103`), not the raw stored
                // bytes -- a `/T` stored as UTF-16BE or PDFDocEncoded would
                // otherwise have the ASCII suffix appended mid-codepoint.
                let raw = current_name.as_string().unwrap_or_default();
                let mut partial = decode_field_name(&raw).into_bytes();
                partial.extend_from_slice(&append);
                // cov:ignore-start: LLVM maps this multiline mutation to a defensive continuation edge.
                field.replace_key(
                    b"/T",
                    ObjectHandle::string(crate::pdf_string::new_unicode_string(&partial)),
                )?;
                // cov:ignore-end
                self.pdf.mark_object_handle_dirty(&field)?;
            }
        }

        let acroform = self.canonical_get_or_create_acroform()?;
        let fields_array = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/Fields")?)?;
        let fields_array = if fields_array.as_array().is_some() {
            fields_array
        } else {
            let replacement = ObjectHandle::array(Vec::new());
            acroform.replace_key(b"/Fields", replacement.clone())?;
            self.pdf.mark_object_handle_dirty(&acroform)?;
            replacement
        };
        for field in &fields {
            fields_array.append_array_item(field.clone())?;
        }
        self.pdf.mark_object_handle_dirty(&fields_array)?;
        self.pdf.mark_object_handle_dirty(&acroform)?;
        for field in fields {
            self.update_cached_field(field)?;
        }
        Ok(())
    }

    /// Append copied top-level fields without renaming them, mirroring qpdf's
    /// `addFormField` (`QPDFAcroFormDocumentHelper.cc:49-59`). The
    /// `flattenRotation` caller has already removed the original field tree,
    /// so qpdf deliberately uses this no-rename route rather than
    /// `addAndRenameFormFields`.
    #[allow(clippy::mutable_key_type)]
    pub(crate) fn add_form_fields(&mut self, fields: Vec<ObjectHandle>) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }

        let acroform = self.canonical_get_or_create_acroform()?;
        let fields_array = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/Fields")?)?;
        let fields_array = if fields_array.as_array().is_some() {
            fields_array
        } else {
            let replacement = ObjectHandle::array(Vec::new());
            acroform.replace_key(b"/Fields", replacement.clone())?;
            self.pdf.mark_object_handle_dirty(&acroform)?;
            replacement
        };
        for field in &fields {
            fields_array.append_array_item(field.clone())?;
        }
        self.pdf.mark_object_handle_dirty(&fields_array)?;
        self.pdf.mark_object_handle_dirty(&acroform)?;
        for field in fields {
            self.update_cached_field(field)?;
        }
        Ok(())
    }

    fn update_cached_field(&mut self, field: ObjectHandle) -> Result<()> {
        let Some(mut cache) = self.cache.borrow_mut().take() else {
            return Ok(());
        };
        let mut visited = BTreeSet::new();
        let result = self.traverse_field_handles(field, None, 0, &mut visited, &mut cache);
        *self.cache.borrow_mut() = Some(cache);
        result
    }

    pub(crate) fn canonical_get_or_create_acroform(&mut self) -> Result<ObjectHandle> {
        let root_ref = self.pdf.root_ref().ok_or(Error::Missing("/Root"))?;
        let root = self.pdf.get_object_handle(root_ref);
        let root = self.pdf.resolve_to_terminal(&root)?;
        let acroform = self
            .pdf
            .resolve_to_terminal(&root.try_get_key(b"/AcroForm")?)?;
        if acroform.as_dictionary().is_some() {
            return Ok(acroform);
        }

        let created = self
            .pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(vec![(
                b"/Fields".to_vec(),
                ObjectHandle::array(Vec::new()),
            )]))?;
        root.replace_key(b"/AcroForm", created.clone())?;
        self.pdf.mark_object_handle_dirty(&root)?;
        Ok(created)
    }

    fn prepare_foreign_resource_plan<RS: Read + Seek>(
        &mut self,
        source_resources: Option<ObjectHandle>,
        source: &mut Pdf<RS>,
    ) -> Result<ForeignResourcePlan> {
        let destination_resources = self.canonical_get_or_create_acroform_resources()?;
        let mut conflicts = ResourceConflicts::new();
        destination_resources.make_resources_indirect(self.pdf)?;
        // A missing source `/DR` mirrors qpdf's null `from_dr`
        // (`QPDFAcroFormDocumentHelper.cc:730-732`): the destination `/DR`
        // still gets created/promoted above, but there is nothing to merge.
        if let Some(source_resources) = source_resources {
            let source_resources = ensure_foreign_indirect(source, source_resources)?;
            let source_resources = self.pdf.copy_foreign_object(&source_resources)?;
            source_resources.make_resources_indirect(self.pdf)?;
            destination_resources.merge_resources(&source_resources, Some(&mut conflicts))?;
        }
        self.pdf.mark_object_handle_dirty(&destination_resources)?;
        Ok(ForeignResourcePlan {
            destination_resources,
            renames: resource_renames_from_conflicts(&conflicts),
        })
    }

    #[allow(clippy::mutable_key_type)]
    fn canonical_get_or_create_acroform_resources(&mut self) -> Result<ObjectHandle> {
        let acroform = self.canonical_get_or_create_acroform()?;
        let resources = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/DR")?)?;
        if resources.as_dictionary().is_some() {
            if resources.object_ref().is_some() {
                return Ok(resources);
            }
            let indirect = self
                .pdf
                .make_indirect_object_handle(resources.shallow_copy()?)?;
            acroform.replace_key(b"/DR", indirect.clone())?;
            self.pdf.mark_object_handle_dirty(&acroform)?;
            return Ok(indirect);
        }

        let created = self
            .pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))?;
        acroform.replace_key(b"/DR", created.clone())?;
        self.pdf.mark_object_handle_dirty(&acroform)?;
        Ok(created)
    }

    fn canonical_acroform_defaults(&mut self) -> Result<AcroFormDefaults> {
        let Some(acroform) = self.canonical_acroform()? else {
            return Ok(AcroFormDefaults::default());
        };
        let appearance = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/DA")?)?;
        let quadding = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/Q")?)?;
        let resources = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/DR")?)?;
        Ok(AcroFormDefaults {
            default_appearance: appearance
                .as_string()
                .map(|value| decode_field_name(&value).into_bytes())
                .unwrap_or_default(),
            quadding: quadding.as_integer().unwrap_or(0),
            resources: resources.as_dictionary().map(|_| resources),
        })
    }

    #[allow(dead_code, clippy::mutable_key_type)]
    fn canonical_fully_qualified_name(&mut self, start: ObjectHandle) -> Result<String> {
        let mut current = self.pdf.resolve_to_terminal(&start)?;
        let mut seen = HashSet::new();
        let mut parts = Vec::new();
        loop {
            if !seen.insert(current.identity_key()) {
                break;
            }
            let partial = self.pdf.resolve_to_terminal(&current.try_get_key(b"/T")?)?;
            if let Some(name) = partial.as_string() {
                parts.push(decode_field_name(&name));
            }
            let parent = self
                .pdf
                .resolve_to_terminal(&current.try_get_key(b"/Parent")?)?;
            if parent.is_null() || parent.as_dictionary().is_none() {
                break;
            }
            current = parent;
        }
        parts.reverse();
        Ok(parts.join("."))
    }

    /// Return the live field handle for a widget annotation, mirroring
    /// `QPDFAcroFormDocumentHelper::getFieldForAnnotation`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:218-232`).
    fn canonical_field_for_annotation(
        &mut self,
        annotation: ObjectHandle,
    ) -> Result<Option<ObjectHandle>> {
        let annotation = self.pdf.resolve_to_terminal(&annotation)?;
        if !annotation.try_is_dictionary_of_type(b"", b"Widget")? {
            return Ok(None);
        }
        self.analyze()?;
        let cache = self.cache.borrow();
        Ok(cache
            .as_ref()
            .expect("analyze always installs an AcroForm cache")
            .annotation_to_field
            .get(&annotation.identity_key())
            .cloned())
    }

    fn canonical_acroform(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let root = self.pdf.get_object_handle(root_ref);
        let root = self.pdf.resolve_to_terminal(&root)?;
        if root.as_dictionary().is_none() {
            return Ok(None);
        }
        let acroform = self
            .pdf
            .resolve_to_terminal(&root.try_get_key(b"/AcroForm")?)?;
        Ok(acroform.as_dictionary().is_some().then_some(acroform))
    }

    fn traverse_field_handles(
        &mut self,
        field: ObjectHandle,
        parent: Option<ObjectHandle>,
        depth: usize,
        visited: &mut BTreeSet<ObjectRef>,
        cache: &mut AcroFormCache,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Ok(());
        }

        let field = self.pdf.resolve_to_terminal(&field)?;
        let Some(field_ref) = field.object_ref() else {
            field.warn_if_possible(
                "encountered a direct object as a field or annotation while traversing /AcroForm; ignoring field or annotation",
            )?; // cov:ignore: warning continuation is an llvm-cov defensive error-edge artifact
            return Ok(());
        };
        if field.as_dictionary().is_none() {
            field.warn_if_possible(
                "encountered a non-dictionary as a field or annotation while traversing /AcroForm; ignoring field or annotation",
            )?; // cov:ignore: warning continuation is an llvm-cov defensive error-edge artifact
            return Ok(());
        }
        if !visited.insert(field_ref) {
            field.warn_if_possible("loop detected while traversing /AcroForm")?;
            return Ok(());
        }

        let kids = self
            .pdf
            .resolve_to_terminal(&field.try_get_key(b"/Kids")?)?;
        let mut is_field = depth == 0;
        let is_annotation;
        if let Some(kids) = kids.as_array() {
            is_field = true;
            let parent = Some(field.clone());
            for kid in kids {
                self.traverse_field_handles(kid, parent.clone(), depth + 1, visited, cache)?;
            }
            is_annotation = false;
        } else {
            is_field |= field.try_has_key(b"/Parent")?;
            is_annotation = field.try_has_key(b"/Subtype")?
                || field.try_has_key(b"/Rect")?
                || field.try_has_key(b"/AP")?;
        }

        if is_annotation {
            let owning_field = if is_field {
                field.clone()
            } else {
                parent.unwrap_or_else(|| field.clone())
            };
            record_association(cache, field.clone(), owning_field);
        }

        if is_field && field.try_has_key(b"/T")? {
            let name = FormFieldObjectHelper::new(field_ref, self.pdf).fully_qualified_name()?;
            record_field_name(cache, field, name);
        }
        Ok(())
    }

    /// Return whether the catalog visibly contains an `/AcroForm` entry.
    ///
    /// This mirrors `QPDFAcroFormDocumentHelper::hasAcroForm`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:32-36`): the entry need not
    /// resolve to a dictionary, but a missing, direct-null, or dangling-null
    /// value is treated as absent.
    ///
    /// # Errors
    ///
    /// Propagates errors while resolving the catalog handle.
    pub fn has_acro_form(&mut self) -> Result<bool> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(false);
        };
        self.pdf
            .get_object_handle(root_ref)
            .try_has_key(b"/AcroForm")
    }

    /// Return the field's inherited `/V` value.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when a field-tree node is not a dictionary, or
    ///   when the field-tree depth limit is exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn field_value(&mut self, field_ref: ObjectRef) -> Result<Option<ObjectHandle>> {
        FormFieldObjectHelper::new(field_ref, self.pdf).field_value()
    }

    /// Return qpdf's `/AcroForm /NeedAppearances` value.
    ///
    /// Missing, malformed, or non-boolean values read as `false`, matching
    /// `QPDFAcroFormDocumentHelper::getNeedAppearances`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:365-374`).
    ///
    /// # Errors
    ///
    /// Propagates errors while resolving the catalog and AcroForm handles.
    pub fn get_need_appearances(&mut self) -> Result<bool> {
        let Some(acroform) = self.canonical_acroform()? else {
            return Ok(false);
        };
        let value = self
            .pdf
            .resolve_to_terminal(&acroform.try_get_key(b"/NeedAppearances")?)?;
        Ok(value.as_boolean() == Some(true))
    }

    /// Set or remove `/AcroForm /NeedAppearances`.
    ///
    /// `true` replaces the entry with a direct boolean. `false` removes the
    /// entry unconditionally. A missing or non-dictionary `/AcroForm` is a
    /// qpdf-style no-op, matching `setNeedAppearances`
    /// (`libqpdf/QPDFAcroFormDocumentHelper.cc:376-391`).
    ///
    /// # Errors
    ///
    /// Propagates errors while resolving the catalog and AcroForm handles, or
    /// while marking a live handle dirty after mutation.
    pub fn set_need_appearances(&mut self, value: bool) -> Result<()> {
        let Some(acroform) = self.canonical_acroform()? else {
            return Ok(());
        };
        if value {
            acroform.replace_key(b"/NeedAppearances", ObjectHandle::boolean(true))?;
            self.pdf.mark_object_handle_dirty(&acroform)
        } else {
            let present = acroform.as_dictionary().is_some_and(|entries| {
                entries
                    .keys()
                    .any(|key| key.as_slice() == b"/NeedAppearances")
            });
            acroform.remove_key(b"/NeedAppearances");
            if present {
                self.pdf.mark_object_handle_dirty(&acroform)
            } else {
                Ok(())
            }
        }
    }

    /// Generate appearances when `/NeedAppearances` is true, then remove the
    /// marker.
    ///
    /// Pages and Widget annotations are visited in qpdf's document order. Each
    /// Widget is resolved through the cached AcroForm annotation-to-field map;
    /// text and choice fields use the canonical appearance renderer, while
    /// checkbox and radio fields reset their value through the form-field
    /// helper so their `/AS` state agrees with `/V`. Other button fields are
    /// intentionally left untouched, matching qpdf's
    /// `generateAppearancesIfNeeded` (`libqpdf/QPDFAcroFormDocumentHelper.cc:393-417`).
    ///
    /// # Errors
    ///
    /// Propagates errors while resolving the page tree, widget associations,
    /// fields, or generated appearance streams.
    pub fn generate_appearances_if_needed(&mut self) -> Result<()> {
        if !self.get_need_appearances()? {
            return Ok(());
        }

        for page_ref in crate::pages::page_refs(self.pdf)? {
            let widgets = {
                let mut page = PageObjectHelper::new(page_ref, self.pdf);
                page.get_annotation_handles(Some(b"/Widget"))?
            };
            for widget in widgets {
                let Some(field) = self.canonical_field_for_annotation(widget.clone())? else {
                    continue;
                };
                let mut form_field = FormFieldObjectHelper::from_object_handle(field, self.pdf);
                if form_field.field_type()?.as_deref() == Some(b"/Btn") {
                    if form_field.is_checkbox()? || form_field.is_radio_button()? {
                        let value = form_field.field_value()?.unwrap_or_else(ObjectHandle::null);
                        form_field.set_value(value, false)?;
                    } // cov:ignore: llvm-cov maps this covered branch closing brace to a zero-count structural region
                } else {
                    form_field.generate_appearance_for_handle(widget)?;
                }
            }
        }

        self.set_need_appearances(false)
    }

    /// Set the field's direct `/V` value.
    ///
    /// This updates the field dictionary itself. It does not synthesize widget
    /// appearance streams.
    ///
    /// # Errors
    ///
    /// - [`Error::Unsupported`] when `field_ref` does not resolve to a
    ///   dictionary.
    /// - Any error from [`Pdf::resolve`].
    pub fn set_field_value(&mut self, field_ref: ObjectRef, value: ObjectHandle) -> Result<()> {
        FormFieldObjectHelper::new(field_ref, self.pdf).set_field_attribute(b"V", value)
    }

    /// Set `/AcroForm/DA`, creating `/AcroForm` if needed.
    ///
    /// # Errors
    ///
    /// - [`Error::Missing`] when the document has no `/Root`.
    /// - [`Error::Unsupported`] when the catalog or `/AcroForm` does not resolve
    ///   to a dictionary, or when the object-number space is exhausted while
    ///   creating `/AcroForm`.
    /// - Any error from [`Pdf::resolve`].
    pub fn set_default_appearance(&mut self, appearance: Vec<u8>) -> Result<()> {
        let acroform_ref = self.ensure_acroform_ref()?;
        let acroform = self.resolve_dict(acroform_ref, "AcroForm")?;
        acroform.replace_key(b"/DA", ObjectHandle::string(appearance))?;
        self.pdf.mark_object_handle_dirty(&acroform)?;
        Ok(())
    }

    fn acroform_ref(&mut self) -> Result<Option<ObjectRef>> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.resolve_dict(root_ref, "catalog")?;
        let acroform = catalog.try_get_key(b"/AcroForm")?;
        Ok(acroform.object_ref().or_else(|| acroform.as_reference()))
    }

    fn acroform_dict(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(root_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.resolve_dict(root_ref, "catalog")?;
        let acroform = self
            .pdf
            .resolve_to_terminal(&catalog.try_get_key(b"/AcroForm")?)?;
        Ok(acroform.try_as_dictionary()?.is_some().then_some(acroform))
    }

    pub(crate) fn ensure_acroform_ref(&mut self) -> Result<ObjectRef> {
        if let Some(existing_ref) = self.acroform_ref()? {
            return Ok(existing_ref);
        }

        let root_ref = self.pdf.root_ref().ok_or(Error::Missing("/Root"))?;
        let catalog = self.resolve_dict(root_ref, "catalog")?;
        let existing = catalog.try_get_key(b"/AcroForm")?;
        let acroform = if existing.try_as_dictionary()?.is_some() {
            existing.shallow_copy()?
        } else {
            ObjectHandle::dictionary(vec![(b"/Fields".to_vec(), ObjectHandle::array(Vec::new()))])
        };
        let new_acroform = self.pdf.make_indirect_object_handle(acroform)?;
        let new_ref = new_acroform
            .object_ref()
            .expect("make_indirect_object_handle returns an indirect handle");
        catalog.replace_key(b"/AcroForm", new_acroform)?;
        self.pdf.mark_object_handle_dirty(&catalog)?;
        self.invalidate_cache();
        Ok(new_ref)
    }

    fn resolve_dict(&mut self, object_ref: ObjectRef, label: &str) -> Result<ObjectHandle> {
        let handle = self.pdf.get_object_handle(object_ref);
        self.pdf.resolve(&handle)?;
        if handle.try_as_dictionary()?.is_some() {
            Ok(handle)
        } else {
            Err(Error::Unsupported(format!(
                "{label} object {object_ref} is not a dictionary"
            )))
        }
    }

    fn resolve_field_dict(&mut self, field_ref: ObjectRef) -> Result<ObjectHandle> {
        self.resolve_dict(field_ref, "field")
    }

    fn walk_field_tree(
        &mut self,
        field_ref: ObjectRef,
        seen: &mut BTreeSet<ObjectRef>,
        out: &mut Vec<ObjectRef>,
    ) -> Result<()> {
        self.walk_field_tree_rec(field_ref, seen, out, 0)
    }

    fn walk_field_tree_rec(
        &mut self,
        field_ref: ObjectRef,
        seen: &mut BTreeSet<ObjectRef>,
        out: &mut Vec<ObjectRef>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Err(Error::Unsupported(format!(
                "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
            )));
        }
        if !seen.insert(field_ref) {
            return Ok(());
        }
        out.push(field_ref);

        let field = self.resolve_field_dict(field_ref)?;
        let Some(kids) = resolve_array_value(self.pdf, field.try_get_key(b"/Kids")?)? else {
            return Ok(());
        };
        for kid in kids {
            if let Some(kid_ref) = kid.object_ref().or_else(|| kid.as_reference()) {
                self.walk_field_tree_rec(kid_ref, seen, out, depth + 1)?;
            }
        }
        Ok(())
    }

    fn walk_field_info_tree(
        &mut self,
        field_ref: ObjectRef,
        inherited: FieldInheritance,
        seen: &mut BTreeSet<ObjectRef>,
        out: &mut Vec<AcroFormFieldInfo>,
        depth: usize,
    ) -> Result<()> {
        if depth > DEFAULT_MAX_ACROFORM_DEPTH {
            return Err(Error::Unsupported(format!(
                "AcroForm field tree depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
            )));
        }
        if !seen.insert(field_ref) {
            return Ok(());
        }

        let field = self.resolve_field_dict(field_ref)?;
        if is_pure_widget_annotation(&field)? {
            return Ok(());
        }
        let current = inherited.apply(self.pdf, &field)?;
        let partial_name = deref_leaf_handle(self.pdf, field.try_get_key(b"/T")?)?
            .as_ref()
            .and_then(ObjectHandle::as_string)
            .map(|name| name.to_vec());

        out.push(AcroFormFieldInfo {
            object_ref: field_ref,
            partial_name,
            full_name: current.full_name.clone(),
            field_type: current.field_type.clone(),
            value: current.value.clone(),
            default_value: current.default_value.clone(),
            field_flags: current.field_flags,
            default_appearance: current.default_appearance.clone(),
            quadding: current.quadding,
            max_len: current.max_len,
        });

        let Some(kids) = resolve_array_value(self.pdf, field.try_get_key(b"/Kids")?)? else {
            return Ok(());
        };
        for kid in kids {
            if let Some(kid_ref) = kid.object_ref().or_else(|| kid.as_reference()) {
                self.walk_field_info_tree(kid_ref, current.clone(), seen, out, depth + 1)?;
            }
        }
        Ok(())
    }
}

impl FieldInheritance {
    fn apply<R: Read + Seek>(&self, pdf: &mut Pdf<R>, field: &ObjectHandle) -> Result<Self> {
        let partial_name = deref_leaf_handle(pdf, field.try_get_key(b"/T")?)?
            .as_ref()
            .and_then(ObjectHandle::as_string)
            .map(|name| decode_field_name(&name));
        let full_name = match (self.full_name.is_empty(), partial_name.as_deref()) {
            (_, None) => self.full_name.clone(),
            (true, Some(name)) => name.to_string(),
            (false, Some(name)) => format!("{}.{}", self.full_name, name),
        };

        Ok(Self {
            full_name,
            field_type: inherited_name(pdf, field, b"/FT")?.or_else(|| self.field_type.clone()),
            value: inherited_object(pdf, field, b"/V")?.or_else(|| self.value.clone()),
            default_value: inherited_object(pdf, field, b"/DV")?
                .or_else(|| self.default_value.clone()),
            field_flags: inherited_integer(pdf, field, b"/Ff")?.or(self.field_flags),
            default_appearance: inherited_object(pdf, field, b"/DA")?
                .or_else(|| self.default_appearance.clone()),
            quadding: inherited_integer(pdf, field, b"/Q")?.or(self.quadding),
            max_len: inherited_integer(pdf, field, b"/MaxLen")?.or(self.max_len),
        })
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level AcroForm helper for this document.
    pub fn acroform(&mut self) -> Result<AcroFormDocumentHelper<'_, R>> {
        AcroFormDocumentHelper::new(self)
    }
}

#[allow(dead_code)]
fn copy_and_transform_appearance_streams<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annotation: &ObjectHandle,
    cm: Matrix,
) -> Result<()> {
    copy_and_transform_appearance_streams_with_renames(pdf, annotation, cm, None)
}

/// Copy and transform annotation appearance streams, then apply qpdf's
/// appearance-resource privatization for a foreign AcroForm merge.
///
/// The resource-replacer implementation is called through the canonical
/// `ObjectHandle` path immediately after each copied appearance stream, which
/// preserves qpdf's `transformAnnotations` ordering and keeps the old raw
/// caller in `overlay_annotations` isolated until its own consumer cutover.
fn copy_and_transform_appearance_streams_with_renames<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annotation: &ObjectHandle,
    cm: Matrix,
    renames: Option<&ResourceRenames>,
) -> Result<()> {
    let appearance = annotation.try_get_key(b"/AP")?;
    appearance.try_dereference()?;
    if appearance.as_dictionary().is_none() {
        return Ok(());
    }

    for key in appearance.try_get_keys()? {
        let entry = appearance.try_get_key(&key)?;
        entry.try_dereference()?;
        if entry.as_stream_dict().is_some() {
            let copied = entry.copy_stream()?;
            transform_appearance_stream_matrix(&copied, cm)?;
            pdf.mark_object_handle_dirty(&copied)?;
            adjust_copied_appearance_resources(pdf, &copied, renames)?;
            appearance.replace_key(&key, copied)?;
            pdf.mark_object_handle_dirty(&appearance)?;
            continue;
        }
        if entry.as_dictionary().is_none() {
            continue;
        }
        for state in entry.try_get_keys()? {
            let stream = entry.try_get_key(&state)?;
            stream.try_dereference()?;
            if stream.as_stream_dict().is_some() {
                let copied = stream.copy_stream()?;
                transform_appearance_stream_matrix(&copied, cm)?;
                pdf.mark_object_handle_dirty(&copied)?;
                adjust_copied_appearance_resources(pdf, &copied, renames)?;
                entry.replace_key(&state, copied)?;
                pdf.mark_object_handle_dirty(&entry)?;
            }
        }
    }
    Ok(())
}

fn adjust_copied_appearance_resources<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    copied: &ObjectHandle,
    renames: Option<&ResourceRenames>,
) -> Result<()> {
    let Some(renames) = renames.filter(|renames| !renames.is_empty()) else {
        return Ok(());
    };
    let mut dr_map = crate::overlay_annotations::DrMap::new();
    for (category, category_renames) in renames {
        for (old_name, new_name) in category_renames {
            dr_map.insert_rename(category, old_name.clone(), new_name.clone());
        }
    }
    crate::overlay_appearance_stream::adjust_appearance_stream_handle(pdf, copied, &dr_map)
}

#[allow(dead_code)]
fn transform_appearance_stream_matrix(stream: &ObjectHandle, cm: Matrix) -> Result<()> {
    let Some(dictionary) = stream.as_stream_dict() else {
        return Ok(());
    };
    let matrix = dictionary.try_get_key(b"/Matrix")?;
    matrix.try_dereference()?;
    let had_matrix = matrix.as_array().is_some();
    let mut transformed = if had_matrix {
        matrix_from_handle(&matrix).unwrap_or_default()
    } else {
        Matrix::default()
    };
    transformed.concat(cm);
    if had_matrix || transformed != Matrix::default() {
        // cov:ignore-start: LLVM maps this multiline replacement to a defensive continuation edge.
        dictionary.replace_key(
            b"/Matrix",
            ObjectHandle::array(
                transformed
                    .get_as_matrix()
                    .into_iter()
                    .map(qpdf_real)
                    .collect(),
            ),
        )?;
        // cov:ignore-end
    }
    Ok(())
}

#[allow(dead_code)]
fn matrix_from_handle(handle: &ObjectHandle) -> Option<Matrix> {
    let items = handle.as_array()?;
    if items.len() != 6 {
        return None;
    }
    let mut numbers = [0.0; 6];
    for (index, item) in items.iter().enumerate() {
        numbers[index] = item
            .as_integer()
            .map(|value| value as f64)
            .or_else(|| item.as_real())?;
    }
    Some(Matrix::from(numbers))
}

#[allow(dead_code)]
fn ensure_foreign_indirect<R: Read + Seek>(
    source: &mut Pdf<R>,
    handle: ObjectHandle,
) -> Result<ObjectHandle> {
    if handle.object_ref().is_some() {
        return Ok(handle);
    }
    source.make_indirect_object_handle(handle)
}

/// Resolve one level of indirection for a metadata leaf handle. A resolved
/// null (freed/unknown ref) is treated as absent to match qpdf's inherited
/// value lookup. Direct values pass through unchanged.
fn deref_leaf_handle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: ObjectHandle,
) -> Result<Option<ObjectHandle>> {
    let value = pdf.resolve_to_terminal(&value)?;
    Ok((!value.is_null()).then_some(value))
}

fn inherited_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: &ObjectHandle,
    key: &[u8],
) -> Result<Option<ObjectHandle>> {
    deref_leaf_handle(pdf, field.try_get_key(key)?)
}

fn inherited_name<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: &ObjectHandle,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    Ok(inherited_object(pdf, field, key)?
        .as_ref()
        .and_then(ObjectHandle::as_name))
}

fn inherited_integer<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    field: &ObjectHandle,
    key: &[u8],
) -> Result<Option<i64>> {
    Ok(inherited_object(pdf, field, key)?
        .as_ref()
        .and_then(ObjectHandle::as_integer))
}

#[allow(dead_code)]
fn transformed_annotation_rectangle<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annotation: &ObjectHandle,
    cm: Matrix,
) -> Result<ObjectHandle> {
    let rect = pdf.resolve_to_terminal(&annotation.try_get_key(b"/Rect")?)?;
    let rectangle = match rect.try_as_array()? {
        Some(items) if items.len() == 4 => {
            let mut numbers = [0.0; 4];
            let mut valid = true;
            for (index, item) in items.iter().enumerate() {
                let item = pdf.resolve_to_terminal(item)?;
                if let Some(number) = item
                    .as_integer()
                    .map(|value| value as f64)
                    .or_else(|| item.as_real())
                {
                    numbers[index] = number;
                } else {
                    valid = false;
                    break;
                }
            }
            if valid {
                Rectangle::new(
                    numbers[0].min(numbers[2]),
                    numbers[1].min(numbers[3]),
                    numbers[0].max(numbers[2]),
                    numbers[1].max(numbers[3]),
                )
            } else {
                Rectangle::default()
            }
        }
        _ => Rectangle::default(),
    };
    let transformed = cm.transform_rectangle(rectangle);
    Ok(ObjectHandle::array(
        [
            transformed.llx,
            transformed.lly,
            transformed.urx,
            transformed.ury,
        ]
        .into_iter()
        .map(qpdf_real)
        .collect(),
    ))
}

fn is_pure_widget_annotation(field: &ObjectHandle) -> Result<bool> {
    let is_widget = field
        .try_get_key(b"/Subtype")?
        .try_as_name()?
        .is_some_and(|name| name == b"Widget");
    let mut has_field_entries = false;
    for key in [
        b"/T".as_slice(),
        b"/FT".as_slice(),
        b"/Kids".as_slice(),
        b"/V".as_slice(),
        b"/DV".as_slice(),
        b"/Ff".as_slice(),
        b"/TU".as_slice(),
        b"/TM".as_slice(),
        b"/DA".as_slice(),
        b"/Q".as_slice(),
        b"/MaxLen".as_slice(),
    ] {
        if field.try_has_key(key)? {
            has_field_entries = true;
            break;
        }
    }

    Ok(is_widget && !has_field_entries)
}

fn decode_field_name(name: &[u8]) -> String {
    // qpdf's getUTF8Value path delegates to pdf_doc_to_utf8, which replaces
    // undefined PDFDocEncoding bytes with U+FFFD
    // (QPDF_String.cc:162-172; QUtil.cc:1772-1788). The JSON decoder
    // intentionally returns None for such bytes, so it is not the right
    // fallback for field names.
    String::from_utf8_lossy(&utf8_value(name)).into_owned()
}

/// Pre-round `v` so `ObjectHandle::real(rounded)`'s writer output (Rust's
/// shortest-roundtrip `f64::to_string`) matches qpdf's
/// `QUtil::double_to_string(v, 6, trim=true)` -- the default `newReal(double)`
/// precision used by every `newFromRectangle`/`newFromMatrix` array element
/// (`libqpdf/QUtil.cc:349-369`). Same round-trip trick as
/// `overlay_annotations.rs`'s `qpdf_real`, adapted to return an
/// [`ObjectHandle`] instead of the legacy [`Object`] type.
fn qpdf_real(v: f64) -> ObjectHandle {
    let s = format!("{v:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    let rounded: f64 = trimmed.parse().unwrap_or(0.0);
    ObjectHandle::real(rounded)
}

impl<'a, R: Read + Seek> AcroFormDocumentHelper<'a, R> {
    pub(crate) fn top_level_fields(&mut self) -> Result<Vec<ObjectRef>> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(Vec::new());
        };
        let Some(fields) = resolve_array_value(self.pdf, acroform.try_get_key(b"/Fields")?)? else {
            return Ok(Vec::new());
        };
        Ok(fields
            .into_iter()
            .filter_map(|item| item.object_ref().or_else(|| item.as_reference()))
            .collect())
    }

    /// Whether the document has an `/AcroForm` dictionary whose `/Fields`
    /// resolves to an array, of any length including empty. This is qpdf's
    /// exact gate for rebuilding `/Fields` after page selection
    /// (`QPDFJob.cc:2609-2610`, `this_afdh->hasAcroForm() &&
    /// fields.isArray()`) — distinct from [`Self::top_level_fields`], which
    /// collapses "no `/AcroForm`", "`/Fields` absent", and "`/Fields` is a
    /// non-empty array" into the same empty `Vec` and cannot signal whether
    /// the rebuild gate should fire at all.
    pub(crate) fn has_fields_array(&mut self) -> Result<bool> {
        let Some(acroform) = self.acroform_dict()? else {
            return Ok(false);
        };
        Ok(resolve_array_value(self.pdf, acroform.try_get_key(b"/Fields")?)?.is_some())
    }
}

pub(crate) fn collect_reachable_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    object_ref: ObjectRef,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    skip_parent_key: bool,
) -> Result<()> {
    // The `seen` cycle guard cannot stop a long *acyclic* indirect-reference chain
    // (obj1 -> obj2 -> ... -> objN), where recursion depth grows with the chain length.
    // Bound the reference chain to avoid stack overflow on hostile source PDFs. Two
    // independent recursion axes are bounded separately: the `depth` parameter bounds the
    // indirect-reference-hop axis (`DEFAULT_MAX_ACROFORM_DEPTH`), incremented once per
    // resolved reference; inline structural nesting within a single resolved object is
    // bounded by the `inline_depth`/`MAX_INLINE_DEPTH` axis (see `collect_refs_in_object`),
    // reset to 0 at each ref hop because a freshly resolved object starts a new inline walk.
    if depth > DEFAULT_MAX_ACROFORM_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm reference chain depth exceeds maximum of {DEFAULT_MAX_ACROFORM_DEPTH}"
        )));
    }
    if !seen.insert(object_ref) {
        return Ok(());
    }
    out.insert(object_ref);

    let obj = pdf.resolve_object(object_ref)?;
    collect_refs_in_object(pdf, &obj, out, seen, depth, 0, skip_parent_key)
}

pub(crate) fn collect_refs_in_object<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    inline_depth: usize,
    skip_parent_key: bool,
) -> Result<()> {
    if inline_depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(format!(
            "AcroForm: inline object nesting exceeds maximum of {MAX_INLINE_DEPTH}"
        )));
    }
    match obj {
        Object::Reference(object_ref) => {
            // Ref hop: bump the ref-hop axis; `collect_reachable_refs` resets
            // `inline_depth` to 0 for the freshly resolved object.
            collect_reachable_refs(pdf, *object_ref, out, seen, depth + 1, skip_parent_key)
        }
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(
                    pdf,
                    item,
                    out,
                    seen,
                    depth,
                    inline_depth + 1,
                    skip_parent_key,
                )?;
            }
            Ok(())
        }
        Object::Dictionary(dict) => collect_refs_in_dict(
            pdf,
            dict,
            out,
            seen,
            depth,
            inline_depth + 1,
            skip_parent_key,
        ),
        Object::Stream(stream) => collect_refs_in_dict(
            pdf,
            &stream.dict,
            out,
            seen,
            depth,
            inline_depth + 1,
            skip_parent_key,
        ),
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::RealLiteral { .. }
        | Object::Name(_)
        | Object::String(_)
        | Object::Operator(_)
        | Object::InlineImage(_) => Ok(()),
    }
}

fn collect_refs_in_dict<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    out: &mut BTreeSet<ObjectRef>,
    seen: &mut BTreeSet<ObjectRef>,
    depth: usize,
    inline_depth: usize,
    skip_parent_key: bool,
) -> Result<()> {
    for (key, value) in dict.iter() {
        // Skip /P while it is a page back-pointer (`skip_parent_key` tracks that
        // context; see the `next_skip_parent_key` derivation below). Inside
        // resource data /P is an ordinary resource name and must be collected.
        if skip_parent_key && key == b"P" {
            continue;
        }
        // /P is a page back-pointer throughout the annotation/field graph — field
        // and widget dicts, but also nested annotations reached via non-field keys
        // (e.g. a widget's /Popup, whose own /P points at a page). Keep skipping it
        // across that whole graph. It only becomes an ordinary resource name once
        // the walk crosses into resource data via /Resources (an appearance
        // stream's, page's, or XObject's resources — e.g. a font named /P), so the
        // skip is lifted there and stays off for that resource subtree. (The
        // inherited /DR /DA walk already enters with the skip off; see the call in
        // `source_field_copy_set`.)
        let next_skip_parent_key = skip_parent_key && key != b"Resources";
        // Forward the same `inline_depth`: the caller incremented it when
        // descending into this dict, and each value re-enters
        // `collect_refs_in_object` where the guard re-checks.
        collect_refs_in_object(
            pdf,
            value,
            out,
            seen,
            depth,
            inline_depth,
            next_skip_parent_key,
        )?;
    }
    Ok(())
}

fn resolve_array_value<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: ObjectHandle,
) -> Result<Option<Vec<ObjectHandle>>> {
    // The array carrier itself may be a holder chain (`/Fields 20 0 R →
    // 21 0 R → [..]`); follow it to the terminal so a doubled-indirect
    // carrier yields its array instead of being dropped as a non-array.
    let value = pdf.resolve_to_terminal(&value)?;
    value.try_as_array()
}

fn resource_renames_from_conflicts(conflicts: &ResourceConflicts) -> ResourceRenames {
    conflicts
        .iter()
        .map(|(category, category_conflicts)| {
            (
                without_pdf_name_slash(category),
                category_conflicts
                    .iter()
                    .map(|(old_name, new_name)| {
                        (
                            without_pdf_name_slash(old_name),
                            without_pdf_name_slash(new_name),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

fn without_pdf_name_slash(value: &[u8]) -> Vec<u8> {
    value.strip_prefix(b"/").unwrap_or(value).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Stream, MAX_INLINE_DEPTH};
    use crate::pdf_string::decode_pdf_text_string;
    use std::rc::Rc;

    fn dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut dict = Dictionary::new();
        for (key, value) in entries {
            dict.insert(*key, value.clone());
        }
        dict
    }

    // Minimal valid PDF; nodes are supplied via set_object refs (catalog
    // unused). The unit tests below need arbitrary refs independent of any
    // fixture file's own object numbering.
    fn empty_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(std::io::Cursor::new(bytes)).expect("open")
    }

    fn rootless_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(std::io::Cursor::new(bytes)).expect("open")
    }

    fn refs_vec(nums: &[u32]) -> Vec<Object> {
        nums.iter()
            .map(|n| Object::Reference(ObjectRef::new(*n, 0)))
            .collect()
    }

    fn refs(nums: &[u32]) -> Object {
        Object::Array(refs_vec(nums))
    }

    /// Build `depth` levels of single-element arrays wrapping `Object::Null`.
    /// Contains no `Reference`, so walking it never reaches the resolve path.
    fn nested_arrays(depth: usize) -> Object {
        let mut o = Object::Null;
        for _ in 0..depth {
            o = Object::Array(vec![o]);
        }
        o
    }

    /// Minimal valid `Pdf` for tests that walk a `Reference`-free object. The
    /// `pdf` argument is required by the walker signature but is never touched
    /// because pure inline structure never reaches `pdf.resolve`.
    fn minimal_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        let bytes = include_bytes!("../../../tests/fixtures/compat/one-page.pdf");
        Pdf::open_mem_owned(bytes.to_vec()).expect("open")
    }

    #[test]
    fn collect_refs_in_object_errors_on_excessive_inline_nesting() {
        let mut pdf = minimal_pdf();
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let deep = nested_arrays(MAX_INLINE_DEPTH + 5);
        // arg order: (pdf, obj, out, seen, depth, inline_depth, skip_parent_key)
        let err = collect_refs_in_object(&mut pdf, &deep, &mut out, &mut seen, 0, 0, true);
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }

    #[test]
    fn collect_refs_in_object_accepts_inline_nesting_within_limit() {
        let mut pdf = minimal_pdf();
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        // Null leaf sits at inline_depth = MAX_INLINE_DEPTH, the deepest level
        // accepted under the strict `>` guard.
        let deep = nested_arrays(MAX_INLINE_DEPTH);
        collect_refs_in_object(&mut pdf, &deep, &mut out, &mut seen, 0, 0, true).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn collect_refs_in_object_walks_dict_and_stream_arms_within_limit() {
        let mut pdf = minimal_pdf();
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        // Shallow object exercising the Dictionary and Stream arms (no Reference,
        // so the resolve path is never hit and `pdf` stays unused by the walk).
        let stream = Object::Stream(Stream::new(
            dict(&[("Length", Object::Integer(0))]),
            Vec::new(),
        ));
        let obj = Object::Dictionary(dict(&[("S", stream), ("N", Object::Integer(1))]));
        collect_refs_in_object(&mut pdf, &obj, &mut out, &mut seen, 0, 0, true).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn annotation_to_field_map_merged_and_separated_widgets() {
        // AcroForm /Fields [7 8]; 7 is a merged field+widget (top-level,
        // /Subtype /Widget directly on it); 8 is a parent field with /Kids
        // [9], where 9 is a separated widget (/Parent 8, no /FT/T of its own).
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7, 8]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("FT", Object::Name(b"Tx".to_vec())),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[("Kids", refs(&[9]))])),
        );
        pdf.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(8, 0))),
            ])),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let map = helper.annotation_to_field_map().unwrap();
        assert_eq!(map.get(&ObjectRef::new(7, 0)), Some(&ObjectRef::new(7, 0)));
        // `annotation_to_field` maps a separated widget with its own
        // /Parent to itself, not its structural parent — qpdf's real
        // `traverseField`: `is_field` is true here because /Parent is
        // present, so `our_field = field` (9), matching
        // `libqpdf/QPDFAcroFormDocumentHelper.cc:343-347`. Callers that want
        // the named field use the canonical FormFieldObjectHelper route.
        assert_eq!(map.get(&ObjectRef::new(9, 0)), Some(&ObjectRef::new(9, 0)));
    }

    #[test]
    fn canonical_top_level_field_keeps_qpdfs_non_dictionary_parent() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(6, 0)))])),
        );
        pdf.set_object(ObjectRef::new(6, 0), Object::Integer(42));

        let field = pdf.get_object_handle(ObjectRef::new(5, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let top = helper
            .canonical_top_level_field(field)
            .expect("non-dictionary parent is a qpdf warning path");
        assert_eq!(top.object_ref(), Some(ObjectRef::new(6, 0)));
    }

    #[test]
    fn constructor_analyzes_before_external_graph_mutation() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[("Pages", Object::Reference(ObjectRef::new(2, 0)))])),
        );
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();

        helper.pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("T", Object::String(b"late".to_vec())),
            ])),
        );
        helper.pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[5]))])),
                ),
            ])),
        );

        assert!(
            helper
                .annotation_to_field_map()
                .expect("eager cache should still be readable")
                .is_empty(),
            "qpdf's constructor caches analyze() before later graph mutation"
        );
    }

    #[test]
    fn eager_analyze_warns_and_ignores_a_stream_field_kid() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[(
                "Kids",
                Object::Array(vec![Object::Reference(ObjectRef::new(8, 0))]),
            )])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        assert!(helper.annotation_to_field_map().unwrap().is_empty());
    }

    #[test]
    fn annotation_to_field_map_orphan_widget_self_maps_when_fields_are_not_an_array() {
        // qpdf replaces a non-array /Fields value with an empty array, then
        // still runs the orphan-widget fallback over page annotations.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", Object::Name(b"not-an-array".to_vec()))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "Annots",
                    Object::Array(vec![
                        Object::Reference(ObjectRef::new(5, 0)),
                        Object::Dictionary(dict(&[("Subtype", Object::Name(b"Widget".to_vec()))])),
                    ]),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Widget".to_vec()))])),
        );

        let map = AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .annotation_to_field_map()
            .unwrap();
        assert_eq!(map.get(&ObjectRef::new(5, 0)), Some(&ObjectRef::new(5, 0)));
    }

    #[test]
    fn annotation_to_field_map_skips_orphan_widget_fallback_without_fields_key() {
        // qpdf's analyze() (QPDFAcroFormDocumentHelper.cc:241-243) returns
        // before the field traversal AND the orphan-widget fallback when
        // /AcroForm lacks a /Fields key entirely -- a page-level orphan
        // widget must NOT be self-mapped in that case.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("NeedAppearances", Object::Boolean(true))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[5])),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Widget".to_vec()))])),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let map = helper.annotation_to_field_map().unwrap();
        assert!(map.is_empty());
        helper.generate_appearances_if_needed().unwrap();
        assert!(!helper.get_need_appearances().unwrap());
    }

    #[test]
    fn annotation_to_field_map_without_root_is_empty() {
        let mut pdf = rootless_pdf();

        assert!(AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .annotation_to_field_map()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn appearance_marker_helpers_without_root_are_noops() {
        let mut pdf = rootless_pdf();
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();

        assert!(!helper.has_acro_form().unwrap());
        assert!(!helper.get_need_appearances().unwrap());
        helper.set_need_appearances(false).unwrap();
    }

    #[test]
    fn annotation_to_field_map_with_a_non_dictionary_root_is_empty() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Name(b"not-a-catalog".to_vec()),
        );

        assert!(AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .annotation_to_field_map()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn annotation_to_field_map_ignores_field_tree_depth_overflow() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[(
                        "Fields",
                        Object::Array(vec![Object::Reference(ObjectRef::new(10, 0))]),
                    )])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        let depth = DEFAULT_MAX_ACROFORM_DEPTH + 2;
        for level in 0..depth {
            let object_ref = ObjectRef::new(10 + level as u32, 0);
            let value = if level + 1 < depth {
                Object::Dictionary(dict(&[(
                    "Kids",
                    Object::Array(vec![Object::Reference(ObjectRef::new(
                        11 + level as u32,
                        0,
                    ))]),
                )]))
            } else {
                Object::Dictionary(dict(&[("Subtype", Object::Name(b"Widget".to_vec()))]))
            };
            pdf.set_object(object_ref, value);
        }

        assert!(AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .annotation_to_field_map()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn annotation_to_field_map_ignores_a_field_tree_cycle() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[(
                        "Fields",
                        Object::Array(vec![Object::Reference(ObjectRef::new(10, 0))]),
                    )])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(10, 0),
            Object::Dictionary(dict(&[(
                "Kids",
                Object::Array(vec![Object::Reference(ObjectRef::new(10, 0))]),
            )])),
        );

        assert!(AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .annotation_to_field_map()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn annotation_to_field_map_treats_a_malformed_indirect_acroform_as_absent() {
        // qpdf's `analyze()` (`QPDFAcroFormDocumentHelper.cc:241-243`) reads
        // `/AcroForm` through `getKey`, which transparently dereferences,
        // then treats any non-dictionary result as absent with no error --
        // regardless of whether the source was a direct value or an
        // indirect reference to a dangling/null/non-dictionary target.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                ("AcroForm", Object::Reference(ObjectRef::new(9, 0))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(ObjectRef::new(9, 0), Object::Null);

        let map = AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .annotation_to_field_map()
            .expect("a malformed indirect /AcroForm must degrade to an empty map, not Err");
        assert!(map.is_empty());
    }

    #[test]
    fn transform_annotations_copies_and_transforms_a_plain_annotation() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Text".to_vec())),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(1),
                        Object::Integer(2),
                        Object::Integer(11),
                        Object::Integer(22),
                    ]),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Array(vec![Object::Reference(ObjectRef::new(5, 0))]),
        );

        let old_annots = pdf.get_object_handle(ObjectRef::new(6, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let transformed = helper
            .transform_annotations(old_annots, crate::Matrix::new(2.0, 0.0, 0.0, 2.0, 3.0, 4.0))
            .unwrap();

        assert!(transformed.new_fields.is_empty());
        assert!(transformed.old_fields.is_empty());
        assert_eq!(transformed.new_annotations.len(), 1);
        let copied = &transformed.new_annotations[0];
        assert_ne!(copied.object_ref(), Some(ObjectRef::new(5, 0)));
        let rect = copied
            .try_get_key(b"/Rect")
            .unwrap()
            .try_as_array()
            .unwrap()
            .unwrap();
        let coordinates: Vec<_> = rect
            .iter()
            .map(|item| {
                item.as_integer()
                    .map(|value| value as f64)
                    .or_else(|| item.as_real())
            })
            .collect();
        assert_eq!(
            coordinates,
            vec![Some(5.0), Some(8.0), Some(25.0), Some(48.0)]
        );
    }

    #[test]
    fn transform_annotations_copies_a_widget_field_tree_and_rewrites_parent_links() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[9])),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("FT", Object::Name(b"Tx".to_vec())),
                ("T", Object::String(b"field".to_vec())),
                ("Kids", refs(&[9])),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(10),
                        Object::Integer(20),
                        Object::Integer(30),
                        Object::Integer(40),
                    ]),
                ),
            ])),
        );
        let old_annots = pdf.get_object_handle(ObjectRef::new(3, 0));
        let old_annots = old_annots
            .try_get_key(b"/Annots")
            .expect("page annotations");

        let transformed = AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .transform_annotations(old_annots, Matrix::default())
            .unwrap();

        assert_eq!(
            transformed.old_fields,
            BTreeSet::from([ObjectRef::new(7, 0)])
        );
        assert_eq!(transformed.new_fields.len(), 1);
        assert_eq!(transformed.new_annotations.len(), 1);
        let copied_field = transformed.new_fields[0].clone();
        let copied_widget = transformed.new_annotations[0].clone();
        assert_ne!(copied_field.object_ref(), Some(ObjectRef::new(7, 0)));
        assert_ne!(copied_widget.object_ref(), Some(ObjectRef::new(9, 0)));

        let copied_parent = copied_widget.try_get_key(b"/Parent").unwrap().object_ref();
        assert_eq!(copied_parent, copied_field.object_ref());
        let copied_kids = copied_field
            .try_get_key(b"/Kids")
            .unwrap()
            .try_as_array()
            .unwrap()
            .unwrap();
        assert_eq!(copied_kids[0].object_ref(), copied_widget.object_ref());

        let copied_field_ref = copied_field.object_ref().unwrap();
        {
            let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
            helper
                .add_and_rename_form_fields(vec![copied_field.clone()])
                .unwrap();
        }
        let renamed = pdf.get_object_handle(copied_field_ref);
        let renamed_name = renamed.try_get_key(b"/T").unwrap().as_string().unwrap();
        assert_eq!(renamed_name, b"field+1");
    }

    #[test]
    fn add_and_rename_form_fields_reuses_suffix_for_shared_source_name() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[4]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        for field_ref in [4, 7, 8] {
            pdf.set_object(
                ObjectRef::new(field_ref, 0),
                Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
            );
        }

        let copied_fields = [7, 8]
            .into_iter()
            .map(|field_ref| pdf.get_object_handle(ObjectRef::new(field_ref, 0)))
            .collect();
        AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .add_and_rename_form_fields(copied_fields)
            .unwrap();

        for field_ref in [7, 8] {
            let field = pdf.get_object_handle(ObjectRef::new(field_ref, 0));
            assert_eq!(
                field.try_get_key(b"/T").unwrap().as_string(),
                Some(b"name+1".to_vec())
            );
        }
    }

    #[test]
    fn add_and_rename_form_fields_freezes_existing_names_until_after_bfs() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[4]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"name+1".to_vec()))])),
        );

        let copied_fields = [7, 8]
            .into_iter()
            .map(|field_ref| pdf.get_object_handle(ObjectRef::new(field_ref, 0)))
            .collect();
        AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .add_and_rename_form_fields(copied_fields)
            .unwrap();

        let first = pdf
            .get_object_handle(ObjectRef::new(7, 0))
            .try_get_key(b"/T")
            .unwrap()
            .as_string();
        let second = pdf
            .get_object_handle(ObjectRef::new(8, 0))
            .try_get_key(b"/T")
            .unwrap()
            .as_string();
        // qpdf checks only the pre-add analyze() cache during this pass. The
        // first copied field therefore becomes name+1, while the distinct
        // source name name+1 is not rechecked against that newly planned name.
        assert_eq!(first, Some(b"name+1".to_vec()));
        assert_eq!(second, Some(b"name+1".to_vec()));
    }

    #[test]
    fn add_and_rename_form_fields_updates_a_warm_name_cache_incrementally() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[4]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        for field_ref in [4, 7] {
            pdf.set_object(
                ObjectRef::new(field_ref, 0),
                Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
            );
        }

        let copied = pdf.get_object_handle(ObjectRef::new(7, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        helper
            .update_cached_field(copied.clone())
            .expect("the eagerly analyzed cache accepts incremental field updates");
        assert!(helper.cache.borrow().is_some());
        helper
            .canonical_annotation_to_field_handles()
            .expect("warm the qpdf name cache");
        helper
            .add_and_rename_form_fields(vec![copied.clone()])
            .expect("append and rename the copied field");

        let cache_store = helper.cache.borrow();
        let cache = cache_store
            .as_ref()
            .expect("qpdf addFormField keeps the analyzed cache valid");
        let renamed = cache
            .name_to_fields
            .get("name+1")
            .expect("incremental name index contains the renamed field");
        assert!(renamed.iter().any(|field| field.is_same_object_as(&copied)));
    }

    #[test]
    fn add_and_rename_form_fields_handles_nested_cyclic_trees_once() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[4]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"name".to_vec())),
                ("Kids", refs(&[8])),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"child".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
                ("Kids", refs(&[7])),
            ])),
        );

        let copied = pdf.get_object_handle(ObjectRef::new(7, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        helper
            .canonical_annotation_to_field_handles()
            .expect("warm the qpdf name cache");
        helper
            .add_and_rename_form_fields(vec![copied])
            .expect("walk the nested cycle once");

        let cache_store = helper.cache.borrow();
        let cache = cache_store
            .as_ref()
            .expect("qpdf preserves the analyzed cache");
        assert_eq!(cache.name_to_fields["name+1"].len(), 1);
        assert_eq!(cache.name_to_fields["name+1.child"].len(), 1);
    }

    #[test]
    fn set_form_field_name_updates_both_warm_qualified_name_indexes() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[4, 7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        for field_ref in [4, 7] {
            pdf.set_object(
                ObjectRef::new(field_ref, 0),
                Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
            );
        }

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        helper
            .canonical_annotation_to_field_handles()
            .expect("warm the qpdf name cache");
        assert_eq!(
            helper
                .get_fields_with_qualified_name("name")
                .expect("read the warm qualified-name cache"),
            BTreeSet::from([ObjectRef::new(4, 0), ObjectRef::new(7, 0)])
        );

        helper
            .set_form_field_name(ObjectRef::new(7, 0), "renamed")
            .expect("rename the field through the qpdf mutation route");

        assert_eq!(
            helper
                .get_fields_with_qualified_name("name")
                .expect("read the old qualified-name cache"),
            BTreeSet::from([ObjectRef::new(4, 0)])
        );
        assert_eq!(
            helper
                .get_fields_with_qualified_name("renamed")
                .expect("read the new qualified-name cache"),
            BTreeSet::from([ObjectRef::new(7, 0)])
        );

        helper
            .set_form_field_name(ObjectRef::new(4, 0), "other")
            .expect("rename the final field under the old qualified name");
        assert!(helper
            .get_fields_with_qualified_name("name")
            .expect("read the removed qualified-name cache entry")
            .is_empty());
        assert!(
            helper.cache.borrow().is_some(),
            "qpdf preserves a warm cache"
        );
    }

    #[test]
    fn remove_form_fields_prunes_a_warm_association_and_name_cache() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[4, 7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(4, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"name".to_vec())),
                ("Subtype", Object::Name(b"Widget".to_vec())),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(1),
                        Object::Integer(1),
                    ]),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"name".to_vec()))])),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let associations = helper
            .canonical_annotation_to_field_handles()
            .expect("warm the qpdf association and name caches");
        assert!(associations.iter().any(|(annotation, field)| {
            annotation.object_ref() == Some(ObjectRef::new(4, 0))
                && field.object_ref() == Some(ObjectRef::new(4, 0))
        }));

        assert!(helper
            .remove_form_fields(&BTreeSet::from([ObjectRef::new(4, 0)]))
            .expect("remove the selected top-level field"));

        {
            let cache_store = helper.cache.borrow();
            let cache = cache_store
                .as_ref()
                .expect("qpdf preserves a warm cache after removal");
            assert_eq!(
                cache
                    .name_to_fields
                    .get("name")
                    .into_iter()
                    .flatten()
                    .filter_map(ObjectHandle::object_ref)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([ObjectRef::new(7, 0)])
            );
            assert!(!cache
                .annotation_to_field
                .values()
                .any(|field| field.object_ref() == Some(ObjectRef::new(4, 0))));
        }

        assert!(!helper
            .remove_form_fields(&BTreeSet::from([ObjectRef::new(99, 0)]))
            .expect("removing an uncached field is a no-op"));

        assert_eq!(
            helper
                .get_fields_with_qualified_name("name")
                .expect("read the pruned qualified-name cache"),
            BTreeSet::from([ObjectRef::new(7, 0)])
        );
        assert!(!helper
            .canonical_annotation_to_field_handles()
            .expect("read the pruned association cache")
            .iter()
            .any(|(annotation, _)| annotation.object_ref() == Some(ObjectRef::new(4, 0))));
        assert!(
            helper.cache.borrow().is_some(),
            "qpdf preserves a warm cache"
        );
    }

    #[test]
    fn remove_cached_fields_erases_annotations_from_removed_forward_owner() {
        let mut pdf = empty_pdf();
        let field_one = pdf.get_object_handle(ObjectRef::new(4, 0));
        let field_two = pdf.get_object_handle(ObjectRef::new(7, 0));
        let annotation = pdf.get_object_handle(ObjectRef::new(9, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();

        let mut cache = AcroFormCache::default();
        record_association(&mut cache, annotation.clone(), field_one.clone());
        // qpdf's traverseField appends to the forward map and overwrites the
        // reverse map. A warm-cache reassociation therefore leaves the same
        // annotation in field one's stale forward list while the reverse map
        // points at field two.
        record_association(&mut cache, annotation.clone(), field_two.clone());
        *helper.cache.borrow_mut() = Some(cache);

        helper.remove_cached_fields(&BTreeSet::from([ObjectRef::new(4, 0)]));

        let cache_store = helper.cache.borrow();
        let cache = cache_store
            .as_ref()
            .expect("qpdf keeps the association cache warm after removal");
        assert!(!cache
            .annotation_to_field
            .contains_key(&annotation.identity_key()));
        assert!(!cache
            .annotation_handles
            .contains_key(&annotation.identity_key()));
        assert!(!cache
            .field_to_annotations
            .contains_key(&field_one.identity_key()));
        assert!(cache
            .field_to_annotations
            .contains_key(&field_two.identity_key()));
    }

    #[test]
    fn transform_annotations_copies_appearance_streams_and_concatenates_matrix() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Stream(Stream::new(
                dict(&[
                    ("Subtype", Object::Name(b"Form".to_vec())),
                    (
                        "Matrix",
                        Object::Array(vec![
                            Object::Integer(1),
                            Object::Integer(0),
                            Object::Integer(0),
                            Object::Integer(1),
                            Object::Integer(2),
                            Object::Integer(3),
                        ]),
                    ),
                    ("Length", Object::Integer(0)),
                ]),
                Vec::new(),
            )),
        );
        pdf.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Text".to_vec())),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(1),
                        Object::Integer(2),
                        Object::Integer(11),
                        Object::Integer(22),
                    ]),
                ),
                (
                    "AP",
                    Object::Dictionary(dict(&[("N", Object::Reference(ObjectRef::new(5, 0)))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(10, 0),
            Object::Array(vec![Object::Reference(ObjectRef::new(9, 0))]),
        );

        let old_annots = pdf.get_object_handle(ObjectRef::new(10, 0));
        let transformed = AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .transform_annotations(old_annots, Matrix::new(2.0, 0.0, 0.0, 2.0, 1.0, 1.0))
            .unwrap();
        let copied = transformed.new_annotations[0].clone();
        let ap = copied.try_get_key(b"/AP").unwrap();
        let normal = ap.try_get_key(b"/N").unwrap();
        assert_ne!(normal.object_ref(), Some(ObjectRef::new(5, 0)));
        let matrix = normal
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Matrix")
            .unwrap()
            .try_as_array()
            .unwrap()
            .unwrap();
        let matrix: Vec<_> = matrix
            .iter()
            .map(|item| {
                item.as_integer()
                    .map(|value| value as f64)
                    .or_else(|| item.as_real())
            })
            .collect();
        assert_eq!(
            matrix,
            vec![
                Some(2.0),
                Some(0.0),
                Some(0.0),
                Some(2.0),
                Some(3.0),
                Some(4.0)
            ]
        );
    }

    #[test]
    fn transform_annotations_rounds_generated_matrix_and_rect_reals_to_qpdf_precision() {
        // qpdf's `newReal(double)` default (`decimal_places = 0`) rounds to
        // six decimal places (`QUtil::double_to_string`, `QUtil.cc:349-369`),
        // not Rust's shortest-roundtrip `f64::to_string`. A 1/3 scale factor
        // produces a repeating decimal that distinguishes the two.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Stream(Stream::new(
                dict(&[
                    ("Subtype", Object::Name(b"Form".to_vec())),
                    ("Length", Object::Integer(0)),
                ]),
                Vec::new(),
            )),
        );
        pdf.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Text".to_vec())),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(0),
                        Object::Integer(0),
                        Object::Integer(1),
                        Object::Integer(1),
                    ]),
                ),
                (
                    "AP",
                    Object::Dictionary(dict(&[("N", Object::Reference(ObjectRef::new(5, 0)))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(10, 0),
            Object::Array(vec![Object::Reference(ObjectRef::new(9, 0))]),
        );

        let old_annots = pdf.get_object_handle(ObjectRef::new(10, 0));
        let third = 1.0 / 3.0;
        let transformed = AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .transform_annotations(old_annots, Matrix::new(third, 0.0, 0.0, third, 0.0, 0.0))
            .unwrap();
        let copied = transformed.new_annotations[0].clone();

        let rect = copied.try_get_key(b"/Rect").unwrap();
        assert_eq!(rect.unparse(), b"[ 0 0 0.333333 0.333333 ]");

        let normal = copied
            .try_get_key(b"/AP")
            .unwrap()
            .try_get_key(b"/N")
            .unwrap();
        let matrix = normal
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Matrix")
            .unwrap();
        assert_eq!(matrix.unparse(), b"[ 0.333333 0 0 0.333333 0 0 ]");
    }

    #[test]
    fn transform_annotations_handles_non_arrays_and_stream_annotations() {
        let mut pdf = empty_pdf();
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();

        let transformed = helper
            .transform_annotations(
                ObjectHandle::name(b"not-an-array".to_vec()),
                Matrix::default(),
            )
            .unwrap();
        assert!(transformed.new_annotations.is_empty());
        assert!(transformed.new_fields.is_empty());

        let stream =
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new()));
        let transformed = helper
            .transform_annotations(ObjectHandle::array(vec![stream]), Matrix::default())
            .unwrap();
        assert!(transformed.new_annotations.is_empty());
        assert!(transformed.new_fields.is_empty());
    }

    #[test]
    fn transform_annotations_from_handles_non_arrays_and_stream_annotations() {
        let mut source = empty_pdf();
        let mut target = empty_pdf();
        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(
                ObjectHandle::name(b"not-an-array".to_vec()),
                Matrix::default(),
                &mut source,
            )
            .unwrap();
        assert!(transformed.new_annotations.is_empty());

        let stream =
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new()));
        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(
                ObjectHandle::array(vec![stream]),
                Matrix::default(),
                &mut source,
            )
            .unwrap();
        assert!(transformed.new_annotations.is_empty());
        assert!(transformed.new_fields.is_empty());
    }

    #[test]
    fn transform_annotations_from_copies_a_foreign_widget_field_tree() {
        let mut source = empty_pdf();
        source.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        source.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"foreign".to_vec())),
                ("Kids", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(10),
                        Object::Integer(20),
                        Object::Integer(30),
                        Object::Integer(40),
                    ]),
                ),
            ])),
        );

        let source_annots = source.get_object_handle(ObjectRef::new(3, 0));
        let source_annots = source_annots.try_get_key(b"/Annots").unwrap();
        let mut target = empty_pdf();
        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(source_annots, Matrix::default(), &mut source)
            .unwrap();

        assert_eq!(transformed.new_fields.len(), 1);
        assert_eq!(transformed.new_annotations.len(), 1);
        let copied_field = &transformed.new_fields[0];
        let copied_widget = &transformed.new_annotations[0];
        assert_eq!(
            copied_widget.try_get_key(b"/Parent").unwrap().object_ref(),
            copied_field.object_ref()
        );
        assert_ne!(copied_field.object_ref(), Some(ObjectRef::new(7, 0)));
        assert_ne!(copied_widget.object_ref(), Some(ObjectRef::new(9, 0)));
    }

    #[test]
    fn copy_field_tree_handles_cycles_and_unseen_parents() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"parent".to_vec()))])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"top".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(6, 0))),
                (
                    "Kids",
                    Object::Array(vec![Object::Reference(ObjectRef::new(7, 0))]),
                ),
            ])),
        );

        let top = pdf.get_object_handle(ObjectRef::new(7, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let copied = helper.copy_field_tree(&top, &mut HashMap::new()).unwrap();
        assert!(copied.object_ref().is_some());
    }

    #[test]
    fn copy_field_tree_propagates_qpdf_stream_clone_error_for_a_stream_kid() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"top".to_vec())),
                (
                    "Kids",
                    Object::Array(vec![Object::Reference(ObjectRef::new(8, 0))]),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
        );

        let top = pdf.get_object_handle(ObjectRef::new(7, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let error = helper
            .copy_field_tree(&top, &mut HashMap::new())
            .expect_err("qpdf shallowCopy must reject a stream kid");
        assert!(matches!(
            error,
            Error::System(ref message) if message == "stream objects cannot be cloned"
        ));
    }

    #[test]
    fn copy_transform_object_rejects_streams_and_canonical_top_field_stops_cycles() {
        let mut pdf = empty_pdf();
        let stream =
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new()));
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(6, 0)))])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(5, 0)))])),
        );
        let field = pdf.get_object_handle(ObjectRef::new(5, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let error = helper
            .copy_transform_object(&stream, &mut HashMap::new())
            .expect_err("qpdf shallowCopy must reject a stream");
        assert!(matches!(
            error,
            Error::System(ref message) if message == "stream objects cannot be cloned"
        ));
        assert!(helper
            .canonical_top_level_field(field)
            .unwrap()
            .object_ref()
            .is_some());
    }

    #[test]
    fn add_and_rename_form_fields_handles_duplicate_handles_and_missing_fields_array() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"field".to_vec()))])),
        );

        let first = ObjectHandle::dictionary(vec![(
            b"/T".to_vec(),
            ObjectHandle::string(b"field".to_vec()),
        )]);
        let second = ObjectHandle::dictionary(vec![(
            b"/T".to_vec(),
            ObjectHandle::string(b"field".to_vec()),
        )]);
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        helper
            .add_and_rename_form_fields(vec![first.clone(), first.clone(), second.clone()])
            .unwrap();
        assert_eq!(
            first.try_get_key(b"/T").unwrap().as_string(),
            Some(b"field+1".to_vec())
        );
        assert_eq!(
            second.try_get_key(b"/T").unwrap().as_string(),
            Some(b"field+1".to_vec())
        );
    }

    #[test]
    fn add_and_rename_form_fields_decodes_utf16_names_before_appending_a_suffix() {
        // qpdf appends the suffix to the *decoded* name
        // (`getUTF8Value() + append`, `QPDFAcroFormDocumentHelper.cc:99-103`),
        // not the raw stored bytes. "名" (U+540D) has no PDFDocEncoding
        // representation, so a proper writer stores it as UTF-16BE with a
        // byte-order mark.
        let utf16_name: Vec<u8> = vec![0xfe, 0xff, 0x54, 0x0d];
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[("T", Object::String(utf16_name.clone()))])),
        );

        let field =
            ObjectHandle::dictionary(vec![(b"/T".to_vec(), ObjectHandle::string(utf16_name))]);
        AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .add_and_rename_form_fields(vec![field.clone()])
            .unwrap();

        let renamed = field.try_get_key(b"/T").unwrap().as_string().unwrap();
        assert_eq!(
            decode_pdf_text_string(&renamed).as_deref(),
            Some("名+1"),
            "renamed /T must decode to the original text plus the ASCII \
             suffix, not mid-codepoint-corrupted bytes: {renamed:?}"
        );
    }

    #[test]
    fn decode_field_name_matches_qpdf_for_undefined_pdfdoc_bytes() {
        assert_eq!(decode_field_name(&[b'A', 0x7f, 0x9f, 0xad, 0x80]), "A���•");
    }

    #[test]
    fn add_and_rename_form_fields_empty_input_is_a_noop() {
        let mut pdf = empty_pdf();
        AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .add_and_rename_form_fields(Vec::new())
            .unwrap();
    }

    #[test]
    fn add_and_rename_form_fields_creates_acroform_and_fields_array() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[("Pages", Object::Reference(ObjectRef::new(2, 0)))])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        let field = ObjectHandle::dictionary(vec![(
            b"/T".to_vec(),
            ObjectHandle::string(b"field".to_vec()),
        )]);
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        helper.add_and_rename_form_fields(Vec::new()).unwrap();
        let created = helper.canonical_get_or_create_acroform().unwrap();
        assert!(created.object_ref().is_some());
        helper
            .add_and_rename_form_fields(vec![field.clone()])
            .unwrap();
        let fields = created.try_get_key(b"/Fields").unwrap();
        assert_eq!(fields.try_as_array().unwrap().unwrap().len(), 1);

        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                ("AcroForm", Object::Dictionary(dict(&[]))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        let field = ObjectHandle::dictionary(vec![(
            b"/T".to_vec(),
            ObjectHandle::string(b"field".to_vec()),
        )]);
        AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .add_and_rename_form_fields(vec![field])
            .unwrap();
        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        let acroform = root.try_get_key(b"/AcroForm").unwrap();
        assert_eq!(
            acroform
                .try_get_key(b"/Fields")
                .unwrap()
                .try_as_array()
                .unwrap()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn add_form_fields_handles_empty_existing_and_malformed_fields_arrays() {
        let mut pdf = empty_pdf();
        let field = ObjectHandle::dictionary(vec![(
            b"/T".to_vec(),
            ObjectHandle::string(b"field".to_vec()),
        )]);
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        helper.add_form_fields(Vec::new()).unwrap();
        helper.add_form_fields(vec![field.clone()]).unwrap();
        drop(helper);

        let root = pdf.get_object_handle(ObjectRef::new(1, 0));
        let acroform = root.try_get_key(b"/AcroForm").unwrap();
        let fields = acroform
            .try_get_key(b"/Fields")
            .unwrap()
            .try_as_array()
            .unwrap()
            .unwrap();
        assert_eq!(fields.len(), 1);
        assert!(fields[0].is_same_object_as(&field));

        let mut malformed = empty_pdf();
        malformed.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[(
                "AcroForm",
                Object::Dictionary(dict(&[("Fields", Object::Name(b"bad".to_vec()))])),
            )])),
        );
        let replacement_field = ObjectHandle::dictionary(Vec::new());
        AcroFormDocumentHelper::new(&mut malformed)
            .unwrap()
            .add_form_fields(vec![replacement_field.clone()])
            .unwrap();
        let root = malformed.get_object_handle(ObjectRef::new(1, 0));
        let fields = root
            .try_get_key(b"/AcroForm")
            .unwrap()
            .try_get_key(b"/Fields")
            .unwrap()
            .try_as_array()
            .unwrap()
            .unwrap();
        assert_eq!(fields.len(), 1);
        assert!(fields[0].is_same_object_as(&replacement_field));
    }

    #[test]
    fn canonical_fully_qualified_name_walks_parents_and_stops_cycles() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("T", Object::String(b"root".to_vec()))])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"child".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(5, 0))),
            ])),
        );
        let child = pdf.get_object_handle(ObjectRef::new(6, 0));
        {
            let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
            assert_eq!(
                helper.canonical_fully_qualified_name(child).unwrap(),
                "root.child"
            );
        }

        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"a".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(8, 0))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"b".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
            ])),
        );
        let cyclic = pdf.get_object_handle(ObjectRef::new(7, 0));
        assert!(!AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .canonical_fully_qualified_name(cyclic)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn appearance_helpers_cover_nested_states_and_malformed_matrices() {
        let mut pdf = empty_pdf();
        let direct_stream = pdf
            .make_indirect_object_handle(ObjectHandle::stream(
                ObjectHandle::dictionary(Vec::new()),
                Rc::new(Vec::new()),
            ))
            .unwrap();
        let state_stream = pdf
            .make_indirect_object_handle(ObjectHandle::stream(
                ObjectHandle::dictionary(Vec::new()),
                Rc::new(Vec::new()),
            ))
            .unwrap();
        let annotation = ObjectHandle::dictionary(vec![(
            b"/AP".to_vec(),
            ObjectHandle::dictionary(vec![
                (b"/N".to_vec(), direct_stream),
                (
                    b"/R".to_vec(),
                    ObjectHandle::dictionary(vec![
                        (b"/On".to_vec(), state_stream),
                        (b"/Off".to_vec(), ObjectHandle::integer(0)),
                    ]),
                ),
                (b"/D".to_vec(), ObjectHandle::integer(0)),
            ]),
        )]);
        copy_and_transform_appearance_streams(&mut pdf, &annotation, Matrix::default()).unwrap();

        let non_stream = ObjectHandle::dictionary(Vec::new());
        transform_appearance_stream_matrix(&non_stream, Matrix::default()).unwrap();
        let no_matrix =
            ObjectHandle::stream(ObjectHandle::dictionary(Vec::new()), Rc::new(Vec::new()));
        transform_appearance_stream_matrix(&no_matrix, Matrix::default()).unwrap();
        assert!(no_matrix
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Matrix")
            .unwrap()
            .as_array()
            .is_none());
        transform_appearance_stream_matrix(&no_matrix, Matrix::new(2.0, 0.0, 0.0, 2.0, 1.0, 1.0))
            .unwrap();
        assert!(no_matrix
            .as_stream_dict()
            .unwrap()
            .try_get_key(b"/Matrix")
            .unwrap()
            .as_array()
            .is_some());

        assert!(matrix_from_handle(&ObjectHandle::array(vec![])).is_none());
        assert!(matrix_from_handle(&ObjectHandle::array(vec![
            ObjectHandle::integer(1),
            ObjectHandle::integer(0),
            ObjectHandle::integer(0),
            ObjectHandle::integer(1),
            ObjectHandle::name(b"bad".to_vec()),
            ObjectHandle::integer(0),
        ]))
        .is_none());
    }

    #[test]
    fn annotation_rectangle_and_foreign_indirect_helpers_handle_invalid_inputs() {
        let mut pdf = empty_pdf();
        for annotation in [
            ObjectHandle::dictionary(Vec::new()),
            ObjectHandle::dictionary(vec![(
                b"/Rect".to_vec(),
                ObjectHandle::array(vec![ObjectHandle::integer(1)]),
            )]),
            ObjectHandle::dictionary(vec![(
                b"/Rect".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(1),
                    ObjectHandle::integer(2),
                    ObjectHandle::name(b"bad".to_vec()),
                    ObjectHandle::integer(4),
                ]),
            )]),
        ] {
            let rectangle =
                transformed_annotation_rectangle(&mut pdf, &annotation, Matrix::default()).unwrap();
            assert_eq!(rectangle.try_as_array().unwrap().unwrap().len(), 4);
        }

        let direct =
            ensure_foreign_indirect(&mut pdf, ObjectHandle::dictionary(Vec::new())).unwrap();
        assert!(direct.object_ref().is_some());
        pdf.set_object(ObjectRef::new(20, 0), Object::Dictionary(dict(&[])));
        let indirect = pdf.get_object_handle(ObjectRef::new(20, 0));
        let same = ensure_foreign_indirect(&mut pdf, indirect.clone()).unwrap();
        assert_eq!(same.object_ref(), indirect.object_ref());
    }

    #[test]
    fn transform_annotations_from_copies_a_foreign_annotation_before_transforming_it() {
        let mut source = empty_pdf();
        source.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Link".to_vec())),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(1),
                        Object::Integer(2),
                        Object::Integer(11),
                        Object::Integer(22),
                    ]),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(10, 0),
            Object::Array(vec![Object::Reference(ObjectRef::new(9, 0))]),
        );
        let source_annots = source.get_object_handle(ObjectRef::new(10, 0));
        let source_annotation = source.get_object_handle(ObjectRef::new(9, 0));

        let mut target = empty_pdf();
        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(
                source_annots,
                Matrix::new(2.0, 0.0, 0.0, 2.0, 3.0, 4.0),
                &mut source,
            )
            .unwrap();

        let copied = &transformed.new_annotations[0];
        assert_ne!(copied.object_ref(), source_annotation.object_ref());
        let rect = copied
            .try_get_key(b"/Rect")
            .unwrap()
            .try_as_array()
            .unwrap()
            .unwrap();
        let coordinates: Vec<_> = rect
            .iter()
            .map(|item| {
                item.as_integer()
                    .map(|value| value as f64)
                    .or_else(|| item.as_real())
            })
            .collect();
        assert_eq!(
            coordinates,
            vec![Some(5.0), Some(8.0), Some(25.0), Some(48.0)]
        );
    }

    #[test]
    fn foreign_transform_pins_source_acroform_da_and_q_when_destination_defaults_differ() {
        let mut source = empty_pdf();
        source.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[
                        ("Fields", refs(&[7])),
                        ("DA", Object::String(b"/Fsrc 10 Tf".to_vec())),
                        ("Q", Object::Integer(1)),
                    ])),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        source.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("FT", Object::Name(b"Tx".to_vec())),
                ("T", Object::String(b"field".to_vec())),
                ("Kids", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(10),
                        Object::Integer(20),
                        Object::Integer(30),
                        Object::Integer(40),
                    ]),
                ),
            ])),
        );
        let source_annots = source
            .get_object_handle(ObjectRef::new(3, 0))
            .try_get_key(b"/Annots")
            .unwrap();

        let mut target = empty_pdf();
        target.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[(
                "AcroForm",
                Object::Dictionary(dict(&[
                    ("Fields", Object::Array(Vec::new())),
                    ("DA", Object::String(b"/Fdst 10 Tf".to_vec())),
                    ("Q", Object::Integer(2)),
                ])),
            )])),
        );

        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(source_annots, Matrix::default(), &mut source)
            .unwrap();
        let copied_field = &transformed.new_fields[0];
        assert_eq!(
            copied_field.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc 10 Tf".to_vec())
        );
        assert_eq!(
            copied_field.try_get_key(b"/Q").unwrap().as_integer(),
            Some(1)
        );
    }

    #[test]
    fn foreign_transform_pins_defaults_on_a_merged_terminal_field_without_kids() {
        // qpdf's `if (kids.isArray()) { ... }` (`QPDFAcroFormDocumentHelper.cc:900-909`)
        // is a plain conditional, not an early exit -- a merged field/widget
        // with no /Kids array still falls through to the unconditional
        // `adjustInheritedFields` call (`:914-917`). The other DA/Q test
        // above only exercises an interior node that HAS /Kids, which does
        // not distinguish the two.
        let mut source = empty_pdf();
        source.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[
                        ("Fields", refs(&[7])),
                        ("DA", Object::String(b"/Fsrc 10 Tf".to_vec())),
                        ("Q", Object::Integer(1)),
                    ])),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        source.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[7])),
            ])),
        );
        source.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("FT", Object::Name(b"Tx".to_vec())),
                ("T", Object::String(b"field".to_vec())),
                ("Subtype", Object::Name(b"Widget".to_vec())),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(10),
                        Object::Integer(20),
                        Object::Integer(30),
                        Object::Integer(40),
                    ]),
                ),
            ])),
        );
        let source_annots = source
            .get_object_handle(ObjectRef::new(3, 0))
            .try_get_key(b"/Annots")
            .unwrap();

        let mut target = empty_pdf();
        target.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[(
                "AcroForm",
                Object::Dictionary(dict(&[
                    ("Fields", Object::Array(Vec::new())),
                    ("DA", Object::String(b"/Fdst 10 Tf".to_vec())),
                    ("Q", Object::Integer(2)),
                ])),
            )])),
        );

        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(source_annots, Matrix::default(), &mut source)
            .unwrap();
        let copied_field = &transformed.new_fields[0];
        assert!(
            copied_field
                .try_get_key(b"/Kids")
                .unwrap()
                .try_as_array()
                .unwrap()
                .is_none(),
            "fixture must stay a merged terminal field with no /Kids"
        );
        assert_eq!(
            copied_field.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc 10 Tf".to_vec()),
            "a terminal field with no /Kids must still receive the pinned source default"
        );
        assert_eq!(
            copied_field.try_get_key(b"/Q").unwrap().as_integer(),
            Some(1)
        );
    }

    #[test]
    fn foreign_transform_merges_dr_rewrites_field_da_and_resets_field_dr() {
        let mut source = empty_pdf();
        source.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[
                        ("Fields", refs(&[7])),
                        ("DA", Object::String(b"/Fsrc 10 Tf".to_vec())),
                        ("DR", Object::Reference(ObjectRef::new(10, 0))),
                    ])),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        source.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"field".to_vec())),
                ("DR", Object::Reference(ObjectRef::new(10, 0))),
                ("Kids", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(10),
                        Object::Integer(20),
                        Object::Integer(30),
                        Object::Integer(40),
                    ]),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(10, 0),
            Object::Dictionary(dict(&[(
                "Font",
                Object::Dictionary(dict(&[("Fsrc", Object::Integer(1))])),
            )])),
        );

        let source_annots = source
            .get_object_handle(ObjectRef::new(3, 0))
            .try_get_key(b"/Annots")
            .unwrap();
        let mut target = empty_pdf();
        target.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[
                        ("Fields", Object::Array(Vec::new())),
                        ("DA", Object::String(b"/Fdst 10 Tf".to_vec())),
                        ("DR", Object::Reference(ObjectRef::new(10, 0))),
                    ])),
                ),
            ])),
        );
        target.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );
        target.set_object(
            ObjectRef::new(10, 0),
            Object::Dictionary(dict(&[(
                "Font",
                Object::Dictionary(dict(&[("Fsrc", Object::Integer(2))])),
            )])),
        );

        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(source_annots, Matrix::default(), &mut source)
            .unwrap();
        let copied_field = &transformed.new_fields[0];
        assert_eq!(
            copied_field.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc_1 10 Tf".to_vec())
        );
        assert_eq!(
            copied_field.try_get_key(b"/DR").unwrap().object_ref(),
            target
                .get_object_handle(ObjectRef::new(1, 0))
                .try_get_key(b"/AcroForm")
                .unwrap()
                .try_get_key(b"/DR")
                .unwrap()
                .object_ref()
        );
        let target_dr = target
            .get_object_handle(ObjectRef::new(10, 0))
            .try_get_key(b"/Font")
            .unwrap();
        assert!(target_dr
            .try_get_key(b"/Fsrc_1")
            .unwrap()
            .as_integer()
            .is_some());
    }

    #[test]
    fn foreign_transform_resets_field_dr_even_when_source_has_no_document_level_dr() {
        // qpdf's `init_dr_map` (`QPDFAcroFormDocumentHelper.cc:772-800`) is
        // gated only on there being a field to copy, not on the source
        // having a document-level `/DR`: `from_dr` stays null and
        // `mergeResources` no-ops on it, but the destination `/AcroForm/DR`
        // still gets created/promoted, and the copied field's own `/DR` (if
        // it has one) still gets reset to point at it
        // (`QPDFAcroFormDocumentHelper.cc:928-930`, unconditional on
        // `dr_map.empty()`).
        let mut source = empty_pdf();
        source.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        source.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                ("Annots", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("T", Object::String(b"field".to_vec())),
                // The field carries its own /DR even though the source
                // document's /AcroForm has none at all.
                ("DR", Object::Reference(ObjectRef::new(10, 0))),
                ("Kids", refs(&[9])),
            ])),
        );
        source.set_object(
            ObjectRef::new(9, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(7, 0))),
                (
                    "Rect",
                    Object::Array(vec![
                        Object::Integer(10),
                        Object::Integer(20),
                        Object::Integer(30),
                        Object::Integer(40),
                    ]),
                ),
            ])),
        );
        source.set_object(
            ObjectRef::new(10, 0),
            Object::Dictionary(dict(&[(
                "Font",
                Object::Dictionary(dict(&[("Fsrc", Object::Integer(1))])),
            )])),
        );

        let source_annots = source
            .get_object_handle(ObjectRef::new(3, 0))
            .try_get_key(b"/Annots")
            .unwrap();
        let mut target = empty_pdf();
        target.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    // The destination also has no /DR yet -- it must be
                    // created, not left absent.
                    Object::Dictionary(dict(&[("Fields", Object::Array(Vec::new()))])),
                ),
            ])),
        );
        target.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(Vec::new())),
                ("Count", Object::Integer(0)),
            ])),
        );

        let transformed = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .transform_annotations_from(source_annots, Matrix::default(), &mut source)
            .unwrap();
        let copied_field = &transformed.new_fields[0];
        let target_dr = target
            .get_object_handle(ObjectRef::new(1, 0))
            .try_get_key(b"/AcroForm")
            .unwrap()
            .try_get_key(b"/DR")
            .unwrap();
        assert!(
            target_dr.object_ref().is_some(),
            "destination /AcroForm/DR must be created even when the source has none"
        );
        assert_eq!(
            copied_field.try_get_key(b"/DR").unwrap().object_ref(),
            target_dr.object_ref(),
            "the copied field's own /DR must be reset to the destination's canonical /DR"
        );
    }

    #[test]
    fn copy_field_tree_rejects_stream_kids() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Kids", refs(&[6, 7]))])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Stream(Stream::new(Dictionary::new(), Vec::new())),
        );
        pdf.set_object(ObjectRef::new(7, 0), Object::Dictionary(Dictionary::new()));

        let top = pdf.get_object_handle(ObjectRef::new(5, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let error = helper
            .copy_field_tree(&top, &mut HashMap::new())
            .expect_err("qpdf stream cloning must fail");
        assert!(matches!(
            error,
            Error::System(ref message) if message == "stream objects cannot be cloned"
        ));
    }

    #[test]
    fn foreign_field_resource_adjustment_handles_empty_and_invalid_da() {
        let mut pdf = empty_pdf();
        let destination_resources = ObjectHandle::dictionary(Vec::new());
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Font".to_vec())
            .or_default()
            .insert(b"Fsrc".to_vec(), b"Fsrc_1".to_vec());
        let plan = ForeignResourcePlan {
            destination_resources: destination_resources.clone(),
            renames,
        };
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();

        let field = ObjectHandle::dictionary(vec![
            (b"/DR".to_vec(), ObjectHandle::dictionary(Vec::new())),
            (
                b"/DA".to_vec(),
                ObjectHandle::string(b"/Fsrc 10 Tf".to_vec()),
            ),
        ]);
        helper
            .adjust_foreign_field_resources(&field, &plan)
            .unwrap();
        assert!(field
            .try_get_key(b"/DR")
            .unwrap()
            .is_same_object_as(&destination_resources));
        assert_eq!(
            field.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc_1 10 Tf".to_vec())
        );

        let empty_plan = ForeignResourcePlan {
            destination_resources: destination_resources.clone(),
            renames: ResourceRenames::new(),
        };
        let unchanged = ObjectHandle::dictionary(vec![(
            b"/DA".to_vec(),
            ObjectHandle::string(b"/Fsrc 10 Tf".to_vec()),
        )]);
        helper
            .adjust_foreign_field_resources(&unchanged, &empty_plan)
            .unwrap();
        assert_eq!(
            unchanged.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc 10 Tf".to_vec())
        );

        let invalid = ObjectHandle::dictionary(vec![(
            b"/DA".to_vec(),
            ObjectHandle::string(b"/Fsrc 10 Tf [".to_vec()),
        )]);
        helper
            .adjust_foreign_field_resources(&invalid, &plan)
            .unwrap();
        assert_eq!(
            invalid.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc 10 Tf [".to_vec())
        );
    }

    #[test]
    fn adjust_foreign_field_resources_decodes_a_utf16_da_before_rewriting() {
        // qpdf's `adjustDefaultAppearances` decodes `/DA` via `getUTF8Value()`
        // before tokenizing it (`QPDFAcroFormDocumentHelper.cc:596`), then
        // writes the filtered result back as raw bytes via `newString`
        // (`:609`), not re-encoded through `newUnicodeString`. A `/DA` stored
        // as UTF-16BE (the qpdf-oracle-mandatory case for non-PDFDocEncoded
        // text) must decode correctly before the resource-name tokenizer
        // ever sees it.
        let mut pdf = empty_pdf();
        let destination_resources = ObjectHandle::dictionary(Vec::new());
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Font".to_vec())
            .or_default()
            .insert(b"Fsrc".to_vec(), b"Fsrc_1".to_vec());
        let plan = ForeignResourcePlan {
            destination_resources,
            renames,
        };
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();

        // UTF-16BE (with BOM) encoding of "/Fsrc 10 Tf".
        let utf16_da: Vec<u8> = vec![
            0xfe, 0xff, 0x00, 0x2f, 0x00, 0x46, 0x00, 0x73, 0x00, 0x72, 0x00, 0x63, 0x00, 0x20,
            0x00, 0x31, 0x00, 0x30, 0x00, 0x20, 0x00, 0x54, 0x00, 0x66,
        ];
        let field =
            ObjectHandle::dictionary(vec![(b"/DA".to_vec(), ObjectHandle::string(utf16_da))]);
        helper
            .adjust_foreign_field_resources(&field, &plan)
            .unwrap();
        assert_eq!(
            field.try_get_key(b"/DA").unwrap().as_string(),
            Some(b"/Fsrc_1 10 Tf".to_vec()),
            "a UTF-16BE-encoded /DA must decode before rewriting, not corrupt the tokenizer input"
        );
    }

    #[test]
    fn canonical_acroform_resources_reuse_indirect_promote_direct_or_create() {
        let mut pdf = empty_pdf();
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let indirect_dr = helper
            .pdf
            .make_indirect_object_handle(ObjectHandle::dictionary(Vec::new()))
            .unwrap();
        let root_seed = helper.pdf.get_object_handle(ObjectRef::new(1, 0));
        let root = helper.pdf.resolve_to_terminal(&root_seed).unwrap();
        root.replace_key(
            b"/AcroForm",
            ObjectHandle::dictionary(vec![(b"/DR".to_vec(), indirect_dr.clone())]),
        )
        .unwrap();
        let reused = helper.canonical_get_or_create_acroform_resources().unwrap();
        assert_eq!(reused.object_ref(), indirect_dr.object_ref());
        drop(helper);

        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[(
                "AcroForm",
                Object::Dictionary(dict(&[("DR", Object::Dictionary(Dictionary::new()))])),
            )])),
        );
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let promoted = helper.canonical_get_or_create_acroform_resources().unwrap();
        assert!(promoted.object_ref().is_some());
        drop(helper);

        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[(
                "AcroForm",
                Object::Dictionary(dict(&[("Fields", Object::Array(Vec::new()))])),
            )])),
        );
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let created = helper.canonical_get_or_create_acroform_resources().unwrap();
        assert!(created.object_ref().is_some());
        assert!(created.as_dictionary().is_some());
    }

    #[test]
    fn prepare_foreign_resource_plan_indirectizes_both_dr_second_level_values() {
        let mut target = empty_pdf();
        let target_root = target.get_object_handle(ObjectRef::new(1, 0));
        target_root.try_dereference().unwrap();
        target_root
            .replace_key(
                b"/AcroForm",
                ObjectHandle::dictionary(vec![(
                    b"/DR".to_vec(),
                    ObjectHandle::dictionary(vec![(
                        b"/Font".to_vec(),
                        ObjectHandle::dictionary(vec![(
                            b"/F0".to_vec(),
                            ObjectHandle::integer(10),
                        )]),
                    )]),
                )]),
            )
            .unwrap();

        let source_resources = ObjectHandle::dictionary(vec![(
            b"/Font".to_vec(),
            ObjectHandle::dictionary(vec![(b"/F1".to_vec(), ObjectHandle::integer(11))]),
        )]);
        let mut source = empty_pdf();
        let plan = AcroFormDocumentHelper::new(&mut target)
            .unwrap()
            .prepare_foreign_resource_plan(Some(source_resources), &mut source)
            .unwrap();

        let font = plan.destination_resources.try_get_key(b"/Font").unwrap();
        assert!(
            font.is_direct(),
            "qpdf keeps the category dictionary direct"
        );
        assert!(font.try_get_key(b"/F0").unwrap().is_indirect());
        assert!(font.try_get_key(b"/F1").unwrap().is_indirect());
    }

    #[test]
    fn inherited_default_helpers_walk_parents_and_stop_cycles() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[
                ("DA", Object::String(b"/Fparent 10 Tf".to_vec())),
                ("Q", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(5, 0)))])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(8, 0)))])),
        );
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[("Parent", Object::Reference(ObjectRef::new(7, 0)))])),
        );
        let parented = pdf.get_object_handle(ObjectRef::new(6, 0));
        let cycle = pdf.get_object_handle(ObjectRef::new(7, 0));
        let leaf = ObjectHandle::dictionary(Vec::new());

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        assert!(helper.field_has_explicit_value(&parented, b"/DA").unwrap());
        assert!(!helper
            .field_has_explicit_value(&parented, b"/Missing")
            .unwrap());
        assert!(!helper.field_has_explicit_value(&leaf, b"/DA").unwrap());
        assert!(!helper.field_has_explicit_value(&cycle, b"/DA").unwrap());
        assert_eq!(
            helper.effective_field_appearance(&parented).unwrap(),
            b"/Fparent 10 Tf".to_vec()
        );
        assert_eq!(helper.effective_field_quadding(&parented).unwrap(), 1);
        assert!(helper
            .effective_field_appearance(&cycle)
            .unwrap()
            .is_empty());
        assert_eq!(helper.effective_field_quadding(&cycle).unwrap(), 0);
    }

    #[test]
    fn effective_field_appearance_stops_at_non_string_inheritable_value() {
        // qpdf's getInheritableFieldValue stops at the first non-null value,
        // while getDefaultAppearance falls back to AcroForm /DA when that
        // value is not a string (QPDFFormFieldObjectHelper.cc:66-84, 197-210).
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Catalog".to_vec())),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("DA", Object::String(b"/FAcro 10 Tf".to_vec()))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[
                ("DA", Object::Integer(7)),
                ("Parent", Object::Reference(ObjectRef::new(6, 0))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("DA", Object::String(b"/Fparent 10 Tf".to_vec()))])),
        );

        let field = pdf.get_object_handle(ObjectRef::new(5, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        assert_eq!(
            helper.effective_field_appearance(&field).unwrap(),
            b"/FAcro 10 Tf".to_vec()
        );
    }

    #[test]
    fn effective_field_quadding_stops_at_non_integer_inheritable_value() {
        // qpdf's getQuadding falls back to AcroForm /Q when the first
        // non-null inheritable value is not an integer
        // (QPDFFormFieldObjectHelper.cc:66-84, 214-227).
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Catalog".to_vec())),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Q", Object::Integer(2))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[
                ("Q", Object::String(b"wrong-type".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(6, 0))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[("Q", Object::Integer(1))])),
        );

        let field = pdf.get_object_handle(ObjectRef::new(5, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        assert_eq!(helper.effective_field_quadding(&field).unwrap(), 2);
    }

    #[test]
    fn effective_field_defaults_inherit_through_direct_and_indirect_null_values() {
        // QPDF_Dictionary::hasKey uses QPDFObjectHandle::isNull, so a direct
        // null and an indirect reference resolving to null are both absent
        // for field inheritance (QPDF_Dictionary.cc:98-101;
        // QPDFObjectHandle.cc:353-356).
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[
                ("DA", Object::Null),
                ("Q", Object::Null),
                ("Parent", Object::Reference(ObjectRef::new(6, 0))),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(6, 0),
            Object::Dictionary(dict(&[
                ("DA", Object::String(b"/Fparent 10 Tf".to_vec())),
                ("Q", Object::Integer(1)),
            ])),
        );
        pdf.set_object(ObjectRef::new(7, 0), Object::Null);
        pdf.set_object(
            ObjectRef::new(8, 0),
            Object::Dictionary(dict(&[
                ("DA", Object::Reference(ObjectRef::new(7, 0))),
                ("Q", Object::Reference(ObjectRef::new(7, 0))),
                ("Parent", Object::Reference(ObjectRef::new(6, 0))),
            ])),
        );

        let direct_null = pdf.get_object_handle(ObjectRef::new(5, 0));
        let indirect_null = pdf.get_object_handle(ObjectRef::new(8, 0));
        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        for field in [&direct_null, &indirect_null] {
            assert_eq!(
                helper.effective_field_appearance(field).unwrap(),
                b"/Fparent 10 Tf".to_vec()
            );
            assert_eq!(helper.effective_field_quadding(field).unwrap(), 1);
        }
    }

    #[test]
    fn get_field_for_annotation_non_widget_returns_none_without_traversal() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(5, 0),
            Object::Dictionary(dict(&[("Subtype", Object::Name(b"Link".to_vec()))])),
        );
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .unwrap()
                .get_field_for_annotation(ObjectRef::new(5, 0))
                .unwrap(),
            None
        );
    }

    #[test]
    fn get_field_for_annotation_missing_subtype_returns_none_without_traversal() {
        let mut pdf = empty_pdf();
        pdf.set_object(ObjectRef::new(5, 0), Object::Dictionary(dict(&[])));
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .unwrap()
                .get_field_for_annotation(ObjectRef::new(5, 0))
                .unwrap(),
            None
        );
    }

    #[test]
    fn get_field_for_annotation_looks_up_the_map() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Name(b"Widget".to_vec())),
                ("FT", Object::Name(b"Tx".to_vec())),
            ])),
        );
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .unwrap()
                .get_field_for_annotation(ObjectRef::new(7, 0))
                .unwrap(),
            Some(ObjectRef::new(7, 0))
        );
    }

    #[test]
    fn get_field_for_annotation_resolves_an_indirect_subtype() {
        // qpdf's `getFieldForAnnotation` gate
        // (`isDictionaryOfType("", "/Widget")`) transparently dereferences
        // `getKey("/Subtype")`, so a /Subtype stored as an indirect reference
        // must classify as a widget the same as a direct one.
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", refs(&[7]))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", Object::Array(vec![])),
                ("Count", Object::Integer(0)),
            ])),
        );
        pdf.set_object(ObjectRef::new(9, 0), Object::Name(b"Widget".to_vec()));
        pdf.set_object(
            ObjectRef::new(7, 0),
            Object::Dictionary(dict(&[
                ("Subtype", Object::Reference(ObjectRef::new(9, 0))),
                ("FT", Object::Name(b"Tx".to_vec())),
            ])),
        );
        assert_eq!(
            AcroFormDocumentHelper::new(&mut pdf)
                .unwrap()
                .get_field_for_annotation(ObjectRef::new(7, 0))
                .unwrap(),
            Some(ObjectRef::new(7, 0))
        );
    }

    #[test]
    fn canonical_field_for_annotation_ignores_a_direct_widget_handle() {
        let mut pdf = empty_pdf();
        let annotation = ObjectHandle::dictionary(vec![(
            b"/Subtype".to_vec(),
            ObjectHandle::name(b"Widget".to_vec()),
        )]);

        assert!(AcroFormDocumentHelper::new(&mut pdf)
            .unwrap()
            .canonical_field_for_annotation(annotation)
            .unwrap()
            .is_none());
    }

    #[test]
    fn canonical_annotation_handles_include_a_direct_orphan_widget() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", Object::Array(Vec::new()))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "Annots",
                    Object::Array(vec![Object::Dictionary(dict(&[
                        ("Type", Object::Name(b"Annot".to_vec())),
                        ("Subtype", Object::Name(b"Widget".to_vec())),
                    ]))]),
                ),
            ])),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        let associations = helper
            .canonical_annotation_to_field_handles()
            .expect("canonical handle association analysis");
        let (annotation, field) = associations
            .iter()
            .find(|(annotation, _)| annotation.is_direct())
            .expect("the direct page Widget must be retained");
        assert!(annotation.is_same_object_as(field));
        assert!(helper
            .form_field_handles()
            .expect("legacy form-field projection")
            .is_empty());
    }

    #[test]
    fn canonical_annotation_cache_requires_explicit_invalidation_after_page_mutation() {
        let mut pdf = empty_pdf();
        pdf.set_object(
            ObjectRef::new(1, 0),
            Object::Dictionary(dict(&[
                ("Pages", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "AcroForm",
                    Object::Dictionary(dict(&[("Fields", Object::Array(Vec::new()))])),
                ),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(2, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Pages".to_vec())),
                ("Kids", refs(&[3])),
                ("Count", Object::Integer(1)),
            ])),
        );
        pdf.set_object(
            ObjectRef::new(3, 0),
            Object::Dictionary(dict(&[
                ("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(ObjectRef::new(2, 0))),
                (
                    "Annots",
                    Object::Array(vec![Object::Dictionary(dict(&[
                        ("Type", Object::Name(b"Annot".to_vec())),
                        ("Subtype", Object::Name(b"Widget".to_vec())),
                    ]))]),
                ),
            ])),
        );

        let mut helper = AcroFormDocumentHelper::new(&mut pdf).unwrap();
        assert_eq!(
            helper
                .canonical_annotation_to_field_handles()
                .unwrap()
                .len(),
            1
        );

        let page = helper.pdf.get_object_handle(ObjectRef::new(3, 0));
        page.replace_key(b"/Annots", ObjectHandle::array(Vec::new()))
            .unwrap();

        // qpdf deliberately keeps the analyzed association cache stable until
        // invalidateCache() is called by the mutating owner.
        assert_eq!(
            helper
                .canonical_annotation_to_field_handles()
                .unwrap()
                .len(),
            1
        );
        helper.invalidate_cache();
        assert!(helper
            .canonical_annotation_to_field_handles()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn adjust_copied_appearance_resources_ignores_a_direct_stream_guard() {
        let mut pdf = empty_pdf();
        let copied = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![(b"/Length".to_vec(), ObjectHandle::integer(0))]),
            Rc::new(Vec::new()),
        );
        let mut renames = ResourceRenames::new();
        renames
            .entry(b"Font".to_vec())
            .or_default()
            .insert(b"Fsrc".to_vec(), b"Fdst".to_vec());
        adjust_copied_appearance_resources(&mut pdf, &copied, Some(&renames)).unwrap();
    }
}
