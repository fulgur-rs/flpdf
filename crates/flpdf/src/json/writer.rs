use std::io::{self, Write};

use super::value::ValueSnapshot;
use super::Json;

impl Json {
    pub fn write(&self, out: &mut (impl Write + ?Sized), depth: usize) -> io::Result<()> {
        match self.value_snapshot() {
            None => out.write_all(b"null"),
            Some(ValueSnapshot::Number(value)) => out.write_all(&value),
            Some(ValueSnapshot::Bool(value)) => {
                out.write_all(if value { b"true" } else { b"false" })
            }
            Some(ValueSnapshot::Null) => out.write_all(b"null"),
            Some(ValueSnapshot::String(encoded)) => {
                out.write_all(b"\"")?;
                out.write_all(&encoded)?;
                out.write_all(b"\"")
            }
            Some(other) => write_container_or_blob(other, out, depth),
        }
    }

    pub fn unparse(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write(&mut out, 0)?;
        Ok(out)
    }
}

fn write_container_or_blob(
    value: ValueSnapshot,
    out: &mut (impl Write + ?Sized),
    depth: usize,
) -> io::Result<()> {
    match value {
        ValueSnapshot::Dictionary(members) => {
            if members.is_empty() {
                return out.write_all(b"{}");
            }
            out.write_all(b"{")?;
            for (index, (key, value)) in members.iter().enumerate() {
                out.write_all(b"\n")?;
                write_indent(out, depth + 1)?;
                out.write_all(b"\"")?;
                out.write_all(key)?;
                out.write_all(b"\": ")?;
                value.write(out, depth + 1)?;
                if index + 1 < members.len() {
                    out.write_all(b",")?;
                }
            }
            out.write_all(b"\n")?;
            write_indent(out, depth)?;
            out.write_all(b"}")
        }
        ValueSnapshot::Array(values) => {
            if values.is_empty() {
                return out.write_all(b"[]");
            }
            out.write_all(b"[")?;
            for (index, value) in values.iter().enumerate() {
                out.write_all(b"\n")?;
                write_indent(out, depth + 1)?;
                value.write(out, depth + 1)?;
                if index + 1 < values.len() {
                    out.write_all(b",")?;
                }
            }
            out.write_all(b"\n")?;
            write_indent(out, depth)?;
            out.write_all(b"]")
        }
        ValueSnapshot::Blob(_) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "JSON blob writing is not available yet",
        )),
        ValueSnapshot::String(_)
        | ValueSnapshot::Number(_)
        | ValueSnapshot::Bool(_)
        | ValueSnapshot::Null => unreachable!("scalar values are handled by Json::write"),
    }
}

fn write_indent(out: &mut (impl Write + ?Sized), depth: usize) -> io::Result<()> {
    for _ in 0..depth {
        out.write_all(b"  ")?;
    }
    Ok(())
}
