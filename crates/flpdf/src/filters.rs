use crate::ascii85;
use crate::ascii_hex;
use crate::run_length;
use crate::security::standard::{decrypt_cipher_bytes, StringCipher};
use crate::{Dictionary, Error, Object, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

/// Maximum number of stages a `/Filter` chain may declare on the **decode**
/// path. Real PDFs use at most a few stages; this rejects only pathological
/// input where each stage re-expands the previous (multiplicative blow-up).
/// Unlike qpdf — which imposes no chain-length cap — flpdf rejects such chains
/// outright; this is an intentional divergence, not a compatibility target.
/// The encode path (writer output, not untrusted) is not capped.
const MAX_FILTER_CHAIN_LEN: usize = 16;

/// Return a human-readable codec label if `filter_name` is an image/binary
/// passthrough codec that flpdf does not decode.
///
/// The four codecs (`DCTDecode`, `JBIG2Decode`, `JPXDecode`, `CCITTFaxDecode`)
/// are always emitted verbatim by the writer.  Callers (e.g. `show-stream`) can
/// use this function to distinguish "known-but-passthrough" filters from
/// genuinely unsupported ones.
///
/// Comparison is **byte-exact** (PDF names are case-sensitive per spec).
/// Returns `None` for any other filter name.
pub fn passthrough_codec_label(filter_name: &[u8]) -> Option<&'static str> {
    match filter_name {
        b"DCTDecode" => Some("DCTDecode"),
        b"JBIG2Decode" => Some("JBIG2Decode"),
        b"JPXDecode" => Some("JPXDecode"),
        b"CCITTFaxDecode" => Some("CCITTFaxDecode"),
        _ => None,
    }
}

/// Decode `stream_data` by applying the stream dictionary's `/Filter` chain,
/// honoring any `/DecodeParms` (including PNG/TIFF predictors).
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when:
/// - a `/Filter` entry is an unknown or unimplemented codec, or a `Crypt`
///   filter (decryption is not performed by this entry point).
/// - `/Filter` is neither a name nor an array of names.
/// - a `/DecodeParms` entry has an invalid value (e.g. a non-integer or
///   negative predictor parameter, or a predictor configuration that overflows).
/// - an implemented codec fails on malformed input — corrupt deflate, LZW,
///   ASCII85, ASCIIHex, or RunLength data, or a corrupt PNG-predictor stream.
pub fn decode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    decode_stream_data_with_filters(dict.get("Filter"), dict.get("DecodeParms"), stream_data)
}

// Wired into encrypted document reads by later flpdf-9hc.3 layers.
#[allow(dead_code)]
pub(crate) fn decode_stream_data_with_decryption(
    dict: &Dictionary,
    stream_data: &[u8],
    cipher: StringCipher<'_>,
) -> Result<Vec<u8>> {
    let mut decrypted = stream_data.to_vec();
    decrypt_cipher_bytes(&mut decrypted, cipher)?;
    decode_stream_data(dict, &decrypted)
}

pub(crate) fn decode_stream_data_with_crypt_filter<F>(
    dict: &Dictionary,
    stream_data: &[u8],
    mut decrypt_crypt: F,
) -> Result<Vec<u8>>
where
    F: FnMut(Option<&Object>, &[u8]) -> Result<Vec<u8>>,
{
    decode_stream_data_with_filters_and_crypt(
        dict.get("Filter"),
        dict.get("DecodeParms"),
        stream_data,
        &mut decrypt_crypt,
    )
}

/// Encode `stream_data` by applying the stream dictionary's `/Filter` chain,
/// the inverse of [`decode_stream_data`].
///
/// # Errors
///
/// Returns [`Error::Unsupported`] when:
/// - a `/Filter` entry is an unknown or unimplemented codec.
/// - `/Filter` is neither a name nor an array of names.
/// - a `/DecodeParms` entry has an invalid value (e.g. a non-integer or
///   negative predictor parameter, or a predictor configuration that overflows).
pub fn encode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    encode_stream_data_with_filters(dict.get("Filter"), dict.get("DecodeParms"), stream_data)
}

fn decode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    decode_stream_data_with_filters_and_crypt(filter, decode_params, stream_data, &mut |_, _| {
        Err(Error::Unsupported(
            "unsupported stream filter: Crypt".to_string(),
        ))
    })
}

fn decode_stream_data_with_filters_and_crypt<F>(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
    decrypt_crypt: &mut F,
) -> Result<Vec<u8>>
where
    F: FnMut(Option<&Object>, &[u8]) -> Result<Vec<u8>>,
{
    match filter {
        None => Ok(stream_data.to_vec()),
        Some(filter) => {
            if let Some(filter_name) = filter.as_name() {
                if filter_name == b"Crypt" {
                    return decrypt_crypt(get_decode_params(decode_params, 0), stream_data);
                }
                let params = get_decode_params(decode_params, 0);
                let decoded = apply_single_filter_decode(filter_name, stream_data, params)
                    .map_err(Error::Unsupported)?;
                return apply_decode_params(params, &decoded);
            }

            if let Some(filters) = filter.as_array() {
                if filters.len() > MAX_FILTER_CHAIN_LEN {
                    return Err(Error::Unsupported(format!(
                        "filter chain length {} exceeds maximum of {MAX_FILTER_CHAIN_LEN}",
                        filters.len()
                    )));
                }
                let mut decoded = stream_data.to_vec();
                for (index, filter) in filters.iter().enumerate() {
                    let Some(filter_name) = filter.as_name() else {
                        return Err(Error::Unsupported(
                            "unsupported stream filter type: expected name".to_string(),
                        ));
                    };
                    if filter_name == b"Crypt" {
                        decoded = decrypt_crypt(get_decode_params(decode_params, index), &decoded)?;
                    } else {
                        let params = get_decode_params(decode_params, index);
                        decoded = apply_single_filter_decode(filter_name, &decoded, params)
                            .map_err(Error::Unsupported)?;
                        decoded = apply_decode_params(params, &decoded)?;
                    }
                }
                return Ok(decoded);
            }

            Err(Error::Unsupported(format!(
                "unsupported stream filter syntax: {}",
                object_debug_repr(filter)
            )))
        }
    }
}

