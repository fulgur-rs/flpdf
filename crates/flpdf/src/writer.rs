use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, Write};

pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let mut queue = VecDeque::from([root_ref]);
    let mut old_to_new = BTreeMap::from([(root_ref, ObjectRef::new(1, 0))]);
    let mut objects = Vec::new();

    while let Some(old_ref) = queue.pop_front() {
        let object = pdf.resolve(old_ref)?;
        collect_refs(&object, &mut old_to_new, &mut queue);
        objects.push((old_ref, object));
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (old_ref, object) in &objects {
        let new_ref = old_to_new[old_ref];
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", new_ref.number).as_bytes());
        let rewritten = rewrite_refs(object, &old_to_new);
        rewritten.write_pdf(&mut bytes);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer((objects.len() + 1) as i64));
    trailer.insert("Root", Object::Reference(old_to_new[&root_ref]));
    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(&mut bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

    out.write_all(&bytes)?;
    Ok(())
}

fn collect_refs(
    object: &Object,
    old_to_new: &mut BTreeMap<ObjectRef, ObjectRef>,
    queue: &mut VecDeque<ObjectRef>,
) {
    match object {
        Object::Reference(object_ref) => {
            if !old_to_new.contains_key(object_ref) {
                let next = ObjectRef::new((old_to_new.len() + 1) as u32, 0);
                old_to_new.insert(*object_ref, next);
                queue.push_back(*object_ref);
            }
        }
        Object::Array(values) => values
            .iter()
            .for_each(|value| collect_refs(value, old_to_new, queue)),
        Object::Dictionary(dict) => dict
            .iter()
            .for_each(|(_, value)| collect_refs(value, old_to_new, queue)),
        Object::Stream(stream) => stream
            .dict
            .iter()
            .for_each(|(_, value)| collect_refs(value, old_to_new, queue)),
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
}

fn rewrite_refs(object: &Object, old_to_new: &BTreeMap<ObjectRef, ObjectRef>) -> Object {
    match object {
        Object::Reference(object_ref) => old_to_new
            .get(object_ref)
            .copied()
            .map(Object::Reference)
            .unwrap_or(Object::Null),
        Object::Array(values) => Object::Array(
            values
                .iter()
                .map(|value| rewrite_refs(value, old_to_new))
                .collect(),
        ),
        Object::Dictionary(dict) => {
            let mut rewritten = Dictionary::new();
            for (key, value) in dict.iter() {
                rewritten.insert(key, rewrite_refs(value, old_to_new));
            }
            Object::Dictionary(rewritten)
        }
        Object::Stream(stream) => Object::Stream(crate::Stream::new(
            match rewrite_refs(&Object::Dictionary(stream.dict.clone()), old_to_new) {
                Object::Dictionary(dict) => dict,
                _ => Dictionary::new(),
            },
            stream.data.clone(),
        )),
        other => other.clone(),
    }
}
