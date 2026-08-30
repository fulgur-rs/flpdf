//! qpdf correspondence: `QPDF_encryption.cc:700-716,860-904` crypt-filter interpretation and `/CF` table construction.
#![allow(dead_code)]

use super::state::{EncryptionMode, EncryptionState};
use crate::error::{EncryptedError, Result};
use crate::ObjectHandle;
#[cfg(test)]
use crate::{Dictionary, Object};
use std::collections::{BTreeMap, HashMap};

/// Crypt-filter method from PDF 1.7 `/CFM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptFilterMethod {
    V2,
    AesV2,
    Identity,
}

/// One named `/CF` dictionary entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CryptFilter {
    pub name: String,
    pub cfm: CryptFilterMethod,
    pub length_bits: Option<i64>,
}

/// Result of resolving a use-site selector against `/CF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptFilterRef<'a> {
    Identity,
    Named(&'a CryptFilter),
}

/// `/StmF`, `/StrF`, and `/EFF` selectors with qpdf's `/EFF` fallback.
#[derive(Debug, Clone)]
pub(crate) struct V4UseSiteSelectors {
    pub stm_f: Option<String>,
    pub str_f: Option<String>,
    pub eff: Option<String>,
}

impl V4UseSiteSelectors {
    pub(crate) fn eff_or_stm(&self) -> Option<&str> {
        self.eff.as_deref().or(self.stm_f.as_deref())
    }
}

/// Resolve a named crypt filter for a use site.
pub(crate) fn select_crypt_filter<'a>(
    cf_table: &'a HashMap<String, CryptFilter>,
    name: Option<&str>,
) -> Result<CryptFilterRef<'a>> {
    match name {
        None | Some("Identity") => Ok(CryptFilterRef::Identity),
        Some(name) => cf_table
            .get(name)
            .map(CryptFilterRef::Named)
            .ok_or_else(|| {
                EncryptedError::Malformed {
                    reason: format!("/CF entry '{name}' not found"),
                }
                .into()
            }),
    }
}

/// Map a crypt-filter method to the Standard handler's object-key algorithm.
pub(crate) fn cfm_to_object_key_alg(cfm: CryptFilterMethod) -> Option<super::keys::ObjectKeyAlg> {
    match cfm {
        CryptFilterMethod::V2 => Some(super::keys::ObjectKeyAlg::Rc4),
        CryptFilterMethod::AesV2 => Some(super::keys::ObjectKeyAlg::Aes),
        CryptFilterMethod::Identity => None,
    }
}

fn interpret_cf_name(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    filter: Option<&[u8]>,
) -> EncryptionMode {
    let Some(filter) = filter else {
        return EncryptionMode::Identity;
    };
    if let Some(mode) = crypt_filters.get(filter) {
        return *mode;
    }
    if filter == b"Identity" {
        EncryptionMode::Identity
    } else {
        EncryptionMode::Unknown
    }
}

/// qpdf `QPDF::interpretCF` for materialized dictionary values.
#[cfg(test)]
pub(crate) fn interpret_cf(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    cf: Option<&Object>,
) -> EncryptionMode {
    interpret_cf_name(crypt_filters, cf.and_then(Object::as_name))
}

/// qpdf `QPDF::interpretCF` at the lazy `ObjectHandle` boundary.
pub(crate) fn interpret_cf_from_handle(
    encryption: &EncryptionState,
    cf: &ObjectHandle,
) -> Result<EncryptionMode> {
    interpret_cf_selector_from_handle(&encryption.crypt_filters, cf)
}

/// Resolve a `/StmF`, `/StrF`, or `/EFF` selector against a parsed handle
/// crypt-filter table.
pub(crate) fn interpret_cf_selector_from_handle(
    crypt_filters: &BTreeMap<Vec<u8>, EncryptionMode>,
    cf: &ObjectHandle,
) -> Result<EncryptionMode> {
    let filter = cf.try_as_name()?;
    Ok(interpret_cf_name(crypt_filters, filter.as_deref()))
}

