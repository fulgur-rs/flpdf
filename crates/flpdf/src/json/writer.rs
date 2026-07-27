//! qpdf correspondence: JSON.cc incremental serialization and blob Base64 responsibilities.

use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::value::{ContainerOrBlobSnapshot, ValueSnapshot};
use super::Json;

struct Base64Writer<'a, W: Write + ?Sized> {
    out: &'a mut W,
    pending: [u8; 3],
    pending_len: usize,
}

impl<'a, W: Write + ?Sized> Base64Writer<'a, W> {
    fn new(out: &'a mut W) -> Self {
        Self {
            out,
            pending: [0; 3],
            pending_len: 0,
        }
    }

    fn write_group(&mut self, group: &[u8; 3]) -> io::Result<()> {
        let mut encoded = [0; 4];
        STANDARD
            .encode_slice(group, &mut encoded)
            .expect("three input bytes always encode into four output bytes");
        self.out.write_all(&encoded)
    }

    fn finish(self) -> io::Result<()> {
        if self.pending_len != 0 {
            let encoded = STANDARD.encode(&self.pending[..self.pending_len]);
            self.out.write_all(encoded.as_bytes())?;
        }
        Ok(())
    }
}

impl<W: Write + ?Sized> Write for Base64Writer<'_, W> {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let input_len = input.len();

        if self.pending_len != 0 {
            let needed = 3 - self.pending_len;
            let copied = needed.min(input.len());
            self.pending[self.pending_len..self.pending_len + copied]
                .copy_from_slice(&input[..copied]);
            self.pending_len += copied;
            input = &input[copied..];
            if self.pending_len == 3 {
                let group = self.pending;
                self.write_group(&group)?;
                self.pending_len = 0;
            }
        }

        while input.len() >= 3 {
            let group: [u8; 3] = input[..3]
                .try_into()
                .expect("three-byte slice has fixed length");
            self.write_group(&group)?;
            input = &input[3..];
        }

        self.pending[..input.len()].copy_from_slice(input);
        self.pending_len = input.len();
        Ok(input_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Json {
    pub fn write_dictionary_open(
        out: &mut (impl Write + ?Sized),
        first: &mut bool,
        _depth: usize,
    ) -> io::Result<()> {
        out.write_all(b"{")?;
        *first = true;
        Ok(())
    }

    pub fn write_array_open(
        out: &mut (impl Write + ?Sized),
        first: &mut bool,
        _depth: usize,
    ) -> io::Result<()> {
        out.write_all(b"[")?;
        *first = true;
        Ok(())
    }

    pub fn write_dictionary_close(
        out: &mut (impl Write + ?Sized),
        first: bool,
        depth: usize,
    ) -> io::Result<()> {
        write_close(out, first, depth, b"}")
    }

    pub fn write_array_close(
        out: &mut (impl Write + ?Sized),
        first: bool,
        depth: usize,
    ) -> io::Result<()> {
        write_close(out, first, depth, b"]")
    }

    pub fn write_dictionary_item(
        out: &mut (impl Write + ?Sized),
        first: &mut bool,
        key: &[u8],
        value: &Json,
        depth: usize,
    ) -> io::Result<()> {
        Self::write_dictionary_key(out, first, key, depth)?;
        value.write(out, depth)
    }

    pub fn write_dictionary_key(
        out: &mut (impl Write + ?Sized),
        first: &mut bool,
        encoded_key: &[u8],
        depth: usize,
    ) -> io::Result<()> {
        Self::write_next(out, first, depth)?;
        out.write_all(b"\"")?;
        out.write_all(encoded_key)?;
        out.write_all(b"\": ")
    }

    pub fn write_array_item(
        out: &mut (impl Write + ?Sized),
        first: &mut bool,
        value: &Json,
        depth: usize,
    ) -> io::Result<()> {
        Self::write_next(out, first, depth)?;
        value.write(out, depth)
    }

    pub fn write_next(
        out: &mut (impl Write + ?Sized),
        first: &mut bool,
        depth: usize,
    ) -> io::Result<()> {
        if *first {
            *first = false;
            out.write_all(b"\n")?;
        } else {
            out.write_all(b",\n")?;
        }
        write_indent(out, depth)
    }

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

    pub fn unparse(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write(&mut out, 0)?;
        Ok(out)
    }
}

fn write_close(
    out: &mut (impl Write + ?Sized),
    first: bool,
    depth: usize,
    delimiter: &[u8],
) -> io::Result<()> {
    if !first {
        out.write_all(b"\n")?;
        write_indent(out, depth)?;
    }
    out.write_all(delimiter)
}

fn write_container_or_blob(
    owner: &Json,
    value: ContainerOrBlobSnapshot,
    out: &mut (impl Write + ?Sized),
    depth: usize,
) -> io::Result<()> {
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
            out.write_all(b"\"")?;
            let mut encoder = Base64Writer::new(&mut *out);
            writer(&mut encoder)?;
            encoder.finish()?;
            out.write_all(b"\"")
        }
    }
}

fn write_indent(out: &mut (impl Write + ?Sized), depth: usize) -> io::Result<()> {
    for _ in 0..depth {
        out.write_all(b"  ")?;
    }
    Ok(())
}
