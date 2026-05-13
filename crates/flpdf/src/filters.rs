use crate::ascii85;
use crate::ascii_hex;
use crate::security::standard::{decrypt_cipher_bytes, StringCipher};
use crate::{Dictionary, Error, Object, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

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

pub fn encode_stream_data(dict: &Dictionary, stream_data: &[u8]) -> Result<Vec<u8>> {
    encode_stream_data_with_filters(dict.get("Filter"), stream_data)
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
        Some(Object::Name(filter_name)) => {
            if filter_name == b"Crypt" {
                return decrypt_crypt(get_decode_params(decode_params, 0), stream_data);
            }
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
                if filter_name == b"Crypt" {
                    decoded = decrypt_crypt(get_decode_params(decode_params, index), &decoded)?;
                } else {
                    decoded = apply_single_filter_decode(filter_name, &decoded)
                        .map_err(Error::Unsupported)?;
                    decoded =
                        apply_decode_params(get_decode_params(decode_params, index), &decoded)?;
                }
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
    let bits_per_pixel = colors
        .checked_mul(bits_per_component)
        .ok_or_else(|| Error::Unsupported("/DecodeParms /Predictor overflow".to_string()))?;
    let bytes_per_pixel = bits_per_pixel.div_ceil(8).max(1);

    decode_png_predictor(stream_data, row_bytes, bytes_per_pixel)
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

    if filter_name == b"ASCII85Decode" {
        return ascii85::decode(stream_data);
    }

    if filter_name == b"ASCIIHexDecode" {
        return ascii_hex::decode(stream_data);
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

    if filter_name == b"ASCII85Decode" {
        return Ok(ascii85::encode(stream_data));
    }

    if filter_name == b"ASCIIHexDecode" {
        return Ok(ascii_hex::encode(stream_data));
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
}
