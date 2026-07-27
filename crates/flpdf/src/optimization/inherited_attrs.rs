//! qpdf correspondence: QPDF_optimization.cc inherited-page-attribute push.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object::{Object, ObjectRef};
use crate::pages::repair::PreparedPages;
use crate::ref_chain::terminal_ref_of_chain;
use crate::{Error, Pdf, Result};

const INHERITABLE_KEYS: [&[u8]; 4] = [b"CropBox", b"MediaBox", b"Resources", b"Rotate"];
const MAX_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

pub(crate) fn push<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    prepared: &PreparedPages,
    allow_changes: bool,
    warn_skipped_keys: bool,
) -> Result<()> {
    let mut key_ancestors: BTreeMap<&'static [u8], Vec<Object>> = BTreeMap::new();
    let mut visited = BTreeSet::new();
    push_internal(
        pdf,
        prepared.root,
        &mut key_ancestors,
        &mut visited,
        allow_changes,
        warn_skipped_keys,
        0,
    )?;
    debug_assert!(
        key_ancestors.values().all(Vec::is_empty),
        "key_ancestors not empty after pushing inherited attributes to pages"
    );
    Ok(())
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    visited: &mut BTreeSet<ObjectRef>,
    allow_changes: bool,
    warn_skipped_keys: bool,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        return Ok(());
    }

    let Object::Dictionary(mut dict) = pdf.resolve(node_ref)? else {
        return Ok(());
    };
    if !matches!(dict.get("Type"), Some(Object::Name(name)) if name == b"Pages") {
        return Ok(());
    }

    if warn_skipped_keys && dict.get("Parent").is_some() {
        let entries = crate::qpdf_null::snapshot_entries(&dict, false);
        for (key, _) in crate::qpdf_null::visible_entries(pdf, entries)? {
            if !INHERITABLE_KEYS.contains(&key.as_slice())
                && ![b"Type".as_slice(), b"Parent", b"Kids", b"Count"].contains(&key.as_slice())
            {
                pdf.push_warning(format!(
                    "Unknown key /{} in /Pages object is being discarded as a result of flattening the /Pages tree",
                    String::from_utf8_lossy(&key),
                ));
            }
        }
    }

    let mut own_keys = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict.remove(key) else {
            continue;
        };
        let is_null = match &value {
            Object::Null => true,
            Object::Reference(reference) => {
                let terminal = terminal_ref_of_chain(pdf, *reference)?;
                matches!(pdf.resolve_borrowed(terminal)?, Object::Null)
            }
            _ => false,
        };
        if is_null {
            dict.insert(key, value);
            continue;
        }
        if !allow_changes {
            return Err(Error::Unsupported(
                "optimize detected an inheritable attribute when called in no-change mode"
                    .to_owned(),
            ));
        }
        let value = match value {
            Object::Reference(_) => value,
            Object::Array(_) | Object::Dictionary(_) => {
                let new_ref = next_object_ref(pdf)?;
                pdf.set_object(new_ref, value);
                Object::Reference(new_ref)
            }
            scalar => scalar,
        };
        key_ancestors.entry(key).or_default().push(value);
        own_keys.push(key);
    }

    let kids = dict
        .get("Kids")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec);
    pdf.set_object(node_ref, Object::Dictionary(dict));

    if let Some(kids) = kids {
        for kid in kids {
            let Object::Reference(kid_ref) = kid else {
                continue;
            };
            let is_pages_node = matches!(
                pdf.resolve_borrowed(kid_ref)?,
                Object::Dictionary(dict)
                    if matches!(dict.get("Type"), Some(Object::Name(name)) if name == b"Pages")
            );
            if is_pages_node {
                push_internal(
                    pdf,
                    kid_ref,
                    key_ancestors,
                    visited,
                    allow_changes,
                    warn_skipped_keys,
                    depth + 1,
                )?;
                continue;
            }

            let Object::Dictionary(mut leaf) = pdf.resolve(kid_ref)? else {
                continue;
            };
            for (&key, values) in key_ancestors.iter() {
                let present = match leaf.get(key) {
                    None | Some(Object::Null) => false,
                    Some(Object::Reference(reference)) => {
                        let terminal = terminal_ref_of_chain(pdf, *reference)?;
                        !matches!(pdf.resolve_borrowed(terminal)?, Object::Null)
                    }
                    Some(_) => true,
                };
                if !present {
                    if let Some(value) = values.last() {
                        leaf.insert(key, value.clone());
                    }
                }
            }
            pdf.set_object(kid_ref, Object::Dictionary(leaf));
        }
    }

    for key in own_keys {
        if let Some(stack) = key_ancestors.get_mut(key) {
            stack.pop();
            if stack.is_empty() {
                key_ancestors.remove(key);
            }
        }
    }
    Ok(())
}

fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let number = pdf
        .object_refs()
        .into_iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_owned()))?;
    Ok(ObjectRef::new(number, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Object, Pdf};

    fn pdf_bytes(bodies: &[(u32, &[u8])]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0_u64; bodies.last().map_or(1, |(number, _)| *number as usize + 1)];
        for &(number, body) in bodies {
            offsets[number as usize] = pdf.len() as u64;
            pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len() as u64;
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.into_iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                bodies.last().map_or(1, |(number, _)| number + 1)
            )
            .as_bytes(),
        );
        pdf
    }

    fn pdf_with_inherited_scalar_rotate() -> Vec<u8> {
        pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>"),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
            ),
        ])
    }

    fn pdf_with_unknown_intermediate_pages_key() -> Vec<u8> {
        pdf_bytes(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>",
            ),
            (
                3,
                b"<< /Type /Pages /Parent 2 0 R /Kids [4 0 R] /Count 1 /Unknown 7 >>",
            ),
            (4, b"<< /Type /Page /Parent 3 0 R >>"),
        ])
    }

    #[test]
    fn no_change_mode_rejects_inheritable_key_before_mutation() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_inherited_scalar_rotate()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();
        let before = pdf.resolve(prepared.root).unwrap();

        let error = push(&mut pdf, &prepared, false, false).unwrap_err();

        assert!(error.to_string().contains("inheritable attribute"));
        assert_eq!(pdf.resolve(prepared.root).unwrap(), before);
    }

    #[test]
    fn warning_mode_reports_unknown_intermediate_pages_key() {
        let mut pdf = Pdf::open_mem_owned(pdf_with_unknown_intermediate_pages_key()).unwrap();
        let prepared = crate::pages::repair::prepare_for_optimization(&mut pdf)
            .unwrap()
            .unwrap();

        push(&mut pdf, &prepared, true, true).unwrap();

        assert!(pdf.repair_diagnostics().entries().iter().any(|diagnostic| {
            diagnostic.message.contains("Unknown key /Unknown")
                && diagnostic.message.contains("/Pages")
        }));
        assert!(matches!(
            pdf.resolve(prepared.pages[0]).unwrap(),
            Object::Dictionary(ref page) if page.get("MediaBox").is_some()
        ));
    }
}
