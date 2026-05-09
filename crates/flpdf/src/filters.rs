use crate::{Dictionary, Error, Object, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

pub fn decode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    decode_stream_data_with_filters(dict.get("Filter"), dict.get("DecodeParms"), stream_data)
}

pub fn encode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    encode_stream_data_with_filters(dict.get("Filter"), stream_data)
}

fn decode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    match filter {
        None => Ok(stream_data.to_vec()),
        Some(Object::Name(filter_name)) => {
            let decoded =
                apply_single_filter_decode(filter_name, stream_data).map_err(Error::Unsupported)?;
            apply_decode_params(get_decode_params(decode_params, 0), &decoded)
        }
        Some(Object::Array(filters)) => {
            let mut decoded = stream_data.to_vec();
            for (index, filter) in filters.iter().enumerate() {
                let Object::Name(filter_name) = filter else {
                    return Err(Error::Unsupported(
                        "unsupported stream filter type: expected name".to_string(),
                    ));
                };
                decoded = apply_single_filter_decode(filter_name, &decoded)
                    .map_err(Error::Unsupported)?;
                decoded = apply_decode_params(get_decode_params(decode_params, index), &decoded)?;
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
        Some(Object::Name(filter_name)) => {
            apply_single_filter_encode(filter_name, stream_data).map_err(Error::Unsupported)
        }
        Some(Object::Array(filters)) => {
            let mut encoded = stream_data.to_vec();
            for filter in filters {
                let Object::Name(filter_name) = filter else {
                    return Err(Error::Unsupported(
                        "unsupported stream filter type: expected name".to_string(),
                    ));
                };
                encoded = apply_single_filter_encode(filter_name, &encoded)
                    .map_err(Error::Unsupported)?;
            }
            Ok(encoded)
        }
        Some(other) => Err(Error::Unsupported(format!(
            "unsupported stream filter syntax: {}",
            object_debug_repr(other)
        ))),
    }
}

fn get_decode_params(params: Option<&Object>, index: usize) -> Option<&Object> {
    match params {
        None => None,
        Some(Object::Dictionary(_)) => Some(params.unwrap()),
        Some(Object::Array(values)) => values.get(index),
        Some(_) => None,
    }
}

fn apply_decode_params(decode_params: Option<&Object>, stream_data: &[u8]) -> Result<Vec<u8>> {
    let Some(Object::Dictionary(params)) = decode_params else {
        return Ok(stream_data.to_vec());
    };

    let predictor = match params.get("Predictor") {
        Some(Object::Integer(value)) => u8::try_from(*value).map_err(|_| {
            Error::Unsupported("/DecodeParms /Predictor must be non-negative".to_string())
        })?,
        Some(_) => {
            return Err(Error::Unsupported(
                "/DecodeParms /Predictor must be integer".to_string(),
            ))
        }
        None => return Ok(stream_data.to_vec()),
    };

    if predictor <= 1 {
        return Ok(stream_data.to_vec());
    }

    if predictor != 12 {
        return Err(Error::Unsupported(format!(
            "unsupported /DecodeParms /Predictor {predictor}"
        )));
    }

    let colors = match params.get("Colors") {
        None => 1usize,
        Some(Object::Integer(value)) => usize::try_from(*value).map_err(|_| {
            Error::Unsupported("/DecodeParms /Colors must be non-negative".to_string())
        })?,
        Some(_) => {
            return Err(Error::Unsupported(
                "/DecodeParms /Colors must be integer".to_string(),
            ))
        }
    };

    let bits_per_component = match params.get("BitsPerComponent") {
        None => 8usize,
        Some(Object::Integer(value)) => usize::try_from(*value).map_err(|_| {
            Error::Unsupported("/DecodeParms /BitsPerComponent must be non-negative".to_string())
        })?,
        Some(_) => {
            return Err(Error::Unsupported(
                "/DecodeParms /BitsPerComponent must be integer".to_string(),
            ))
        }
    };

    let columns = match params.get("Columns") {
        Some(Object::Integer(value)) => usize::try_from(*value).map_err(|_| {
            Error::Unsupported("/DecodeParms /Columns must be non-negative".to_string())
        })?,
        Some(_) => {
            return Err(Error::Unsupported(
                "/DecodeParms /Columns must be integer".to_string(),
            ))
        }
        None => {
            return Err(Error::Unsupported(
                "/DecodeParms /Columns required for PNG predictor".to_string(),
            ))
        }
    };

    let row_bits = columns
        .checked_mul(colors)
        .and_then(|value| value.checked_mul(bits_per_component))
        .ok_or_else(|| Error::Unsupported("/DecodeParms /Predictor overflow".to_string()))?;
    let row_bytes = row_bits.div_ceil(8);
    if row_bytes == 0 {
        return Err(Error::Unsupported(
            "/DecodeParms /Predictor produced zero row width".to_string(),
        ));
    }

    decode_png_predictor(stream_data, row_bytes)
}

fn decode_png_predictor(bytes: &[u8], row_bytes: usize) -> Result<Vec<u8>> {
    let row_size = row_bytes
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("PNG predictor row size overflow".to_string()))?;
    if !bytes.len().is_multiple_of(row_size) {
        return Err(Error::Unsupported(
            "corrupt PNG predictor stream".to_string(),
        ));
    }

    let row_count = bytes.len() / row_size;
    let mut decoded = Vec::with_capacity(row_count * row_bytes);
    let mut previous_row = vec![0u8; row_bytes];

    for row in bytes.chunks_exact(row_size) {
        let filter = row[0];
        let raw = &row[1..];
        let mut current = vec![0u8; row_bytes];
        let bytes_per_pixel = 1;

        for i in 0..row_bytes {
            let raw_byte = raw[i];
            let left = if i >= bytes_per_pixel {
                current[i - bytes_per_pixel]
            } else {
                0
            };
            let above = previous_row[i];
            let upper_left = if i >= bytes_per_pixel {
                previous_row[i - bytes_per_pixel]
            } else {
                0
            };

            current[i] = match filter {
                0 => raw_byte,
                1 => raw_byte.wrapping_add(left),
                2 => raw_byte.wrapping_add(above),
                3 => {
                    let average = (u16::from(left) + u16::from(above)) / 2;
                    raw_byte.wrapping_add(average as u8)
                }
                4 => {
                    let p = left as i16 + above as i16 - upper_left as i16;
                    let pa = (p - left as i16).abs();
                    let pb = (p - above as i16).abs();
                    let pc = (p - upper_left as i16).abs();

                    let predictor = if pa <= pb && pa <= pc {
                        left
                    } else if pb <= pc {
                        above
                    } else {
                        upper_left
                    };

                    raw_byte.wrapping_add(predictor)
                }
                _ => {
                    return Err(Error::Unsupported(
                        "unsupported PNG predictor filter".to_string(),
                    ))
                }
            };
        }

        decoded.extend_from_slice(&current);
        previous_row = current;
    }

    Ok(decoded)
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
