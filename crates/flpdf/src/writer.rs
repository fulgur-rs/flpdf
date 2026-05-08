use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, Write};

pub fn write_pdf<R: Read + Seek, W: Write>(pdf: &mut Pdf<R>, mut out: W) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Err(crate::Error::Missing("/Root"));
    };

    let linearized_hint = pdf.linearized_hint_ref()?;

    let mut queue = VecDeque::new();
    let mut old_to_new = BTreeMap::from([(root_ref, root_ref)]);
    queue.push_back(root_ref);

    if let Some(linearized_ref) = linearized_hint {
        if old_to_new.insert(linearized_ref, linearized_ref).is_none() {
            queue.push_back(linearized_ref);
        }
    }

    let mut objects = Vec::new();
    let mut offsets = BTreeMap::new();

    while let Some(old_ref) = queue.pop_front() {
        let object = pdf.resolve(old_ref)?;
        collect_refs(&object, &mut old_to_new, &mut queue);
        objects.push((old_ref, object));
    }

    objects.sort_by_key(|(object_ref, _)| object_ref.number);

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");

    for (old_ref, object) in &objects {
        let new_ref = old_to_new[old_ref];
        offsets.insert(new_ref.number, bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", new_ref.number).as_bytes());
        let rewritten = rewrite_refs(object, &old_to_new);
        rewritten.write_pdf(&mut bytes);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    let object_count = objects
        .iter()
        .map(|(object_ref, _)| object_ref.number)
        .max()
        .unwrap_or(0)
        .saturating_add(1) as usize;

    bytes.extend_from_slice(format!("xref\n0 {}\n", object_count).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..object_count {
        match offsets.get(&(number as u32)) {
            Some(offset) => bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }

    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer(object_count as i64));
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
                old_to_new.insert(*object_ref, *object_ref);
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