/// Parse qpdf's `/CF` table directly from the canonical encryption handle.
/// The caller has already resolved `encrypt`; child dictionary and `/CFM`
/// handles retain their qpdf identity and are resolved only at the accessor
/// that needs their value.
pub(crate) fn crypt_filter_modes_from_handle(
    encrypt: &ObjectHandle,
    v: i64,
) -> Result<BTreeMap<Vec<u8>, EncryptionMode>> {
    let mut modes = BTreeMap::new();
    if !matches!(v, 4 | 5) {
        return Ok(modes);
    }
    let cf = encrypt.try_get_key(b"/CF")?;
    let Some(cf) = cf.try_as_dictionary()? else {
        return Ok(modes);
    };
    for (name, value) in cf {
        let Some(filter) = value.try_as_dictionary()? else {
            continue;
        };
        let mut mode = EncryptionMode::Identity;
        let cfm = filter
            .get(b"/CFM".as_slice())
            .cloned()
            .unwrap_or_else(ObjectHandle::null);
        cfm.try_dereference()?;
        if let Some(cfm) = cfm.try_as_name()? {
            mode = match cfm.as_slice() {
                b"V2" => EncryptionMode::Rc4,
                b"AESV2" => EncryptionMode::Aes128,
                b"AESV3" => EncryptionMode::Aes256,
                _ => EncryptionMode::Unknown,
            };
        }
        let selector = name.strip_prefix(b"/").unwrap_or(&name).to_vec();
        modes.insert(selector, mode);
    }
    Ok(modes)
}

/// Report `/CF/StdCF/CFM` without materializing the encryption dictionary.
pub(crate) fn crypt_filter_method_from_handle(encrypt: &ObjectHandle) -> Result<Option<String>> {
    let cf = encrypt.try_get_key(b"/CF")?;
    let Some(cf) = cf.try_as_dictionary()? else {
        return Ok(None);
    };
    let Some(std_cf) = cf.get(b"/StdCF".as_slice()).cloned() else {
        return Ok(None);
    };
    let Some(std_cf) = std_cf.try_as_dictionary()? else {
        return Ok(None);
    };
    let Some(cfm) = std_cf.get(b"/CFM".as_slice()).cloned() else {
        return Ok(None);
    };
    cfm.try_dereference()?;
    Ok(cfm
        .try_as_name()?
        .map(|name| String::from_utf8_lossy(&name).into_owned()))
}

/// qpdf's `/CF` loop inside `QPDF::initializeEncryption`.
#[cfg(test)]
pub(crate) fn crypt_filter_modes(
    encrypt: &Dictionary,
    v: i64,
) -> BTreeMap<Vec<u8>, EncryptionMode> {
    let mut modes = BTreeMap::new();
    if !matches!(v, 4 | 5) {
        return modes;
    }
    let Some(cf) = encrypt.get("CF").and_then(Object::as_dict) else {
        return modes;
    };
    for (name, value) in cf.iter() {
        let Some(filter) = value.as_dict() else {
            continue;
        };
        let mut mode = EncryptionMode::Identity;
        if let Some(cfm) = filter.get("CFM").and_then(Object::as_name) {
            mode = match cfm {
                b"V2" => EncryptionMode::Rc4,
                b"AESV2" => EncryptionMode::Aes128,
                b"AESV3" => EncryptionMode::Aes256,
                _ => EncryptionMode::Unknown,
            };
        }
        modes.insert(name.to_vec(), mode);
    }
    modes
}

