//! qpdf correspondence: QPDFStreamFilter.cc and QPDF_Stream.cc filter-name,
//! DecodeParms-alignment, and decode-pipeline construction responsibilities.

use crate::{Error, Object, Result};

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterSpec<'a> {
    pub(crate) name: &'a [u8],
    pub(crate) decode_params: Option<&'a Object>,
}

impl FilterSpec<'_> {
    pub(crate) fn normalized_name(&self) -> &[u8] {
        match self.name {
            b"Fl" => b"FlateDecode",
            b"LZW" => b"LZWDecode",
            b"A85" => b"ASCII85Decode",
            b"AHx" => b"ASCIIHexDecode",
            b"RL" => b"RunLengthDecode",
            b"CCF" => b"CCITTFaxDecode",
            b"DCT" => b"DCTDecode",
            name => name,
        }
    }
}

pub(crate) fn decode_filter_specs<'a>(
    filter: Option<&'a Object>,
    decode_params: Option<&'a Object>,
) -> Result<Vec<FilterSpec<'a>>> {
    let names: Vec<&[u8]> = match filter {
        None | Some(Object::Null) => return Ok(Vec::new()),
        Some(Object::Name(name)) => vec![name],
        Some(Object::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_name().ok_or_else(|| {
                    Error::Unsupported("stream filter type is not name or array".to_string())
                })
            })
            .collect::<Result<_>>()?,
        Some(_) => {
            return Err(Error::Unsupported(
                "stream filter type is not name or array".to_string(),
            ))
        }
    };

    if names.is_empty() {
        return Ok(Vec::new());
    }

    let params = match decode_params {
        None | Some(Object::Null) => vec![None; names.len()],
        Some(Object::Array(items)) if items.is_empty() => vec![None; names.len()],
        Some(Object::Array(items)) => {
            if items.len() != names.len() {
                return Err(Error::Unsupported(
                    "stream /DecodeParms length is inconsistent with filters".to_string(),
                ));
            }
            items
                .iter()
                .map(|item| (!matches!(item, Object::Null)).then_some(item))
                .collect()
        }
        Some(item) => vec![Some(item); names.len()],
    };

    Ok(names
        .into_iter()
        .zip(params)
        .map(|(name, decode_params)| FilterSpec {
            name,
            decode_params,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::decode_filter_specs;
    use crate::{Dictionary, Error, Object};

    #[test]
    fn scalar_decode_parms_are_reused_for_each_filter() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let params = Object::Dictionary(Dictionary::new());

        let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs.len(), 2);
        assert!(std::ptr::eq(specs[0].decode_params.unwrap(), &params));
        assert!(std::ptr::eq(specs[1].decode_params.unwrap(), &params));
    }

    #[test]
    fn decode_parms_array_must_align_with_filter_array() {
        let filter = Object::Array(vec![
            Object::Name(b"FlateDecode".to_vec()),
            Object::Name(b"ASCII85Decode".to_vec()),
        ]);
        let params = Object::Array(vec![Object::Null]);

        let error = decode_filter_specs(Some(&filter), Some(&params)).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream /DecodeParms length is inconsistent with filters"
        );
    }

    #[test]
    fn empty_decode_parms_array_is_null_and_filter_abbreviation_expands() {
        let filter = Object::Name(b"Fl".to_vec());
        let params = Object::Array(Vec::new());

        let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs[0].normalized_name(), b"FlateDecode");
        assert!(specs[0].decode_params.is_none());
    }

    #[test]
    fn no_filter_ignores_decode_parms() {
        let params = Object::Array(vec![Object::Integer(1)]);

        let specs = decode_filter_specs(None, Some(&params)).unwrap();

        assert!(specs.is_empty());
    }

    #[test]
    fn non_name_filter_item_is_rejected_before_decode() {
        let filter = Object::Array(vec![Object::Integer(1)]);

        let error = decode_filter_specs(Some(&filter), None).unwrap_err();

        assert!(matches!(error, Error::Unsupported(_)));
        assert_eq!(
            error.to_string(),
            "unsupported PDF feature: stream filter type is not name or array"
        );
    }

    #[test]
    fn one_element_decode_parms_array_aligns_with_name_filter() {
        let filter = Object::Name(b"FlateDecode".to_vec());
        let params_item = Object::Dictionary(Dictionary::new());
        let params = Object::Array(vec![params_item.clone()]);

        let specs = decode_filter_specs(Some(&filter), Some(&params)).unwrap();

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].decode_params, Some(&params_item));
    }

    #[test]
    fn qpdf_filter_abbreviations_expand_without_changing_full_names() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"Fl", b"FlateDecode"),
            (b"LZW", b"LZWDecode"),
            (b"A85", b"ASCII85Decode"),
            (b"AHx", b"ASCIIHexDecode"),
            (b"RL", b"RunLengthDecode"),
            (b"CCF", b"CCITTFaxDecode"),
            (b"DCT", b"DCTDecode"),
            (b"FlateDecode", b"FlateDecode"),
        ];

        for &(abbreviation, expected) in cases {
            let filter = Object::Name(abbreviation.to_vec());
            let specs = decode_filter_specs(Some(&filter), None).unwrap();
            assert_eq!(specs[0].normalized_name(), expected);
        }
    }
}