fn encode_stream_data_with_filters(
    filter: Option<&Object>,
    decode_params: Option<&Object>,
    stream_data: &[u8],
) -> Result<Vec<u8>> {
    match filter {
        None => Ok(stream_data.to_vec()),
        Some(filter) => {
            if let Some(filter_name) = filter.as_name() {
                let params = get_decode_params(decode_params, 0);
                let after_predictor = apply_encode_params(params, stream_data)?;
                return apply_single_filter_encode(filter_name, &after_predictor)
                    .map_err(Error::Unsupported);
            }

            if let Some(filters) = filter.as_array() {
                // ISO 32000-1 §7.4.2: the /Filter array names filters in *decode*
                // order, so encoding must apply them in reverse for round-tripping.
                let mut encoded = stream_data.to_vec();
                for (index, filter) in filters.iter().enumerate().rev() {
                    let Some(filter_name) = filter.as_name() else {
                        return Err(Error::Unsupported(
                            "unsupported stream filter type: expected name".to_string(),
                        ));
                    };
                    let params = get_decode_params(decode_params, index);
                    encoded = apply_encode_params(params, &encoded)?;
                    encoded = apply_single_filter_encode(filter_name, &encoded)
                        .map_err(Error::Unsupported)?;
                }
                return Ok(encoded);
            }

            Err(Error::Unsupported(format!(
                "unsupported stream filter syntax: {}",
                object_debug_repr(filter)
            )))
        }
    }
}

fn get_decode_params(params: Option<&Object>, index: usize) -> Option<&Object> {
    let param = params?;
    if param.as_dict().is_some() {
        Some(param)
    } else {
        param.as_array()?.get(index)
    }
}

fn integer_decode_param(params: &Dictionary, key: &str) -> Result<Option<i64>> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    value
        .as_integer()
        .map(Some)
        .ok_or_else(|| Error::Unsupported(format!("/DecodeParms /{key} must be integer")))
}

fn non_negative_usize_param(params: &Dictionary, key: &str) -> Result<Option<usize>> {
    integer_decode_param(params, key)?
        .map(|value| {
            usize::try_from(value).map_err(|_| {
                Error::Unsupported(format!("/DecodeParms /{key} must be non-negative"))
            })
        })
        .transpose()
}

fn non_negative_u8_param(params: &Dictionary, key: &str) -> Result<Option<u8>> {
    integer_decode_param(params, key)?
        .map(|value| {
            u8::try_from(value).map_err(|_| {
                Error::Unsupported(format!("/DecodeParms /{key} must be non-negative"))
            })
        })
        .transpose()
}

/// Extract PNG predictor parameters from a DecodeParms dictionary.
///
/// Returns `Ok(None)` when no predictor is needed (no dict, no Predictor key, or Predictor ≤ 1).
/// Returns `Ok(Some((predictor, row_bytes, bytes_per_pixel)))` for PNG predictors 10..=15.
/// Returns `Err` for Predictor 2 or any other unsupported value.
fn extract_predictor_params(decode_params: Option<&Object>) -> Result<Option<(u8, usize, usize)>> {
    let Some(params) = decode_params.and_then(Object::as_dict) else {
        return Ok(None);
    };

    let Some(predictor) = non_negative_u8_param(params, "Predictor")? else {
        return Ok(None);
    };

    if predictor <= 1 {
        return Ok(None);
    }

    if predictor == 2 {
        return Err(Error::Unsupported(
            "/DecodeParms /Predictor 2 is not supported for this stream type".to_string(),
        ));
    }

    if !(10..=15).contains(&predictor) {
        return Err(Error::Unsupported(format!(
            "unsupported /DecodeParms /Predictor {predictor}"
        )));
    }

    let colors = non_negative_usize_param(params, "Colors")?.unwrap_or(1);
    let bits_per_component = non_negative_usize_param(params, "BitsPerComponent")?.unwrap_or(8);
    let columns = non_negative_usize_param(params, "Columns")?.ok_or_else(|| {
        Error::Unsupported("/DecodeParms /Columns required for PNG predictor".to_string())
    })?;

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
    let bits_per_pixel = colors
        .checked_mul(bits_per_component)
        .ok_or_else(|| Error::Unsupported("/DecodeParms /Predictor overflow".to_string()))?;
    let bytes_per_pixel = bits_per_pixel.div_ceil(8).max(1);

    Ok(Some((predictor, row_bytes, bytes_per_pixel)))
}

fn apply_decode_params(decode_params: Option<&Object>, stream_data: &[u8]) -> Result<Vec<u8>> {
    match extract_predictor_params(decode_params)? {
        None => Ok(stream_data.to_vec()),
        Some((_predictor, row_bytes, bytes_per_pixel)) => {
            decode_png_predictor(stream_data, row_bytes, bytes_per_pixel)
        }
    }
}

fn apply_encode_params(decode_params: Option<&Object>, stream_data: &[u8]) -> Result<Vec<u8>> {
    match extract_predictor_params(decode_params)? {
        None => Ok(stream_data.to_vec()),
        Some((predictor, row_bytes, bytes_per_pixel)) => {
            encode_png_predictor(stream_data, row_bytes, bytes_per_pixel, predictor)
        }
    }
}

