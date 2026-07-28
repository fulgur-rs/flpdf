//! qpdf correspondence: JSON.cc incremental serialization and blob Base64 responsibilities.

use crate::pipeline::{Base64Action, Pipeline, PipelineResult, PlBase64, PlConcatenate, PlString};

use super::value::{ContainerOrBlobSnapshot, ValueSnapshot};
use super::Json;

impl Json {
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
        out.write(b"\"")?;
        out.write(encoded_key)?;
        out.write(b"\": ")
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
        if *first {
            *first = false;
            out.write(b"\n")?;
        } else {
            out.write(b",\n")?;
        }
        write_indent(out, depth)
    }

    pub fn write(&self, out: &mut dyn Pipeline, depth: usize) -> PipelineResult<()> {
        match self.value_snapshot() {
            None => out.write(b"null"),
            Some(ValueSnapshot::Number(value)) => out.write(&value),
            Some(ValueSnapshot::Bool(value)) => out.write(if value { b"true" } else { b"false" }),
            Some(ValueSnapshot::Null) => out.write(b"null"),
            Some(ValueSnapshot::String(encoded)) => {
                out.write(b"\"")?;
                out.write(&encoded)?;
                out.write(b"\"")
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
        out.write(b"\n")?;
        write_indent(out, depth)?;
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

fn write_indent(out: &mut dyn Pipeline, depth: usize) -> PipelineResult<()> {
    for _ in 0..depth {
        out.write(b"  ")?;
    }
    Ok(())
}
