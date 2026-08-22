//! qpdf correspondence: `QPDF_encryption.cc:700-716,860-904` crypt-filter interpretation and `/CF` table construction.
#![allow(dead_code)]

use super::state::{EncryptionMode, EncryptionState};
use crate::error::{EncryptedError, Result};
use crate::{Dictionary, Object, ObjectHandle};
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
    let filter = cf.try_as_name()?;
    Ok(interpret_cf_name(
        &encryption.crypt_filters,
        filter.as_deref(),
    ))
}

/// qpdf's `/CF` loop inside `QPDF::initializeEncryption`.
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