fn decode_png_predictor(bytes: &[u8], row_bytes: usize, bytes_per_pixel: usize) -> Result<Vec<u8>> {
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

/// Apply a single PNG filter to one byte and return the encoded (filtered) byte.
///
/// Shared between the optimum-cost loop and the per-row encoding loop so that
/// the filter logic exists in exactly one place.
#[inline]
fn png_filter_byte(filter: u8, raw: u8, left: u8, above: u8, upper_left: u8) -> u8 {
    match filter {
        0 => raw,
        1 => raw.wrapping_sub(left),
        2 => raw.wrapping_sub(above),
        3 => {
            let avg = ((u16::from(left) + u16::from(above)) / 2) as u8;
            raw.wrapping_sub(avg)
        }
        4 => {
            let p = i16::from(left) + i16::from(above) - i16::from(upper_left);
            let pa = (p - i16::from(left)).abs();
            let pb = (p - i16::from(above)).abs();
            let pc = (p - i16::from(upper_left)).abs();
            let predictor_val = if pa <= pb && pa <= pc {
                left
            } else if pb <= pc {
                above
            } else {
                upper_left
            };
            raw.wrapping_sub(predictor_val)
        }
        _ => unreachable!("png_filter_byte expects filter in 0..=4"),
    }
}

fn encode_png_predictor(
    bytes: &[u8],
    row_bytes: usize,
    bytes_per_pixel: usize,
    predictor: u8,
) -> Result<Vec<u8>> {
    if row_bytes == 0 || !bytes.len().is_multiple_of(row_bytes) {
        return Err(Error::Unsupported(
            "raw data not divisible by row_bytes".to_string(),
        ));
    }

    let row_count = bytes.len() / row_bytes;
    // Each encoded row = 1 filter byte + row_bytes of filtered data
    let mut encoded = Vec::with_capacity(row_count * (row_bytes + 1));
    let mut previous_row = vec![0u8; row_bytes];

    for raw_row in bytes.chunks_exact(row_bytes) {
        // Determine which filter byte to use for this row. For predictor 15
        // (Optimum) we accumulate all 5 filters' costs in a single pass over
        // the row to avoid iterating 5× and recomputing neighbor values for
        // each filter (libpng minimum-sum heuristic).
        let filter_byte = if predictor == 15 {
            let mut costs = [0u64; 5];
            for i in 0..row_bytes {
                let raw = raw_row[i];
                let left = if i >= bytes_per_pixel {
                    raw_row[i - bytes_per_pixel]
                } else {
                    0
                };
                let above = previous_row[i];
                let upper_left = if i >= bytes_per_pixel {
                    previous_row[i - bytes_per_pixel]
                } else {
                    0
                };
                for (f, cost) in costs.iter_mut().enumerate() {
                    let filtered = png_filter_byte(f as u8, raw, left, above, upper_left);
                    *cost += u64::from((filtered as i8).unsigned_abs());
                }
            }
            costs
                .iter()
                .enumerate()
                .min_by_key(|&(_, &c)| c)
                .map(|(i, _)| i as u8)
                .expect("costs has 5 entries")
        } else {
            // Fixed filter: Predictor 10→0, 11→1, 12→2, 13→3, 14→4
            predictor - 10
        };

        encoded.push(filter_byte);

        for i in 0..row_bytes {
            let raw = raw_row[i];
            let left = if i >= bytes_per_pixel {
                raw_row[i - bytes_per_pixel]
            } else {
                0
            };
            let above = previous_row[i];
            let upper_left = if i >= bytes_per_pixel {
                previous_row[i - bytes_per_pixel]
            } else {
                0
            };
            encoded.push(png_filter_byte(filter_byte, raw, left, above, upper_left));
        }

        // Reuse previous_row's buffer instead of allocating a fresh Vec each
        // row. previous_row was initialised with `row_bytes` zeros and
        // raw_row is row_bytes long (guaranteed by `chunks_exact(row_bytes)`),
        // so the lengths always match.
        previous_row.copy_from_slice(raw_row);
    }

    Ok(encoded)
}

fn apply_single_filter_decode(
    filter_name: &[u8],
    stream_data: &[u8],
    decode_params: Option<&Object>,
) -> std::result::Result<Vec<u8>, String> {
    if filter_name == b"FlateDecode" {
        let mut decoded = Vec::new();
        let mut decoder = ZlibDecoder::new(stream_data);
        decoder
            .read_to_end(&mut decoded)
            .map_err(|error| error.to_string())?;
        return Ok(decoded);
    }

    if filter_name == b"LZWDecode" {
        // EarlyChange (default 1 per PDF spec): when 1, the code width increases
        // one symbol *before* the table fills; when 0, it increases *after*.
        let early_change = match decode_params {
            Some(Object::Dictionary(params)) => match params.get("EarlyChange") {
                Some(Object::Integer(v)) => *v != 0,
                _ => true, // default EarlyChange = 1
            },
            _ => true, // no DecodeParms → default EarlyChange = 1
        };
        return lzw_decode(stream_data, early_change);
    }

    if filter_name == b"ASCII85Decode" {
        return ascii85::decode(stream_data);
    }

    if filter_name == b"ASCIIHexDecode" {
        return ascii_hex::decode(stream_data);
    }

    if filter_name == b"RunLengthDecode" {
        return run_length::decode(stream_data);
    }

    // Passthrough codecs: flpdf does not decode image/binary streams.
    // The writer preserves these streams verbatim (qpdf parity).
    if let Some(label) = passthrough_codec_label(filter_name) {
        return Err(format!(
            "passthrough codec {label}: image/binary stream data is not decoded by flpdf (preserved verbatim)"
        ));
    }

    Err(format!(
        "unsupported stream filter: {}",
        std::str::from_utf8(filter_name).unwrap_or("<binary>")
    ))
}

/// Decode an LZW-compressed byte stream as defined by PDF §7.4.4 (LZWDecode).
///
/// PDF's LZW variant:
/// - Starts at 9-bit codes; maximum code width is 12 bits.
/// - Code 256 = ClearTable (resets the table to the initial state and next code
///   width to 9 bits).
/// - Code 257 = EOD (end of data; any remaining bits in the current byte are
///   discarded).
/// - `early_change`: when `true` (PDF default / EarlyChange=1), the code width
///   increments one code *before* the table is full (i.e. when the next code
///   to be added would exceed the current code width capacity).  When `false`
///   (EarlyChange=0), the width increments *after* the table fills.
fn lzw_decode(data: &[u8], early_change: bool) -> std::result::Result<Vec<u8>, String> {
    const CLEAR_CODE: u16 = 256;
    const EOD_CODE: u16 = 257;
    const FIRST_CODE: u16 = 258;
    const MAX_BITS: u32 = 12;
    const MAX_TABLE_SIZE: usize = 1 << MAX_BITS; // 4096

    // The string table: each entry is the byte sequence it represents.
    // Entries 0–255 are the literal single-byte strings; 256 and 257 are
    // control codes (not stored in the table as strings).
    let mut table: Vec<Vec<u8>> = (0u8..=255).map(|b| vec![b]).collect();
    // Pad to index 258 so FIRST_CODE aligns with the next push.
    table.push(vec![]); // slot 256 (ClearCode sentinel — never looked up)
    table.push(vec![]); // slot 257 (EOD sentinel — never looked up)

    let mut output = Vec::new();
    let mut bit_buf: u64 = 0;
    let mut bits_in_buf: u32 = 0;
    let mut code_bits: u32 = 9;

    // The "next table index" at which the width bumps.  With early_change=true
    // the bump triggers when next_entry == (1 << code_bits) - 1 (one slot before
    // the table fills); with early_change=false it triggers when
    // next_entry == (1 << code_bits) (table exactly full).
    let early_offset: usize = if early_change { 1 } else { 0 };

    let mut prev_entry: Option<Vec<u8>> = None;

    let mut byte_pos = 0usize;

    loop {
        // Fill the bit buffer until it has at least `code_bits` bits.
        while bits_in_buf < code_bits {
            if byte_pos >= data.len() {
                // Ran out of input before EOD code — treat as implicit EOD.
                return Ok(output);
            }
            bit_buf = (bit_buf << 8) | u64::from(data[byte_pos]);
            bits_in_buf += 8;
            byte_pos += 1;
        }

        // Extract the next `code_bits`-wide code from the MSB side.
        let shift = bits_in_buf - code_bits;
        let code = ((bit_buf >> shift) & ((1u64 << code_bits) - 1)) as u16;
        bits_in_buf -= code_bits;
        bit_buf &= (1u64 << bits_in_buf) - 1;

        if code == EOD_CODE {
            break;
        }

        if code == CLEAR_CODE {
            // Reset: truncate the table back to the 256 literals + 2 sentinels.
            table.truncate(FIRST_CODE as usize);
            code_bits = 9;
            prev_entry = None;
            continue;
        }

        // Resolve the code to its string.
        let entry: Vec<u8> = if (code as usize) < table.len() {
            table[code as usize].clone()
        } else if code as usize == table.len() {
            // The classic "KwKwK" case: the code being added is the one we're
            // currently processing.  Its string is prev + first_byte(prev).
            match &prev_entry {
                Some(prev) => {
                    let mut s = prev.clone();
                    s.push(prev[0]);
                    s
                }
                None => {
                    return Err(format!(
                        "LZWDecode: code {code} is one past table end but no previous entry"
                    ))
                }
            }
        } else {
            return Err(format!(
                "LZWDecode: code {code} out of range (table size {})",
                table.len()
            ));
        };

        output.extend_from_slice(&entry);

        // Add a new entry = prev_string + first_byte(current_string).
        if let Some(ref prev) = prev_entry {
            if table.len() < MAX_TABLE_SIZE {
                let mut new_entry = prev.clone();
                new_entry.push(entry[0]);
                table.push(new_entry);

                // Bump code width when the table reaches the trigger threshold.
                if code_bits < MAX_BITS && table.len() == (1usize << code_bits) - early_offset {
                    code_bits += 1;
                }
            }
        }

        prev_entry = Some(entry);
    }

    Ok(output)
}

/// Apply a single encode filter to `stream_data`.
///
/// # Write-side compression policy
///
/// flpdf writes stream compression as **FlateDecode only**.
/// LZWEncode is intentionally unsupported — qpdf also has no LZW encoder.
/// Image/binary passthrough codecs (DCTDecode, JBIG2Decode, JPXDecode, CCITTFaxDecode)
/// are never re-encoded by flpdf; the writer preserves those streams verbatim.
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

    if filter_name == b"ASCII85Decode" {
        return Ok(ascii85::encode(stream_data));
    }

    if filter_name == b"ASCIIHexDecode" {
        return Ok(ascii_hex::encode(stream_data));
    }

    if filter_name == b"RunLengthDecode" {
        return Ok(run_length::encode(stream_data));
    }

    // LZWEncode is not supported: flpdf writes stream compression as FlateDecode only
    // (decision flpdf-9hc.7.2; qpdf has no LZW encoder either).
    if filter_name == b"LZWDecode" {
        return Err(
            "LZWEncode is not supported: flpdf writes stream compression as FlateDecode only \
             (decision flpdf-9hc.7.2; qpdf has no LZW encoder either)"
                .to_string(),
        );
    }

    // Passthrough codecs are never re-encoded; the writer preserves those streams verbatim.
    if let Some(label) = passthrough_codec_label(filter_name) {
        return Err(format!(
            "encode not supported for passthrough codec {label}: \
             image/binary streams are preserved verbatim by flpdf"
        ));
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
        Object::Name(name) if name == b"RunLengthDecode" => "RunLengthDecode",
        _ => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::primitives::rc4;
    use crate::security::standard::StringCipher;
    use aes::{Aes128, Aes256};
    use cbc::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    use cbc::Encryptor;

    fn flate_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict
    }

    fn aes128_stream(key: &[u8; 16], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let enc = <Encryptor<Aes128> as KeyIvInit>::new(key.into(), iv.into());
        let mut buf = plaintext.to_vec();
        let msg_len = buf.len();
        buf.resize(msg_len + 16, 0);
        let ciphertext = enc.encrypt_padded_mut::<Pkcs7>(&mut buf, msg_len).unwrap();
        let mut out = iv.to_vec();
        out.extend_from_slice(ciphertext);
        out
    }

    fn aes256_stream(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
        let enc = <Encryptor<Aes256> as KeyIvInit>::new(key.into(), iv.into());
        let mut buf = plaintext.to_vec();
        let msg_len = buf.len();
        buf.resize(msg_len + 16, 0);
        let ciphertext = enc.encrypt_padded_mut::<Pkcs7>(&mut buf, msg_len).unwrap();
        let mut out = iv.to_vec();
        out.extend_from_slice(ciphertext);
        out
    }

    #[test]
    fn decode_stream_data_with_decryption_decrypts_before_flate() {
        let dict = flate_dict();
        let plaintext = b"stream plaintext after flate";
        let mut encrypted = encode_stream_data(&dict, plaintext).unwrap();
        rc4(b"Key", &mut encrypted).unwrap();

        let decoded = decode_stream_data_with_decryption(
            &dict,
            &encrypted,
            StringCipher::Rc4 { key: b"Key" },
        )
        .unwrap();

        assert_eq!(decoded, plaintext);
    }

    #[test]
    fn decode_stream_data_with_decryption_handles_no_filter_rc4_streams() {
        let dict = Dictionary::new();
        let mut encrypted = b"raw encrypted stream".to_vec();
        rc4(b"Key", &mut encrypted).unwrap();

        let decoded = decode_stream_data_with_decryption(
            &dict,
            &encrypted,
            StringCipher::Rc4 { key: b"Key" },
        )
        .unwrap();

        assert_eq!(decoded, b"raw encrypted stream");
    }

    #[test]
    fn decode_stream_data_with_decryption_handles_aes_streams() {
        let dict = Dictionary::new();
        let aes128_key = [0x11; 16];
        let aes128_iv = [0x22; 16];
        let aes128 = aes128_stream(&aes128_key, &aes128_iv, b"AES-128 stream");
        let decoded = decode_stream_data_with_decryption(
            &dict,
            &aes128,
            StringCipher::Aes128 { key: &aes128_key },
        )
        .unwrap();
        assert_eq!(decoded, b"AES-128 stream");

        let aes256_key = [0x33; 32];
        let aes256_iv = [0x44; 16];
        let aes256 = aes256_stream(&aes256_key, &aes256_iv, b"AES-256 stream");
        let decoded = decode_stream_data_with_decryption(
            &dict,
            &aes256,
            StringCipher::Aes256 { key: &aes256_key },
        )
        .unwrap();
        assert_eq!(decoded, b"AES-256 stream");
    }

    #[test]
    fn decode_stream_data_without_decryption_keeps_plaintext_behavior() {
        let dict = flate_dict();
        let plaintext = b"legacy plaintext flate";
        let encoded = encode_stream_data(&dict, plaintext).unwrap();

        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext);
    }

    // ----- ASCIIHexDecode filter integration tests -----

    fn ascii_hex_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
        dict
    }

    #[test]
    fn decode_stream_data_ascii_hex_round_trip() {
        let dict = ascii_hex_dict();
        let plaintext = b"Hello from ASCIIHexDecode filter!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii_hex_empty() {
        let dict = ascii_hex_dict();
        let plaintext = b"";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii_hex_odd_length_data() {
        let dict = ascii_hex_dict();
        // 3 bytes → odd nibble count in inner encoding only if we provide raw odd data;
        // encode always emits two hex chars per byte so no padding needed on decode
        let plaintext = b"ABC";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    // ----- ASCII85Decode filter integration tests -----

    fn ascii85_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"ASCII85Decode".to_vec()));
        dict
    }

    #[test]
    fn decode_stream_data_ascii85_round_trip() {
        let dict = ascii85_dict();
        let plaintext = b"Hello from ASCII85Decode filter!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_empty() {
        let dict = ascii85_dict();
        let plaintext = b"";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_zero_block() {
        let dict = ascii85_dict();
        // A 4-byte all-zero block triggers the 'z' shorthand in the encoder
        let plaintext = [0u8; 8]; // two complete zero blocks → encoder emits "zz~>"

        let encoded = encode_stream_data(&dict, &plaintext).unwrap();
        // Verify the encoder actually used the 'z' shorthand
        assert!(
            encoded.contains(&b'z'),
            "encoder should emit 'z' for 4-byte zero block"
        );
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_ascii85_short_final_group() {
        let dict = ascii85_dict();
        // Test all three short-final-group lengths: 1, 2, 3 bytes remainder
        for plaintext in [b"M".as_slice(), b"Ma", b"Man"] {
            let encoded = encode_stream_data(&dict, plaintext).unwrap();
            let decoded = decode_stream_data(&dict, &encoded).unwrap();
            assert_eq!(
                decoded,
                plaintext,
                "short final group round-trip failed for {} bytes",
                plaintext.len()
            );
        }
    }

    #[test]
    fn decode_stream_data_ascii85_rejects_invalid_byte() {
        let dict = ascii85_dict();
        // 'v' (0x76) is above the valid range '!'..'u' (0x21..=0x75)
        // Feed a hand-crafted stream: "9jqov~>" where 'v' is out-of-range
        let invalid_stream = b"9jqov~>";

        let result = decode_stream_data(&dict, invalid_stream);

        assert!(result.is_err(), "expected error for out-of-range byte");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("ASCII85Decode"),
            "error message should contain 'ASCII85Decode', got: {msg}"
        );
    }

    // ----- RunLengthDecode filter integration tests -----

    fn run_length_dict() -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"RunLengthDecode".to_vec()));
        dict
    }

    #[test]
    fn decode_stream_data_run_length_round_trip() {
        let dict = run_length_dict();
        let plaintext = b"Hello from RunLengthDecode filter!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_run_length_empty() {
        let dict = run_length_dict();
        let plaintext = b"";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_run_length_with_repeats() {
        let dict = run_length_dict();
        // Data with prominent repeat runs (triggers repeat-run encoding).
        let mut plaintext = vec![0x42u8; 100]; // 100 'B' bytes
        plaintext.extend(b"literal");
        plaintext.extend(vec![0xCCu8; 50]); // 50 0xCC bytes

        let encoded = encode_stream_data(&dict, &plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn decode_stream_data_run_length_rejects_truncated_literal() {
        let dict = run_length_dict();
        // Hand-crafted truncated stream: header says 6 literals (l=5) but only 3 follow.
        let truncated_stream = vec![0x05u8, b'A', b'B', b'C']; // 3 bytes instead of 6

        let result = decode_stream_data(&dict, &truncated_stream);

        assert!(
            result.is_err(),
            "expected error for truncated literal stream"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("RunLengthDecode"),
            "error message should contain 'RunLengthDecode', got: {msg}"
        );
    }

    // ----- Array filter chain round-trip tests (regression for flpdf-fh8) -----
    //
    // Per ISO 32000-1 §7.4.2, the /Filter array names the filters in the order
    // they must be applied to *decode* the stream. The encoder therefore has
    // to apply them in reverse so that `decode(encode(x))` round-trips for any
    // multi-element filter chain.

    fn array_filter_dict(filters: &[&[u8]]) -> Dictionary {
        let mut dict = Dictionary::new();
        let names: Vec<Object> = filters.iter().map(|f| Object::Name(f.to_vec())).collect();
        dict.insert("Filter", Object::Array(names));
        dict
    }

    #[test]
    fn encode_stream_data_array_chain_round_trips_ascii85_then_flate() {
        // Decoder order: ASCII85Decode, then FlateDecode.
        // Encoder must therefore apply FlateDecode first, then ASCII85Decode.
        let dict = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let plaintext = b"Round-trip me through ASCII85 over Flate, please!";

        let encoded = encode_stream_data(&dict, plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext.as_slice());
    }

    #[test]
    fn encode_stream_data_array_chain_round_trips_ascii_hex_then_flate() {
        let dict = array_filter_dict(&[b"ASCIIHexDecode", b"FlateDecode"]);
        let plaintext: Vec<u8> = (0u8..=200u8).collect();

        let encoded = encode_stream_data(&dict, &plaintext).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, plaintext);
    }

    #[test]
    fn encode_stream_data_array_chain_single_filter_matches_name_form() {
        // /Filter [/FlateDecode] should behave identically to /Filter /FlateDecode.
        let array_dict = array_filter_dict(&[b"FlateDecode"]);
        let name_dict = flate_dict();
        let plaintext = b"single-filter array form";

        let encoded_array = encode_stream_data(&array_dict, plaintext).unwrap();
        let encoded_name = encode_stream_data(&name_dict, plaintext).unwrap();

        assert_eq!(
            encoded_array, encoded_name,
            "Array form with one filter should produce the same bytes as the Name form"
        );
    }

    // ----- PNG predictor encode round-trip tests -----

    fn png_predictor_dict(predictor: i64, columns: i64) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(predictor));
        parms.insert("Columns", Object::Integer(columns));
        dict.insert("DecodeParms", Object::Dictionary(parms));
        dict
    }

    fn png_predictor_dict_rgb(predictor: i64, columns: i64) -> Dictionary {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor", Object::Integer(predictor));
        parms.insert("Columns", Object::Integer(columns));
        parms.insert("Colors", Object::Integer(3));
        parms.insert("BitsPerComponent", Object::Integer(8));
        dict.insert("DecodeParms", Object::Dictionary(parms));
        dict
    }

    /// Simple 2-row, 4-column grayscale raw data for predictor round-trip tests.
    fn sample_raw_4x2() -> Vec<u8> {
        vec![
            10, 20, 30, 40, // row 0
            50, 60, 70, 80, // row 1
        ]
    }

    #[test]
    fn encode_stream_data_png_predictor_10_round_trip() {
        let dict = png_predictor_dict(10, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_11_round_trip() {
        let dict = png_predictor_dict(11, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_12_round_trip() {
        let dict = png_predictor_dict(12, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_13_round_trip() {
        let dict = png_predictor_dict(13, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_14_round_trip() {
        let dict = png_predictor_dict(14, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_15_round_trip() {
        let dict = png_predictor_dict(15, 4);
        let raw = sample_raw_4x2();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_handles_multi_row() {
        // row_bytes=8, rows=4 → 32 bytes total
        let dict = png_predictor_dict(12, 8);
        let raw: Vec<u8> = (0u8..32).collect();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn encode_stream_data_png_predictor_rgb_fixture_round_trip() {
        // Colors=3, BitsPerComponent=8, Columns=4 → row_bytes=12, rows=4 → 48 bytes
        let dict = png_predictor_dict_rgb(15, 4);
        let raw: Vec<u8> = (0u8..48).collect();
        let encoded = encode_stream_data(&dict, &raw).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    // ----- passthrough_codec_label tests (flpdf-9hc.7.4) -----

    #[test]
    fn passthrough_codec_label_recognizes_all_four_codecs() {
        assert_eq!(
            passthrough_codec_label(b"DCTDecode"),
            Some("DCTDecode"),
            "DCTDecode must be recognised"
        );
        assert_eq!(
            passthrough_codec_label(b"JBIG2Decode"),
            Some("JBIG2Decode"),
            "JBIG2Decode must be recognised"
        );
        assert_eq!(
            passthrough_codec_label(b"JPXDecode"),
            Some("JPXDecode"),
            "JPXDecode must be recognised"
        );
        assert_eq!(
            passthrough_codec_label(b"CCITTFaxDecode"),
            Some("CCITTFaxDecode"),
            "CCITTFaxDecode must be recognised"
        );
    }

    #[test]
    fn passthrough_codec_label_is_case_sensitive() {
        // PDF names are case-sensitive; lower-case variants must return None.
        assert_eq!(
            passthrough_codec_label(b"dctdecode"),
            None,
            "lowercase dctdecode must not match"
        );
        assert_eq!(
            passthrough_codec_label(b"jbig2decode"),
            None,
            "lowercase jbig2decode must not match"
        );
        assert_eq!(
            passthrough_codec_label(b"jpxdecode"),
            None,
            "lowercase jpxdecode must not match"
        );
        assert_eq!(
            passthrough_codec_label(b"ccittfaxdecode"),
            None,
            "lowercase ccittfaxdecode must not match"
        );
    }

    #[test]
    fn passthrough_codec_label_returns_none_for_unknown_filters() {
        assert_eq!(passthrough_codec_label(b"FlateDecode"), None);
        assert_eq!(passthrough_codec_label(b"LZWDecode"), None);
        assert_eq!(passthrough_codec_label(b"ASCII85Decode"), None);
        assert_eq!(passthrough_codec_label(b"ASCIIHexDecode"), None);
        assert_eq!(passthrough_codec_label(b"RunLengthDecode"), None);
        assert_eq!(passthrough_codec_label(b"UnknownFilter"), None);
        assert_eq!(passthrough_codec_label(b""), None);
    }

    // ----- flpdf-9hc.7.5: dispatch coverage tests -----

    /// Chain round-trip: Flate→ASCII85 encode, [/ASCII85Decode /FlateDecode] decode.
    /// This verifies that encode and decode correctly handle multi-filter chains
    /// (encode applies filters in reverse; decode applies in forward order).
    #[test]
    fn filter_chain_flate_ascii85_round_trip() {
        let dict = array_filter_dict(&[b"ASCII85Decode", b"FlateDecode"]);
        let payload = b"chain round-trip: Flate + ASCII85 (flpdf-9hc.7.5)";

        let encoded = encode_stream_data(&dict, payload).unwrap();
        let decoded = decode_stream_data(&dict, &encoded).unwrap();

        assert_eq!(decoded, payload.as_slice());
    }

    /// Case-sensitivity: lowercase filter names must not match and must return Err.
    /// PDF names are case-sensitive per spec.
    #[test]
    fn filter_dispatch_is_case_sensitive() {
        // lowercase "flatedecode" is not a recognised filter
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"flatedecode".to_vec()));
        let result = decode_stream_data(&dict, b"anything");
        assert!(
            result.is_err(),
            "lowercase 'flatedecode' must not be accepted"
        );

        // passthrough_codec_label also rejects lowercase
        assert_eq!(
            passthrough_codec_label(b"dctdecode"),
            None,
            "passthrough_codec_label must be case-sensitive"
        );
        assert_eq!(passthrough_codec_label(b"jpxdecode"), None);

        // lowercase "dctdecode" in a stream dict must produce an unsupported Err
        let mut dict2 = Dictionary::new();
        dict2.insert("Filter", Object::Name(b"dctdecode".to_vec()));
        let result2 = decode_stream_data(&dict2, b"anything");
        assert!(
            result2.is_err(),
            "lowercase 'dctdecode' must not be accepted"
        );
        // message should NOT claim it is a passthrough codec; it is generic unsupported
        let msg2 = result2.unwrap_err().to_string();
        assert!(
            !msg2.contains("passthrough codec"),
            "lowercase filter should hit generic unsupported, not passthrough branch; got: {msg2}"
        );
    }

    /// passthrough-in-chain: /Filter [/ASCII85Decode /DCTDecode] decode must return Err
    /// because DCTDecode is a passthrough codec that flpdf does not decode.
    /// The input must be valid ASCII85 data so that step 0 succeeds and
    /// step 1 (DCTDecode) is reached.
    #[test]
    fn passthrough_in_chain_returns_err_with_passthrough_message() {
        // Build a valid ASCII85-encoded payload so the first filter succeeds.
        let ascii85_encoded = ascii85::encode(b"some binary jpeg payload");

        let dict = array_filter_dict(&[b"ASCII85Decode", b"DCTDecode"]);
        let result = decode_stream_data(&dict, &ascii85_encoded);

        assert!(
            result.is_err(),
            "chain containing DCTDecode must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("DCTDecode"),
            "error must mention DCTDecode; got: {msg}"
        );
        assert!(
            msg.contains("passthrough"),
            "error must indicate passthrough intent; got: {msg}"
        );
    }

    /// LZWEncode is not supported: encode_stream_data with /LZWDecode filter must Err.
    #[test]
    fn lzw_encode_unsupported_returns_err() {
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));

        let result = encode_stream_data(&dict, b"some data");

        assert!(result.is_err(), "LZWEncode must not be supported");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LZWEncode"),
            "error must mention LZWEncode; got: {msg}"
        );
        assert!(
            msg.contains("FlateDecode only"),
            "error must mention FlateDecode only policy; got: {msg}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CARRY-OVER flpdf-9hc.7.1: LZW malformed-rejection unit tests
    //
    // Verifies that lzw_decode (via decode_stream_data) returns Err for
    // inputs containing out-of-range codes (review-pattern #3: external
    // integer values must not cause silent wrap-around or panic).
    // ─────────────────────────────────────────────────────────────────────────

    /// A 9-bit LZW stream containing a code that is one past the current table
    /// end (code 258, "KwKwK" scenario with no previous entry) must return Err.
    /// This exercises the "one past table end but no previous entry" branch in
    /// lzw_decode (~L610-613).
    #[test]
    fn lzw_decode_malformed_one_past_end_without_prev_returns_err() {
        // Craft a minimal LZW stream:
        //   9-bit codes, no ClearCode prefix (so table has entries 0-257 only,
        //   next entry == 258).  Emit code 258 directly; since there is no
        //   previous entry, this must Err.
        //
        // Code 258 in 9-bit big-endian MSB-first packing:
        //   0b100_000_010 = 0x102
        //   Packed into two bytes at the start of the bit buffer:
        //     bits 8..0 of code 258 across two bytes:
        //     byte 0: bits [8..1] = 0b1000_0001 = 0x81
        //     byte 1: bits [0]   = 0b0_0000000 + EOD code...
        //   Simpler: pack [ClearCode=256][code=258][EOD=257].
        //   ClearCode resets the table but also clears prev_entry; then code 258
        //   comes in with no prev → Err.
        //
        //   Bit layout (9 bits each, MSB first):
        //     ClearCode 256 = 0b100000000 → 9 bits
        //     code 258      = 0b100000010 → 9 bits
        //     EOD 257       = 0b100000001 → 9 bits (never reached)
        //   Total = 27 bits → 4 bytes.
        //
        //   Byte packing (MSB first across bytes):
        //     256 = 1 00000000
        //     258 = 1 00000010
        //     Concatenated 18 bits: 1_00000000_1_00000010 = 0x10082 padded to 3 bytes
        //     byte0 = 0x80, byte1 = 0x40 (256 = 0x100 >> 1, then 258 = 0x102 << 0)
        //   Let's compute manually:
        //     bit buffer: 1 00000000 | 1 00000010 = 0b10000000010000001_0
        //       byte0 = bits [17..10] = 0b10000000 = 0x80
        //       byte1 = bits [9..2]   = 0b01000000 = 0x40  (0b0_10000001 >> 1)
        //       byte2 = bits [1..0]   = 0b10 << 6  = 0x80
        //
        //   Verified against the lzw_decode bit-extraction logic.
        let malformed: &[u8] = &[0x80, 0x40, 0x80];
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
        let result = decode_stream_data(&dict, malformed);
        assert!(
            result.is_err(),
            "LZWDecode: code-258-after-clear-with-no-prev must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LZWDecode"),
            "error must mention LZWDecode; got: {msg}"
        );
        assert!(
            msg.contains("no previous entry") || msg.contains("out of range"),
            "error must describe the out-of-range / no-prev condition; got: {msg}"
        );
    }

    /// A 9-bit LZW stream containing a genuinely out-of-range code (≥ 259 when
    /// table has only 258 entries) must return Err with an "out of range" message.
    /// This exercises the explicit range-check in lzw_decode (~L615-619).
    #[test]
    fn lzw_decode_malformed_genuinely_out_of_range_code_returns_err() {
        // We want code 259 (0b100000011) to appear AFTER code 258 has been
        // added by processing one normal code, making table size == 259.
        // That is complicated; instead use the simpler path:
        //   Emit ClearCode (resets table to 258 entries), then code 259.
        //   Table size after clear is 258; code 259 > 258 → "out of range".
        //
        // Bit layout (9 bits each, MSB first):
        //   ClearCode 256 = 0b100000000
        //   code 259      = 0b100000011
        //   = 18 bits: 1_00000000_1_00000011
        //   byte0 = top 8 bits = 0b10000000 = 0x80
        //   byte1 = next 8     = 0b01000000 = 0x40  (wait — need 259 after)
        //
        //   Concatenate 9+9 = 18 bits:
        //     0b1_0000_0000_1_0000_0011
        //     split into bytes:
        //       byte0 = 0b1000_0000 = 0x80
        //       byte1 = 0b0100_0000 = 0x40  [wrong: 0b01_00000011 >> 1 = 0b0010...]
        //
        //   Correct bit-level packing (MSB first):
        //     bits: 1 0000_0000  1 0000_0011  (18 bits total)
        //     byte 0: bits[17..10] = 1000_0000 = 0x80
        //     byte 1: bits[9..2]   = 0100_0000 | (0b00_1100_0000 >> 2)...
        //   Let's just do it directly:
        //     value = (256 << 9) | 259 = 0x20103
        //     byte0 = (0x20103 >> 10) & 0xFF = 0x80
        //     byte1 = (0x20103 >> 2)  & 0xFF = 0x40
        //     byte2 = (0x20103 << 6)  & 0xFF = 0xC0  (bits[1..0]=0b11, shifted left 6)
        //
        //   After ClearCode the table has entries 0..=257 (size 258).
        //   Code 259 > 258 → "out of range" branch.
        let malformed: &[u8] = &[0x80, 0x40, 0xC0];
        let mut dict = Dictionary::new();
        dict.insert("Filter", Object::Name(b"LZWDecode".to_vec()));
        let result = decode_stream_data(&dict, malformed);
        assert!(
            result.is_err(),
            "LZWDecode: genuinely out-of-range code after ClearCode must return Err"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LZWDecode"),
            "error must mention LZWDecode; got: {msg}"
        );
        assert!(
            msg.contains("out of range") || msg.contains("no previous entry"),
            "error must describe the out-of-range condition; got: {msg}"
        );
    }

    // ----- Task 1: /Filter chain length cap (flpdf-hn1g.4) -----

    #[test]
    fn decode_rejects_overlong_filter_chain() {
        // 17 filters (> MAX_FILTER_CHAIN_LEN = 16) on the decode path is rejected
        // before any stage runs. The data is irrelevant; the cap trips first.
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"FlateDecode".to_vec()); 17]),
        );
        let err = decode_stream_data(&dict, b"anything");
        assert!(
            matches!(err, Err(Error::Unsupported(ref m)) if m.contains("filter chain length")),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_accepts_max_length_filter_chain() {
        // Exactly MAX_FILTER_CHAIN_LEN (16) ASCIIHexDecode stages round-trips (each
        // stage is identity here: hex-encode applied 16 times, then this many decodes).
        // Build by encoding 16 times so the 16-deep decode chain reproduces the input.
        let original = b"hello";
        let mut data = original.to_vec();
        for _ in 0..16 {
            data = encode_stream_data(
                &{
                    let mut d = Dictionary::new();
                    d.insert("Filter", Object::Name(b"ASCIIHexDecode".to_vec()));
                    d
                },
                &data,
            )
            .unwrap();
        }
        let mut dict = Dictionary::new();
        dict.insert(
            "Filter",
            Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()); 16]),
        );
        let decoded = decode_stream_data(&dict, &data).unwrap();
        assert_eq!(decoded, original);
    }
}
