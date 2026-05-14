//! ObjStm eligibility predicate — decides whether an indirect object may be
//! stored inside an object stream (PDF 1.5+, ISO 32000-1 §7.5.7).

// These items are consumed by the upcoming ObjStm writer; suppress dead_code
// until that code lands.
#![allow(dead_code)]

use crate::object::{Dictionary, Object, ObjectRef};

// ── Public types ─────────────────────────────────────────────────────────────

/// Context resolved once per document, used to identify objects that must stay
/// outside any ObjStm.
pub(crate) struct EligibilityContext {
    /// The indirect reference of the encryption dictionary, if any.
    pub encryption_ref: Option<ObjectRef>,
    /// The indirect reference of the linearization parameter dictionary, if any.
    pub linearization_param_ref: Option<ObjectRef>,
}

// ── Predicate ────────────────────────────────────────────────────────────────

/// Returns `true` when the object identified by `object_ref` with body
/// `object` may be stored inside an ObjStm.
///
/// Disqualifying conditions (PDF spec + implementation constraints):
/// 1. `object_ref.generation != 0`  — ObjStm members must have generation 0.
/// 2. `object` is a [`Object::Stream`] — streams cannot be embedded in ObjStm.
/// 3. The object is a dictionary with `/Type /ObjStm` — no nested ObjStm.
/// 4. The object is a dictionary with `/Type /XRef` — xref streams must be direct.
/// 5. `object_ref` is the encryption dictionary reference.
/// 6. `object_ref` is the linearization parameter dictionary reference.
pub(crate) fn is_eligible_for_objstm(
    object_ref: ObjectRef,
    object: &Object,
    ctx: &EligibilityContext,
) -> bool {
    // 1. Generation must be 0.
    if object_ref.generation != 0 {
        return false;
    }

    // 2. Stream objects cannot be embedded.
    if matches!(object, Object::Stream(_)) {
        return false;
    }

    // 3 & 4. Check /Type for Dictionary objects.
    if let Object::Dictionary(dict) = object {
        if dict_type_is(dict, b"ObjStm") || dict_type_is(dict, b"XRef") {
            return false;
        }
    }

    // 5. Encryption dictionary must not be embedded.
    if Some(object_ref) == ctx.encryption_ref {
        return false;
    }

    // 6. Linearization parameter dictionary must not be embedded.
    if Some(object_ref) == ctx.linearization_param_ref {
        return false;
    }

    true
}

// ── Context builder ──────────────────────────────────────────────────────────

/// Build an [`EligibilityContext`] by querying `pdf` for the encryption and
/// linearization parameter references.  Must be called once before processing
/// any objects; the result is then used with [`is_eligible_for_objstm`] which
/// is a pure function.
pub(crate) fn eligibility_context<R: std::io::Read + std::io::Seek>(
    pdf: &mut crate::reader::Pdf<R>,
) -> crate::Result<EligibilityContext> {
    Ok(EligibilityContext {
        encryption_ref: pdf.encryption_ref(),
        linearization_param_ref: pdf.linearized_hint_ref()?,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns `true` when `dict` contains `/Type /<expected>`.
fn dict_type_is(dict: &Dictionary, expected: &[u8]) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(n)) if n.as_slice() == expected)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Dictionary, Stream};

    fn no_ctx() -> EligibilityContext {
        EligibilityContext {
            encryption_ref: None,
            linearization_param_ref: None,
        }
    }

    fn ref0(n: u32) -> ObjectRef {
        ObjectRef::new(n, 0)
    }

    fn ref1(n: u32) -> ObjectRef {
        ObjectRef::new(n, 1)
    }

    fn typed_dict(type_name: &[u8]) -> Object {
        let mut d = Dictionary::new();
        d.insert("Type", Object::Name(type_name.to_vec()));
        Object::Dictionary(d)
    }

    #[test]
    fn generation_one_is_ineligible() {
        let obj = Object::Null;
        assert!(!is_eligible_for_objstm(ref1(1), &obj, &no_ctx()));
    }

    #[test]
    fn stream_object_is_ineligible() {
        let obj = Object::Stream(Stream::new(Dictionary::new(), vec![]));
        assert!(!is_eligible_for_objstm(ref0(1), &obj, &no_ctx()));
    }

    #[test]
    fn objstm_typed_dict_is_ineligible() {
        let obj = typed_dict(b"ObjStm");
        assert!(!is_eligible_for_objstm(ref0(1), &obj, &no_ctx()));
    }

    #[test]
    fn xref_typed_dict_is_ineligible() {
        let obj = typed_dict(b"XRef");
        assert!(!is_eligible_for_objstm(ref0(1), &obj, &no_ctx()));
    }

    #[test]
    fn encryption_dict_ref_is_ineligible() {
        let ctx = EligibilityContext {
            encryption_ref: Some(ref0(5)),
            linearization_param_ref: None,
        };
        let obj = Object::Null;
        assert!(!is_eligible_for_objstm(ref0(5), &obj, &ctx));
    }

    #[test]
    fn linearization_param_dict_ref_is_ineligible() {
        let ctx = EligibilityContext {
            encryption_ref: None,
            linearization_param_ref: Some(ref0(7)),
        };
        let obj = Object::Null;
        assert!(!is_eligible_for_objstm(ref0(7), &obj, &ctx));
    }

    #[test]
    fn plain_page_dict_is_eligible() {
        let obj = typed_dict(b"Page");
        assert!(is_eligible_for_objstm(ref0(3), &obj, &no_ctx()));
    }

    #[test]
    fn plain_null_object_is_eligible() {
        let obj = Object::Null;
        assert!(is_eligible_for_objstm(ref0(10), &obj, &no_ctx()));
    }
}
