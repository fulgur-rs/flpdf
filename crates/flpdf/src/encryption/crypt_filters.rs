//! Interpret PDF crypt filters and construct the `/CF` table.
//!
//! qpdf correspondence: `QPDF_encryption.cc:700-716,860-904` crypt-filter interpretation and `/CF` table construction.
//!
//! qpdf keeps its whole crypt-filter state in one bare
//! `std::map<std::string, encryption_method_e>` (`QPDF.hh:912`) plus the three
//! interpreted use-site values `cf_stream`/`cf_string`/`cf_file`; the `/CF`
//! walk at `QPDF_encryption.cc:860-884` discards each entry's name and
//! `/Length`, and `/StmF`, `/StrF`, `/EFF` stay function locals. The
//! equivalent flpdf state therefore lives in
//! [`EncryptionState`](super::state::EncryptionState) as a
//! `BTreeMap<Vec<u8>, EncryptionMode>`, and this module holds only the
//! functions that read it.

use super::state::{EncryptionMode, EncryptionState};
use crate::error::Result;
use crate::ObjectHandle;
use std::collections::BTreeMap;

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