/// Report the named `/CF/StdCF/CFM` method for qpdf-compatible diagnostics.
#[cfg(test)]
pub(crate) fn crypt_filter_method(encrypt: &Dictionary) -> Option<String> {
    let Some(Object::Dictionary(cf)) = encrypt.get("CF") else {
        return None;
    };
    let Object::Dictionary(std_cf) = cf.get("StdCF")? else {
        return None;
    };
    let Object::Name(cfm) = std_cf.get("CFM")? else {
        return None;
    };
    Some(String::from_utf8_lossy(cfm).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary<K: AsRef<[u8]>>(
        entries: impl IntoIterator<Item = (K, ObjectHandle)>,
    ) -> ObjectHandle {
        ObjectHandle::dictionary(
            entries
                .into_iter()
                .map(|(key, value)| (key.as_ref().to_vec(), value))
                .collect(),
        )
    }

    #[test]
    fn crypt_filter_method_reports_each_qpdf_shape() {
        assert_eq!(crypt_filter_method(&Dictionary::new()), None);

        let mut not_a_cf_dictionary = Dictionary::new();
        not_a_cf_dictionary.insert("CF", Object::Name(b"StdCF".to_vec()));
        assert_eq!(crypt_filter_method(&not_a_cf_dictionary), None);

        let mut no_std_cf = Dictionary::new();
        no_std_cf.insert("CF", Object::Dictionary(Dictionary::new()));
        assert_eq!(crypt_filter_method(&no_std_cf), None);

        let mut std_cf_not_a_dictionary = Dictionary::new();
        let mut cf = Dictionary::new();
        cf.insert("StdCF", Object::Name(b"not-a-dict".to_vec()));
        std_cf_not_a_dictionary.insert("CF", Object::Dictionary(cf));
        assert_eq!(crypt_filter_method(&std_cf_not_a_dictionary), None);

        let mut missing_cfm = Dictionary::new();
        let mut cf = Dictionary::new();
        cf.insert("StdCF", Object::Dictionary(Dictionary::new()));
        missing_cfm.insert("CF", Object::Dictionary(cf));
        assert_eq!(crypt_filter_method(&missing_cfm), None);

        let mut wrong_cfm_type = Dictionary::new();
        let mut std_cf = Dictionary::new();
        std_cf.insert("CFM", Object::Integer(4));
        let mut cf = Dictionary::new();
        cf.insert("StdCF", Object::Dictionary(std_cf));
        wrong_cfm_type.insert("CF", Object::Dictionary(cf));
        assert_eq!(crypt_filter_method(&wrong_cfm_type), None);

        let mut valid = Dictionary::new();
        let mut std_cf = Dictionary::new();
        std_cf.insert("CFM", Object::Name(b"AESV2".to_vec()));
        let mut cf = Dictionary::new();
        cf.insert("StdCF", Object::Dictionary(std_cf));
        valid.insert("CF", Object::Dictionary(cf));
        assert_eq!(crypt_filter_method(&valid), Some("AESV2".to_owned()));
    }

    #[test]
    fn handle_crypt_filter_helpers_cover_qpdf_filter_shapes() {
        let invalid_cf = dictionary([(b"/CF", ObjectHandle::name(b"not-a-dict".to_vec()))]);
        assert!(crypt_filter_modes_from_handle(&invalid_cf, 4)
            .expect("invalid CF shape is ignored")
            .is_empty());

        let cf = dictionary([
            (
                b"/V2".as_slice(),
                dictionary([(b"/CFM", ObjectHandle::name(b"V2".to_vec()))]),
            ),
            (
                b"/AESV2".as_slice(),
                dictionary([(b"/CFM", ObjectHandle::name(b"AESV2".to_vec()))]),
            ),
            (
                b"/AESV3".as_slice(),
                dictionary([(b"/CFM", ObjectHandle::name(b"AESV3".to_vec()))]),
            ),
            (
                b"/Unknown".as_slice(),
                dictionary([(b"/CFM", ObjectHandle::name(b"Other".to_vec()))]),
            ),
            (
                b"/NoCfm".as_slice(),
                dictionary(std::iter::empty::<(&[u8], ObjectHandle)>()),
            ),
            (b"/Scalar".as_slice(), ObjectHandle::integer(1)),
        ]);
        let encrypt = dictionary([(b"/CF", cf)]);
        let modes = crypt_filter_modes_from_handle(&encrypt, 4).expect("parse CF table");
        assert_eq!(modes.get(b"V2".as_slice()), Some(&EncryptionMode::Rc4));
        assert_eq!(
            modes.get(b"AESV2".as_slice()),
            Some(&EncryptionMode::Aes128)
        );
        assert_eq!(
            modes.get(b"AESV3".as_slice()),
            Some(&EncryptionMode::Aes256)
        );
        assert_eq!(
            modes.get(b"Unknown".as_slice()),
            Some(&EncryptionMode::Unknown)
        );
        assert_eq!(
            modes.get(b"NoCfm".as_slice()),
            Some(&EncryptionMode::Identity)
        );
        assert!(!modes.contains_key(b"Scalar".as_slice()));

        assert_eq!(
            crypt_filter_method_from_handle(&ObjectHandle::dictionary(Vec::new()))
                .expect("missing CF is ignored"),
            None
        );
        let no_std_cf = dictionary([(
            b"/CF",
            dictionary(std::iter::empty::<(&[u8], ObjectHandle)>()),
        )]);
        assert_eq!(
            crypt_filter_method_from_handle(&no_std_cf).expect("missing StdCF is ignored"),
            None
        );
        let std_cf_not_dict = dictionary([(
            b"/CF",
            dictionary([(b"/StdCF", ObjectHandle::name(b"not-a-dict".to_vec()))]),
        )]);
        assert_eq!(
            crypt_filter_method_from_handle(&std_cf_not_dict)
                .expect("non-dictionary StdCF is ignored"),
            None
        );
        let missing_cfm = dictionary([(
            b"/CF",
            dictionary([(
                b"/StdCF",
                dictionary(std::iter::empty::<(&[u8], ObjectHandle)>()),
            )]),
        )]);
        assert_eq!(
            crypt_filter_method_from_handle(&missing_cfm).expect("missing CFM is ignored"),
            None
        );
        let valid = dictionary([(
            b"/CF",
            dictionary([(
                b"/StdCF",
                dictionary([(b"/CFM", ObjectHandle::name(b"AESV2".to_vec()))]),
            )]),
        )]);
        assert_eq!(
            crypt_filter_method_from_handle(&valid).expect("read CFM"),
            Some("AESV2".to_owned())
        );
    }
}
