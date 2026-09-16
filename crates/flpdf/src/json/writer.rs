//! Writers leave the caller-supplied outer pipeline unfinished. Blob
//! serialization finishes only its internally owned Base64 stage.
//!
//! qpdf correspondence: JSON.cc incremental serialization and blob Base64 responsibilities.
//!
//!

use crate::pipeline::{Base64Action, Pipeline, PipelineResult, PlBase64, PlConcatenate, PlString};
use crate::qpdf_obj_gen::QpdfObjGen;

use super::value::{ContainerOrBlobSnapshot, ValueSnapshot};
use super::Json;

impl Json {
    const DICTIONARY_KEY_STACK_CAPACITY: usize = 256;

    pub fn write_dictionary_open(
        out: &mut dyn Pipeline,
        first: &mut bool,
        _depth: usize,
    ) -> PipelineResult<()> {
        out.write(b"{")?;
        *first = true;
        Ok(())
    }

    pub fn write_array_open(
        out: &mut dyn Pipeline,
        first: &mut bool,
        _depth: usize,
    ) -> PipelineResult<()> {
        out.write(b"[")?;
        *first = true;
        Ok(())
    }

    pub fn write_dictionary_close(
        out: &mut dyn Pipeline,
        first: bool,
        depth: usize,
    ) -> PipelineResult<()> {
        write_close(out, first, depth, b"}")
    }

    pub fn write_array_close(
        out: &mut dyn Pipeline,
        first: bool,
        depth: usize,
    ) -> PipelineResult<()> {
        write_close(out, first, depth, b"]")
    }

    pub fn write_dictionary_item(
        out: &mut dyn Pipeline,
        first: &mut bool,
        key: &[u8],
        value: &Json,
        depth: usize,
    ) -> PipelineResult<()> {
        Self::write_dictionary_key(out, first, key, depth)?;
        value.write(out, depth)
    }

    pub fn write_dictionary_key(
        out: &mut dyn Pipeline,
        first: &mut bool,
        encoded_key: &[u8],
        depth: usize,
    ) -> PipelineResult<()> {
        Self::write_next(out, first, depth)?;
        let required = encoded_key.len() + 4;
        if required <= Self::DICTIONARY_KEY_STACK_CAPACITY {
            let mut item = [0u8; Self::DICTIONARY_KEY_STACK_CAPACITY];
            item[0] = b'"';
            item[1..encoded_key.len() + 1].copy_from_slice(encoded_key);
            item[encoded_key.len() + 1..required].copy_from_slice(b"\": ");
            out.write(&item[..required])
        } else {
            let mut item = Vec::with_capacity(required);
            item.push(b'"');
            item.extend_from_slice(encoded_key);
            item.extend_from_slice(b"\": ");
            out.write(&item)
        }
    }

    /// Write a qpdf v2 object-map key without allocating its decimal spelling.
    ///
    /// `QPDF::writeJSON` constructs `obj:N G R` from the already-known
    /// `QPDFObjGen` and writes it through the JSON pipeline
    /// (`libqpdf/QPDF_json.cc:891-922`). The complete key fits in this stack
    /// buffer for the signed i32 object/generation domain used by flpdf; the
    /// outer dictionary-key helper retains the same single-chunk framing.
    pub(crate) fn write_qpdf_object_key(
        out: &mut dyn Pipeline,
        first: &mut bool,
        object_gen: QpdfObjGen,
        depth: usize,
    ) -> PipelineResult<()> {
        let mut key = [0u8; 32];
        let mut length = 0;
        key[..4].copy_from_slice(b"obj:");
        length += 4;
        append_decimal_i32(&mut key, &mut length, object_gen.get_obj());
        key[length] = b' ';
        length += 1;
        append_decimal_i32(&mut key, &mut length, object_gen.get_gen());
        key[length..length + 2].copy_from_slice(b" R");
        length += 2;
        Self::write_dictionary_key(out, first, &key[..length], depth)
    }

    pub fn write_array_item(
        out: &mut dyn Pipeline,
        first: &mut bool,
        value: &Json,
        depth: usize,
    ) -> PipelineResult<()> {
        Self::write_next(out, first, depth)?;
        value.write(out, depth)
    }

    pub fn write_next(
        out: &mut dyn Pipeline,
        first: &mut bool,
        depth: usize,
    ) -> PipelineResult<()> {
        let prefix = if *first {
            *first = false;
            b"\n".as_slice()
        } else {
            b",\n".as_slice()
        };
        write_indented(out, prefix, depth, b"")
    }

    pub fn write(&self, out: &mut dyn Pipeline, depth: usize) -> PipelineResult<()> {
        match self.value_snapshot() {
            None => out.write(b"null"),
            Some(ValueSnapshot::Number(value)) => out.write(&value),
            Some(ValueSnapshot::Bool(value)) => out.write(if value { b"true" } else { b"false" }),
            Some(ValueSnapshot::Null) => out.write(b"null"),
            Some(ValueSnapshot::String(encoded)) => {
                let mut quoted = Vec::with_capacity(encoded.len() + 2);
                quoted.push(b'"');
                quoted.extend_from_slice(&encoded);
                quoted.push(b'"');
                out.write(&quoted)
            }
            Some(other) => write_container_or_blob(
                self,
                other
                    .into_container_or_blob()
                    .expect("scalar values are handled by Json::write"),
                out,
                depth,
            ),
        }
    }

