use crate::{Dictionary, Error, Object, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

pub fn decode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    decode_stream_data_with_filters(dict.get("Filter"), stream_data)
}

pub fn encode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    encode_stream_data_with_filters(dict.get("Filter"), stream_data)
}

fn decode_stream_data_with_filters(filter: Option<&Object>, stream_data: &[u8]) -> Result<Vec<u8>> {
    match filter {
        None => Ok(stream_data.to_vec()),
        Some(Object::Name(filter_name)) => apply_single_filter_decode(filter_name, stream_data)
            .map_err(|message| Error::Unsupported(message)),
        Some(Object::Array(filters)) => {
            let mut decoded = stream_data.to_vec();
            for filter in filters {
                let Object::Name(filter_name) = filter else {
                    return Err(Error::Unsupported(
                        "unsupported stream filter type: expected name".to_string(),
                    ));
                };
                decoded = apply_single_filter_decode(filter_name, &decoded)
                    .map_err(|message| Error::Unsupported(message))?;
            }
            Ok(decoded)
        }
        Some(other) => Err(Error::Unsupported(format!(
            "unsupported stream filter syntax: {}",
            object_debug_repr(other)
        ))),
    }
}

fn encode_stream_data_with_filters(filter: Option<&Object>, stream_data: &[u8]) -> Result<Vec<u8>> {
    match filter {
        None => Ok(stream_data.to_vec()),
        Some(Object::Name(filter_name)) => apply_single_filter_encode(filter_name, stream_data)
            .map_err(|message| Error::Unsupported(message)),
        Some(Object::Array(filters)) => {
            let mut encoded = stream_data.to_vec();
            for filter in filters {
                let Object::Name(filter_name) = filter else {
                    return Err(Error::Unsupported(
                        "unsupported stream filter type: expected name".to_string(),
                    ));
                };
                encoded = apply_single_filter_encode(filter_name, &encoded)
                    .map_err(|message| Error::Unsupported(message))?;
            }
            Ok(encoded)
        }
        Some(other) => Err(Error::Unsupported(format!(
            "unsupported stream filter syntax: {}",
            object_debug_repr(other)
        ))),
    }
}

fn apply_single_filter_decode(
    filter_name: &[u8],
    stream_data: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    if filter_name == b"FlateDecode" {
        let mut decoded = Vec::new();
        let mut decoder = ZlibDecoder::new(stream_data);
        decoder
            .read_to_end(&mut decoded)
            .map_err(|error| error.to_string())?;
        return Ok(decoded);
    }

    Err(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>")
    ))
}

fn apply_single_filter_encode(
    filter_name: &[u8],
    stream_data: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    if filter_name == b"FlateDecode" {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(stream_data)
            .map_err(|error| error.to_string())?;
        let encoded = encoder.finish().map_err(|error| error.to_string())?;
        return Ok(encoded);
    }

    Err(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>"),
    ))
}

fn object_debug_repr(object: &Object) -> &'static str {
    match object {
        Object::Name(name) if name == b"FlateDecode" => "FlateDecode",
        Object::Name(name) if name == b"ASCII85Decode" => "ASCII85Decode",
        Object::Name(name) if name == b"ASCIIHexDecode" => "ASCIIHexDecode",
        Object::Name(name) if name == b"LZWDecode" => "LZWDecode",
        _ => "unsupported",
    }
}