    pub fn unparse(&self) -> PipelineResult<Vec<u8>> {
        let mut bytes = Vec::new();
        {
            let mut output = PlString::new("unparse", None, &mut bytes);
            self.write(&mut output, 0)?;
        }
        Ok(bytes)
    }
}

fn write_close(
    out: &mut dyn Pipeline,
    first: bool,
    depth: usize,
    delimiter: &[u8],
) -> PipelineResult<()> {
    if !first {
        return write_indented(out, b"\n", depth, delimiter);
    }
    out.write(delimiter)
}

fn write_container_or_blob(
    owner: &Json,
    value: ContainerOrBlobSnapshot,
    out: &mut dyn Pipeline,
    depth: usize,
) -> PipelineResult<()> {
    match value {
        ContainerOrBlobSnapshot::Dictionary => {
            let mut first = true;
            Json::write_dictionary_open(out, &mut first, depth)?;
            let mut previous_key = None;
            while let Some((key, value)) = owner.next_dictionary_item_after(previous_key.as_deref())
            {
                let selected = value;
                Json::write_dictionary_key(out, &mut first, &key, depth + 1)?;
                let value = owner.dictionary_item_for_write(&key).unwrap_or(selected);
                value.write(out, depth + 1)?;
                previous_key = Some(key);
            }
            Json::write_dictionary_close(out, first, depth)
        }
        ContainerOrBlobSnapshot::Array => {
            let mut first = true;
            Json::write_array_open(out, &mut first, depth)?;
            let values = owner
                .array_items_snapshot()
                .expect("array tag was obtained from the same Json handle");
            for value in &values {
                Json::write_array_item(out, &mut first, value, depth + 1)?;
            }
            Json::write_array_close(out, first, depth)
        }
        ContainerOrBlobSnapshot::Blob(writer) => {
            out.write(b"\"")?;
            {
                let mut concatenate = PlConcatenate::new("blob concatenate", out);
                let mut base64 =
                    PlBase64::new("blob base64", &mut concatenate, Base64Action::Encode);
                writer(&mut base64)?;
                base64.finish()?;
            }
            out.write(b"\"")
        }
    }
}

fn write_indented(
    out: &mut dyn Pipeline,
    prefix: &[u8],
    depth: usize,
    suffix: &[u8],
) -> PipelineResult<()> {
    const STACK_CAPACITY: usize = 256;
    let indentation = depth.saturating_mul(2);
    let required = prefix
        .len()
        .saturating_add(indentation)
        .saturating_add(suffix.len());
    if required <= STACK_CAPACITY {
        let mut chunk = [0u8; STACK_CAPACITY];
        let mut offset = 0;
        chunk[offset..offset + prefix.len()].copy_from_slice(prefix);
        offset += prefix.len();
        for _ in 0..depth {
            chunk[offset..offset + 2].copy_from_slice(b"  ");
            offset += 2;
        }
        chunk[offset..offset + suffix.len()].copy_from_slice(suffix);
        out.write(&chunk[..required])
    } else {
        let mut chunk = Vec::with_capacity(required);
        chunk.extend_from_slice(prefix);
        for _ in 0..depth {
            chunk.extend_from_slice(b"  ");
        }
        chunk.extend_from_slice(suffix);
        out.write(&chunk)
    }
}

fn append_decimal_i32(buffer: &mut [u8], length: &mut usize, value: i32) {
    let mut digits = [0u8; 10];
    let mut digit_count = 0;
    let mut magnitude = value.unsigned_abs();
    loop {
        digits[digit_count] = b'0' + (magnitude % 10) as u8;
        digit_count += 1;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    if value < 0 {
        buffer[*length] = b'-';
        *length += 1;
    }
    for digit in digits[..digit_count].iter().rev() {
        buffer[*length] = *digit;
        *length += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{append_decimal_i32, write_indented, Json};
    use crate::pipeline::PlString;

    #[test]
    fn long_dictionary_keys_use_the_heap_fallback() {
        let key = vec![b'k'; Json::DICTIONARY_KEY_STACK_CAPACITY + 1];
        let mut bytes = Vec::new();
        {
            let mut output = PlString::new("json long key", None, &mut bytes);
            let mut first = true;
            Json::write_dictionary_key(&mut output, &mut first, &key, 0)
                .expect("long dictionary key should write");
        }
        assert_eq!(bytes.len(), key.len() + 5);
        assert!(bytes.starts_with(b"\n\""));
        assert!(bytes.ends_with(b"\": "));
    }

    #[test]
    fn deeply_indented_json_uses_the_heap_fallback() {
        let mut bytes = Vec::new();
        {
            let mut output = PlString::new("json deep indent", None, &mut bytes);
            write_indented(&mut output, b"\n", 200, b"}").expect("deep indentation should write");
        }
        assert_eq!(bytes.len(), 402);
        assert_eq!(bytes[0], b'\n');
        assert_eq!(&bytes[1..bytes.len() - 1], vec![b' '; 400].as_slice());
        assert_eq!(bytes[bytes.len() - 1], b'}');
    }

    #[test]
    fn json_object_generation_decimal_writer_handles_negative_values() {
        let mut buffer = [0u8; 32];
        let mut length = 0;
        append_decimal_i32(&mut buffer, &mut length, -123);
        assert_eq!(&buffer[..length], b"-123");
    }
}
