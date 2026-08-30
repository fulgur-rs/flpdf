//! qpdf correspondence: `QPDF.hh:899-923` and `QPDF_encryption.cc:700-1205` encryption state, crypt-filter dispatch, object-key cache, and inspection projection.

use super::crypt_filters::{
    crypt_filter_method_from_handle, crypt_filter_modes_from_handle,
    interpret_cf_selector_from_handle,
};
use super::password::{password_bytes_for_read, PasswordMode};
use super::permissions::Permissions;
use super::standard::{
    check_owner_password_r5, check_owner_password_r6, check_owner_password_v4_with_user_password,
    check_owner_password_with_user_password, check_user_password, check_user_password_r5,
    check_user_password_r6, check_user_password_v4, StandardHandlerInputs, StandardHandlerR5Inputs,
};
use crate::encryption::standard::{decrypt_cipher_bytes, StringCipher};
use crate::error::{EncryptedError, Result};
use crate::{ObjectHandle, ObjectRef};
use std::collections::BTreeMap;

/// qpdf's `QPDF::EncryptionParameters` state for an authenticated document.
///
/// qpdf's `encrypted` and `encryption_initialized` flags are folded into this
/// being an `Option<EncryptionState>`; the user password is retained only for
/// qpdf's read-only `getTrimmedUserPassword` inspection contract.
#[derive(Debug, Clone)]
pub(crate) struct EncryptionState {
    pub(crate) file_key: Vec<u8>,
    /// qpdf `encryption_V` / `encryption_R`.
    pub(crate) encryption_v: i64,
    pub(crate) encryption_r: i64,
    /// qpdf `cf_stream` / `cf_string` / `cf_file`.
    pub(crate) cf_stream: EncryptionMode,
    pub(crate) cf_string: EncryptionMode,
    pub(crate) cf_file: EncryptionMode,
    pub(crate) crypt_filters: BTreeMap<Vec<u8>, EncryptionMode>,
    pub(crate) encrypt_metadata: bool,
    pub(crate) encrypt_ref: Option<ObjectRef>,
    pub(crate) weak_crypto: bool,
    pub(crate) permissions: Permissions,
    /// The `/ID[0]` bytes used to derive `file_key`, when derivation is
    /// `/ID[0]`-dependent (qpdf's Algorithm 2, V<5). `None` for the R5/R6
    /// AES-256 path, which does not read `/ID` at all
    /// (`QPDF_encryption.cc:736-743` computes `id1` unconditionally before
    /// dispatching to a handler, but only the V<5 handler consumes it). A
    /// consumer that must stay paired with `file_key` (for example a
    /// preserve-encryption copy source) must use this cached value instead
    /// of re-reading a possibly-since-mutated live trailer `/ID`.
    pub(crate) id0: Option<Vec<u8>>,
    pub(crate) user_password_matched: bool,
    pub(crate) owner_password_matched: bool,
    /// qpdf `EncryptionParameters::user_password`: padded/recovered for
    /// V<5 owner authentication, supplied bytes for a user-password match,
    /// and empty for V>=5 owner-only or raw-key authentication.
    pub(crate) user_password: Vec<u8>,
    /// qpdf `cached_object_encryption_key` / `cached_key_og`
    /// (`include/qpdf/QPDF.hh:918-919`).
    pub(crate) cached_object_encryption_key: Vec<u8>,
    pub(crate) cached_key_og: Option<ObjectRef>,
}

/// What qpdf's crypt-filter switch decided: whether AES is used, whether the
/// caller returns without a decrypt stage, and whether an unknown-filter
/// warning is owed.
type MethodChoice = (Option<bool>, bool);

impl EncryptionState {
    /// qpdf's `decryptString` and `decryptStream` crypt-filter switch
    /// (`QPDF_encryption.cc:982-1006,1062-1134`).
    pub(crate) fn select_method(
        method: EncryptionMode,
        cf: &mut EncryptionMode,
        encryption_v: i64,
    ) -> MethodChoice {
        if encryption_v < 4 {
            // qpdf enters this switch only for V >= 4. Older revisions always
            // use RC4, regardless of the stored crypt-filter fields.
            return (Some(false), false);
        }
        match method {
            EncryptionMode::Identity => (None, false),
            EncryptionMode::Aes128 | EncryptionMode::Aes256 => (Some(true), false),
            EncryptionMode::Rc4 => (Some(false), false),
            EncryptionMode::Unknown => {
                // qpdf rewrites the selected field so the warning is emitted
                // only once for subsequent objects.
                *cf = EncryptionMode::Aes128;
                (Some(true), true)
            }
        }
    }

    /// qpdf `QPDF::decryptString` method selection.
    pub(crate) fn string_method(&mut self) -> MethodChoice {
        let (method, encryption_v) = (self.cf_string, self.encryption_v);
        Self::select_method(method, &mut self.cf_string, encryption_v)
    }

    /// qpdf `QPDF::decryptString` for one literal string.
    pub(crate) fn decrypt_object_string(
        &mut self,
        object_ref: ObjectRef,
        bytes: &mut Vec<u8>,
    ) -> Result<bool> {
        let (use_aes, warn_unknown_string) = self.string_method();
        if let Some(use_aes) = use_aes {
            self.with_object_cipher(object_ref, use_aes, |cipher| {
                decrypt_cipher_bytes(bytes, cipher)
            })?;
        }
        Ok(warn_unknown_string)
    }

    /// qpdf `QPDF::decryptStream` method selection, after the caller has
    /// inspected the stream's own `/Crypt` filter.
    pub(crate) fn stream_method(&mut self, method: Option<EncryptionMode>) -> MethodChoice {
        let (method, encryption_v) = (method.unwrap_or(self.cf_stream), self.encryption_v);
        Self::select_method(method, &mut self.cf_stream, encryption_v)
    }

    /// Whether qpdf would prepend a decryption stage for this stream method,
    /// without applying the unknown-filter state rewrite.
    pub(crate) fn stream_method_transforms(&self, method: Option<EncryptionMode>) -> bool {
        let method = method.unwrap_or(self.cf_stream);
        self.encryption_v < 4 || !matches!(method, EncryptionMode::Identity)
    }

    /// qpdf `QPDF::compute_data_key` (`QPDF_encryption.cc:325-357`),
    /// Algorithm 3.1 from the PDF 1.7 Reference Manual.
    ///
    /// Not the same function as [`super::keys::per_object_key`], which
    /// truncates to `min(file_key.len() + 5, 16)` and so drops the four salt
    /// bytes from the length qpdf takes the minimum against. The two agree for
    /// every `/V` and `/R` pair the standard handler actually admits, because
    /// AES requires a 128-bit key and `min(21, 16) == min(25, 16)`.
    pub(crate) fn compute_data_key(&self, og: ObjectRef, use_aes: bool) -> Vec<u8> {
        let mut result = self.file_key.clone();
        if self.encryption_v >= 5 {
            return result;
        }

        let objid = og.number;
        let generation = u32::from(og.generation);
        result.push((objid & 0xff) as u8);
        result.push(((objid >> 8) & 0xff) as u8);
        result.push(((objid >> 16) & 0xff) as u8);
        result.push((generation & 0xff) as u8);
        result.push(((generation >> 8) & 0xff) as u8);
        if use_aes {
            result.extend_from_slice(b"sAlT");
        }

        let digest = crate::encryption::primitives::md5(&result);
        digest[..result.len().min(16)].to_vec()
    }

    pub(crate) fn with_object_cipher<T>(
        &mut self,
        og: ObjectRef,
        use_aes: bool,
        apply: impl FnOnce(StringCipher<'_>) -> Result<T>,
    ) -> Result<T> {
        let key = self.key_for_object(og, use_aes).to_vec();
        if !use_aes {
            return apply(StringCipher::Rc4 { key: &key });
        }
        if let Ok(key) = <&[u8; 32]>::try_from(key.as_slice()) {
            return apply(StringCipher::Aes256 { key });
        }
        let key = aes128_object_key(&key)?;
        apply(StringCipher::Aes128 { key: &key })
    }

    /// qpdf `QPDF::getKeyForObject` cache semantics. The cache key is only the
    /// object/generation pair; `use_aes` is intentionally omitted.
    pub(crate) fn key_for_object(&mut self, og: ObjectRef, use_aes: bool) -> &[u8] {
        if self.cached_key_og != Some(og) {
            self.cached_object_encryption_key = self.compute_data_key(og, use_aes);
            self.cached_key_og = Some(og);
        }
        &self.cached_object_encryption_key
    }
}

pub(crate) fn aes128_object_key(key: &[u8]) -> Result<[u8; 16]> {
    key.try_into().map_err(|_| {
        EncryptedError::Malformed {
            reason: "AES-128 object key is not 16 bytes".into(),
        }
        .into()
    })
}

/// qpdf `QPDF::encryption_method_e` (`QPDF.hh:436`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncryptionMode {
    Rc4,
    Aes128,
    Identity,
    Aes256,
    /// Unknown `/CFM` or crypt-filter name. qpdf defers judgment until the
    /// filter is actually referenced.
    Unknown,
}

impl EncryptionMode {
    /// qpdf `show_encryption_method()` spelling.
    pub(crate) fn qpdf_name(self) -> &'static str {
        match self {
            Self::Rc4 => "RC4",
            Self::Aes128 => "AESv2",
            Self::Aes256 => "AESv3",
            Self::Identity => "none",
            Self::Unknown => "unknown",
        }
    }
}

/// qpdf's parsed `EncryptionParameters` projection that exists before
/// password authentication completes. This is intentionally separate from
/// [`EncryptionState`]: an inspection may retain this state after a bad
/// password, but no decryption consumer may see an unauthenticated file key.
#[derive(Debug, Clone)]
pub(crate) struct EncryptionInspectionState {
    pub(crate) v: i64,
    pub(crate) r: i64,
    pub(crate) length_bits: i64,
    pub(crate) filter: String,
    pub(crate) permissions: Permissions,
    pub(crate) encrypt_metadata: bool,
    pub(crate) cf_stream: EncryptionMode,
    pub(crate) cf_string: EncryptionMode,
    pub(crate) cf_file: EncryptionMode,
    pub(crate) crypt_filters: BTreeMap<Vec<u8>, EncryptionMode>,
    pub(crate) stream_method: &'static str,
    pub(crate) string_method: &'static str,
    pub(crate) eff_method: &'static str,
    pub(crate) named_crypt_filters: Vec<(String, &'static str)>,
    pub(crate) user_password: Vec<u8>,
    pub(crate) user_password_matched: bool,
    pub(crate) owner_password_matched: bool,
    /// Whether the standard security handler uses RC4 or R=5, matching
    /// [`EncryptionState::weak_crypto`]'s classification. Computed here
    /// (before authentication) from the same revision/crypt-filter fields,
    /// so `Pdf::uses_weak_crypto` can report this after a `BadPassword` open
    /// that never populates the authenticated `EncryptionState`.
    pub(crate) weak_crypto: bool,
}

struct StandardHandlerInputsOwned {
    v: i64,
    r: i64,
    length_bits: i64,
    p: i32,
    id0: Vec<u8>,
    u: [u8; 32],
    o: [u8; 32],
    encrypt_metadata: bool,
}

impl StandardHandlerInputsOwned {
    fn borrowed(&self) -> StandardHandlerInputs<'_> {
        StandardHandlerInputs {
            v: self.v,
            r: self.r,
            length_bits: self.length_bits,
            p: self.p,
            id0: &self.id0,
            u: &self.u,
            o: &self.o,
            encrypt_metadata: self.encrypt_metadata,
        }
    }
}

struct StandardHandlerR5InputsOwned {
    u: [u8; 48],
    o: [u8; 48],
    ue: [u8; 32],
    oe: [u8; 32],
}

impl StandardHandlerR5InputsOwned {
    fn borrowed(&self) -> StandardHandlerR5Inputs<'_> {
        StandardHandlerR5Inputs {
            u: &self.u,
            o: &self.o,
            ue: &self.ue,
            oe: &self.oe,
        }
    }
}

/// Parse the qpdf-owned `/Encrypt` parameters before authentication.
pub(crate) fn parse_inspection_state(encrypt: &ObjectHandle) -> Result<EncryptionInspectionState> {
    let v = required_version_from_handle(encrypt)?;
    let r = required_revision_from_handle(encrypt)?;
    let permissions = Permissions::new(required_permissions_from_handle(encrypt)?);
    let filter = required_name_from_handle(encrypt, "Filter")?;
    let length = encrypt.try_get_key(b"/Length")?;
    let length_bits = effective_length_bits(v, &length)?;
    let encrypt_metadata = encrypt_metadata_flag_from_handle(encrypt)?;
    let crypt_filters = crypt_filter_modes_from_handle(encrypt, v)?;
    let (cf_stream, cf_string, cf_file) = if matches!(v, 4 | 5) {
        let stream =
            interpret_cf_selector_from_handle(&crypt_filters, &encrypt.try_get_key(b"/StmF")?)?;
        let string =
            interpret_cf_selector_from_handle(&crypt_filters, &encrypt.try_get_key(b"/StrF")?)?;
        let eff = encrypt.try_get_key(b"/EFF")?;
        let file = if eff.try_as_name()?.is_some() {
            interpret_cf_selector_from_handle(&crypt_filters, &eff)?
        } else {
            stream
        };
        (stream, string, file)
    } else {
        (
            EncryptionMode::Identity,
            EncryptionMode::Identity,
            EncryptionMode::Identity,
        )
    };
    let (stream_method, string_method, eff_method, named_crypt_filters) = if matches!(v, 4 | 5) {
        let named = crypt_filters
            .iter()
            .map(|(name, method)| {
                (
                    String::from_utf8_lossy(name).into_owned(),
                    method.qpdf_name(),
                )
            })
            .collect();
        (
            cf_stream.qpdf_name(),
            cf_string.qpdf_name(),
            cf_file.qpdf_name(),
            named,
        )
    } else {
        ("none", "none", "none", Vec::new())
    };
    // Same classification `authenticate` computes per revision branch
    // (`weak_crypto = revision == 5 || rc4_in_use()` for R5/R6, otherwise
    // `rc4_in_use()` alone) -- password-independent, so it is available
    // before authentication even attempts a candidate.
    let effective = |cf: EncryptionMode| {
        if v >= 4 {
            cf
        } else {
            EncryptionMode::Rc4
        }
    };
    let rc4_in_use = matches!(effective(cf_stream), EncryptionMode::Rc4)
        || matches!(effective(cf_string), EncryptionMode::Rc4)
        || crypt_filters
            .values()
            .any(|mode| matches!(mode, EncryptionMode::Rc4));
    let weak_crypto = if matches!(r, 5 | 6) {
        r == 5 || rc4_in_use
    } else {
        rc4_in_use
    };

    Ok(EncryptionInspectionState {
        v,
        r,
        length_bits,
        filter,
        permissions,
        encrypt_metadata,
        cf_stream,
        cf_string,
        cf_file,
        crypt_filters,
        stream_method,
        string_method,
        eff_method,
        named_crypt_filters,
        user_password: Vec::new(),
        user_password_matched: false,
        owner_password_matched: false,
        weak_crypto,
    })
}

/// Read-only snapshot of an encrypted document's `/Encrypt` parameters.
///
/// The snapshot is also available from the dedicated encryption-inspection
/// open path after password authentication fails; the match flags and
/// password bytes then retain qpdf's partial-initialization semantics.
#[derive(Debug, Clone)]
pub struct EncryptionInfo {
    pub v: i64,
    pub r: i64,
    pub length_bits: i64,
    pub filter: String,
    pub permissions: Permissions,
    pub encrypt_metadata: bool,
    /// qpdf `getTrimmedUserPassword()` bytes for the authenticated document.
    /// Empty means no recoverable/displayable user password (for example a
    /// V>=5 owner-password authentication).
    pub user_password: Vec<u8>,
    pub user_password_matched: bool,
    pub owner_password_matched: bool,
    pub stream_method: &'static str,
    pub string_method: &'static str,
    pub eff_method: &'static str,
    pub named_crypt_filters: Vec<(String, &'static str)>,
}

/// Result of qpdf's `initializeEncryption` plus password authentication.
pub(crate) struct AuthenticationResult {
    pub(crate) state: EncryptionState,
    pub(crate) perms_warning: Option<String>,
}

/// Authenticate one password candidate and construct the canonical encryption
/// state. The reader supplies only document-owned dictionary values and the
/// canonical `/ID` handle selected from the trailer; all Standard-handler
/// parsing and key derivation stays in this module.
pub(crate) fn authenticate(
    encrypt: &ObjectHandle,
    id: &ObjectHandle,
    encrypt_ref: Option<ObjectRef>,
    password: &[u8],
    password_mode: PasswordMode,
    password_is_hex_key: bool,
) -> Result<AuthenticationResult> {
    let raw_password = password;
    let parsed = parse_inspection_state(encrypt)?;
    let revision = parsed.r;
    let version = parsed.v;
    let permissions = parsed.permissions;
    let cf_stream = parsed.cf_stream;
    let cf_string = parsed.cf_string;
    let cf_file = parsed.cf_file;
    let crypt_filters = parsed.crypt_filters.clone();

    let effective = |cf: EncryptionMode| {
        if version >= 4 {
            cf
        } else {
            EncryptionMode::Rc4
        }
    };
    let rc4_in_use = || {
        matches!(effective(cf_stream), EncryptionMode::Rc4)
            || matches!(effective(cf_string), EncryptionMode::Rc4)
            || crypt_filters
                .values()
                .any(|mode| matches!(mode, EncryptionMode::Rc4))
    };
    let password = if password_is_hex_key {
        Vec::new()
    } else {
        password_bytes_for_read(password, password_mode)?
    };

    let (
        file_key,
        encrypt_metadata,
        weak_crypto,
        user_password_matched,
        owner_password_matched,
        user_password,
        id0,
    ) = if password_is_hex_key {
        // qpdf `--password-is-hex-key`: the value passed via --password is
        // the precomputed file encryption key as hex, NOT a user/owner
        // password. We skip ALL password→key derivation (Algorithm 2 /
        // 2.A / 2.B / 6 / 7) and the layer-2 user/owner attempt +
        // bad-password ordering block entirely. This is a SEPARATE
        // sibling branch: the `else` below preserves layer-2's password
        // authentication logic (flpdf-9hc.3.21).
        //
        // revision / crypt_filters / encrypt_ref / permissions and the
        // crypt-filter methods are already determined above and do NOT
        // depend on the password. /EncryptMetadata is likewise
        // password-independent; compute it with the SAME revision-aware
        // split layer-2 uses.
        let file_key = decode_hex_file_key(raw_password)?;
        let (encrypt_metadata, weak_crypto, id0) = if matches!(revision, 5 | 6) {
            let encrypt_metadata = encrypt_metadata_flag_from_handle(encrypt)?;
            // Same weak-crypto classification as layer-2's R5/R6 branch.
            (encrypt_metadata, revision == 5 || rc4_in_use(), None)
        } else {
            let id0 = first_file_id_handle(id)?;
            let inputs = standard_handler_inputs_from_handle(encrypt, &id0)?;
            let borrowed = inputs.borrowed();
            (borrowed.encrypt_metadata, rc4_in_use(), Some(id0))
        };
        // A raw key bypasses authentication, so neither the user nor the
        // owner password was matched. qpdf likewise reports no password
        // match for `--password-is-hex-key`; report both as false.
        (
            file_key,
            encrypt_metadata,
            weak_crypto,
            false,
            false,
            Vec::new(),
            id0,
        )
    } else if matches!(revision, 5 | 6) {
        // Authentication error behavior must match qpdf (see
        // flpdf-9hc.3.21):
        //
        //   1. Password authentication runs FIRST.  If neither the user nor
        //      the owner password authenticates, return `BadPassword`.
        //   2. A wrong-length `/U` or `/O` entry on this authentication
        //      path is reported as `BadPassword` (an unusable credential
        //      entry is indistinguishable from a wrong password to a
        //      caller), not `Malformed`.  This is scoped to the auth path
        //      via `standard_handler_r5_inputs` (its only caller); all
        //      other `Malformed` reclassification is intentionally NOT done
        //      (e.g. `/UE`/`/OE` length errors stay `Malformed`).
        //
        // Keep this authentication ordering identical in the `else`
        // (V<5 / V=4) branch below.
        let inputs = standard_handler_r5_inputs_from_handle(encrypt)
            .map_err(map_uo_length_to_bad_password)?;
        let encrypt_metadata = encrypt_metadata_flag_from_handle(encrypt)?;
        let inputs = inputs.borrowed();
        let weak_crypto = revision == 5 || rc4_in_use();
        let user_attempt = if revision == 5 {
            check_user_password_r5(&password, &inputs)
        } else {
            check_user_password_r6(&password, &inputs)
        };
        let owner_attempt = if revision == 5 {
            check_owner_password_r5(&password, &inputs)
        } else {
            check_owner_password_r6(&password, &inputs)
        };
        let user_password_matched = user_attempt.is_ok();
        let owner_password_matched = owner_attempt.is_ok();
        let file_key = match (user_attempt, owner_attempt) {
            (Ok(key), _) => key,
            (Err(_), Ok(key)) => key,
            (Err(user_err), Err(_owner_err)) => return Err(user_err),
        };
        let user_password = if user_password_matched {
            password.clone()
        } else {
            Vec::new()
        };
        (
            file_key,
            encrypt_metadata,
            weak_crypto,
            user_password_matched,
            owner_password_matched,
            user_password,
            None,
        )
    } else {
        let id0 = first_file_id_handle(id)?;
        let inputs = standard_handler_inputs_from_handle(encrypt, &id0)?;
        let borrowed = inputs.borrowed();
        let encrypt_metadata = borrowed.encrypt_metadata;
        let weak_crypto = rc4_in_use();
        // Password authentication runs before any state is committed, so
        // both failing attempts return `BadPassword`.
        let v4_path = inputs.v == 4 && inputs.r == 4;
        let user_attempt = if v4_path {
            check_user_password_v4(&password, &borrowed)
        } else {
            check_user_password(&password, &borrowed)
        };
        let owner_attempt = if v4_path {
            check_owner_password_v4_with_user_password(&password, &borrowed)
        } else {
            check_owner_password_with_user_password(&password, &borrowed)
        };
        let user_password_matched = user_attempt.is_ok();
        let owner_password_matched = owner_attempt.is_ok();
        let (file_key, user_password) = match (user_attempt, owner_attempt) {
            (Ok(key), _) => (key, password.clone()),
            (Err(_), Ok((key, recovered))) => (key, recovered),
            (Err(user_err), Err(_owner_err)) => return Err(user_err),
        };
        (
            file_key,
            encrypt_metadata,
            weak_crypto,
            user_password_matched,
            owner_password_matched,
            user_password,
            Some(id0),
        )
    };

    // qpdf's raw-key branch bypasses password recovery entirely, including
    // the R=6 /Perms validation that belongs to the password-authenticated
    // path (QPDF_encryption.cc:907-950).
    let perms_warning = if revision == 6 && !password_is_hex_key {
        r6_perms_warning_from_handle(encrypt, &file_key, permissions, encrypt_metadata)?
    } else {
        None
    };
    Ok(AuthenticationResult {
        state: EncryptionState {
            file_key,
            encryption_v: version,
            encryption_r: revision,
            cf_stream,
            cf_string,
            cf_file,
            crypt_filters,
            encrypt_metadata,
            encrypt_ref,
            weak_crypto,
            permissions,
            id0,
            user_password_matched,
            owner_password_matched,
            user_password,
            cached_object_encryption_key: Vec::new(),
            cached_key_og: None,
        },
        perms_warning,
    })
}

fn standard_handler_inputs_from_handle(
    encrypt: &ObjectHandle,
    id0: &[u8],
) -> Result<StandardHandlerInputsOwned> {
    let filter = required_name_from_handle(encrypt, "Filter")?;
    let v = required_integer_from_handle(encrypt, "V")?;
    let r = required_integer_from_handle(encrypt, "R")?;
    if filter != "Standard" || !matches!((v, r), (1 | 2, 2 | 3) | (4, 4)) {
        return Err(crate::error::EncryptedError::UnsupportedHandler {
            filter,
            v,
            r,
            cfm: crypt_filter_method_from_handle(encrypt)?,
        }
        .into());
    }
    let length = encrypt.try_get_key(b"/Length")?;
    let length_bits = effective_length_bits(v, &length)?;
    let p = required_permissions_from_handle(encrypt)?;
    let u = required_32_byte_string_from_handle(encrypt, "U")?;
    let o = required_32_byte_string_from_handle(encrypt, "O")?;
    let encrypt_metadata = encrypt_metadata_flag_from_handle(encrypt)?;
    Ok(StandardHandlerInputsOwned {
        v,
        r,
        length_bits,
        p,
        id0: id0.to_vec(),
        u,
        o,
        encrypt_metadata,
    })
}

/// qpdf's `initializeEncryption` length selection
/// (`QPDF_encryption.cc:835-853`). V=1 always uses 40 bits, V=4 uses 128,
/// and V=5 uses 256. Other versions honor a valid `/Length`; missing,
/// malformed, or out-of-range values fall back to qpdf's 128-bit guess.
pub(crate) fn effective_length_bits(v: i64, length: &ObjectHandle) -> Result<i64> {
    if v <= 1 {
        return Ok(40);
    }
    if v == 4 {
        return Ok(128);
    }
    if v == 5 {
        return Ok(256);
    }
    match length.try_as_integer()? {
        Some(value) if (40..=128).contains(&value) && value % 8 == 0 => Ok(value),
        _ => Ok(128),
    }
}

fn standard_handler_r5_inputs_from_handle(
    encrypt: &ObjectHandle,
) -> Result<StandardHandlerR5InputsOwned> {
    let filter = required_name_from_handle(encrypt, "Filter")?;
    let v = required_integer_from_handle(encrypt, "V")?;
    let r = required_integer_from_handle(encrypt, "R")?;
    if filter != "Standard" || v != 5 || !matches!(r, 5 | 6) {
        return Err(crate::error::EncryptedError::UnsupportedHandler {
            filter,
            v,
            r,
            cfm: crypt_filter_method_from_handle(encrypt)?,
        }
        .into());
    }
    Ok(StandardHandlerR5InputsOwned {
        u: required_48_byte_string_from_handle(encrypt, "U")?,
        o: required_48_byte_string_from_handle(encrypt, "O")?,
        ue: required_32_byte_string_from_handle(encrypt, "UE")?,
        oe: required_32_byte_string_from_handle(encrypt, "OE")?,
    })
}

/// Scoped to the V=5 R=5/R=6 authentication path (the sole caller of
/// `standard_handler_r5_inputs`): a `/U` or `/O` entry that is not exactly
/// 48 bytes is an unusable credential entry that is indistinguishable, from a
/// caller's perspective, from supplying the wrong password — qpdf reports
/// "invalid password" here, so we map to `BadPassword` for parity.
///
/// Only the `/U` / `/O` *length* error is remapped. `/UE` / `/OE` length
/// errors, missing entries, and non-string entries stay `Malformed`: those are
/// genuine structural defects, not credential mismatches. No broader
/// `Malformed` reclassification is performed.
fn map_uo_length_to_bad_password(err: crate::Error) -> crate::Error {
    match &err {
        crate::Error::Encrypted(crate::error::EncryptedError::Malformed { reason })
            if reason == "/U entry is not 48 bytes" || reason == "/O entry is not 48 bytes" =>
        {
            crate::error::EncryptedError::BadPassword.into()
        }
        _ => err,
    }
}

fn decode_hex_file_key(raw: &[u8]) -> Result<Vec<u8>> {
    let trimmed: Vec<u8> = raw
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let key = hex::decode(&trimmed).map_err(|err| crate::error::EncryptedError::Malformed {
        reason: format!("--password-is-hex-key: --password is not valid hex ({err})"),
    })?;
    if key.len() > 32 {
        return Err(crate::error::EncryptedError::Malformed {
            reason: format!(
                "--password-is-hex-key: decoded key is {} bytes; the Standard security handler key is at most 32 bytes",
                key.len()
            ),
        }
        .into());
    }
    Ok(key)
}

fn required_integer_from_handle(dict: &ObjectHandle, key: &'static str) -> Result<i64> {
    let key_name = format!("/{key}");
    let value = dict.try_get_key(key_name.as_bytes())?;
    match value.try_as_integer()? {
        Some(value) => Ok(value),
        None if value.is_null() => Err(crate::error::EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
        None => Err(crate::error::EncryptedError::Malformed {
            reason: format!("/{key} entry is not an integer"),
        }
        .into()),
    }
}

fn required_revision_from_handle(encrypt: &ObjectHandle) -> Result<i64> {
    required_integer_from_handle(encrypt, "R")
}

fn required_version_from_handle(encrypt: &ObjectHandle) -> Result<i64> {
    required_integer_from_handle(encrypt, "V")
}

fn required_name_from_handle(dict: &ObjectHandle, key: &'static str) -> Result<String> {
    let key_name = format!("/{key}");
    let value = dict.try_get_key(key_name.as_bytes())?;
    match value.try_as_name()? {
        Some(name) => String::from_utf8(name).map_err(|_| {
            crate::error::EncryptedError::Malformed {
                reason: format!("/{key} entry is not valid UTF-8"),
            }
            .into()
        }),
        None if value.is_null() => Err(crate::error::EncryptedError::Malformed {
            reason: format!("missing /{key} entry"),
        }
        .into()),
        None => Err(crate::error::EncryptedError::Malformed {
            reason: format!("/{key} entry is not a name"),
        }
        .into()),
    }
}

fn required_32_byte_string_from_handle(dict: &ObjectHandle, key: &'static str) -> Result<[u8; 32]> {
    let key_name = format!("/{key}");
    let value = dict.try_get_key(key_name.as_bytes())?;
    value.try_dereference()?;
    let Some(bytes) = value.as_string() else {
        return Err(if value.is_null() {
            crate::error::EncryptedError::Malformed {
                reason: format!("missing /{key} entry"),
            }
        } else {
            crate::error::EncryptedError::Malformed {
                reason: format!("/{key} entry is not a string"),
            }
        }
        .into());
    };
    bytes.as_slice().try_into().map_err(|_| {
        crate::error::EncryptedError::Malformed {
            reason: format!("/{key} entry is not 32 bytes"),
        }
        .into()
    })
}

fn required_48_byte_string_from_handle(dict: &ObjectHandle, key: &'static str) -> Result<[u8; 48]> {
    let key_name = format!("/{key}");
    let value = dict.try_get_key(key_name.as_bytes())?;
    value.try_dereference()?;
    let Some(bytes) = value.as_string() else {
        return Err(if value.is_null() {
            crate::error::EncryptedError::Malformed {
                reason: format!("missing /{key} entry"),
            }
        } else {
            crate::error::EncryptedError::Malformed {
                reason: format!("/{key} entry is not a string"),
            }
        }
        .into());
    };
    if bytes.len() < 48 {
        return Err(crate::error::EncryptedError::Malformed {
            reason: format!("/{key} entry is not 48 bytes"),
        }
        .into());
    }
    // cov:ignore-start: the preceding length guard makes this slice exactly 48 bytes
    bytes[..48].try_into().map_err(|_| {
        crate::error::EncryptedError::Malformed {
            reason: format!("/{key} entry is not 48 bytes"),
        }
        .into()
    })
    // cov:ignore-end
}

fn encrypt_metadata_flag_from_handle(encrypt: &ObjectHandle) -> Result<bool> {
    let value = encrypt.try_get_key(b"/EncryptMetadata")?;
    value.try_dereference()?;
    match value.as_boolean() {
        Some(value) => Ok(value),
        None if value.is_null() => Ok(true),
        None => Err(crate::error::EncryptedError::Malformed {
            reason: "/EncryptMetadata entry is not a boolean".into(),
        }
        .into()),
    }
}

fn required_permissions_from_handle(encrypt: &ObjectHandle) -> Result<i32> {
    i32::try_from(required_integer_from_handle(encrypt, "P")?).map_err(|_| {
        crate::error::EncryptedError::Malformed {
            reason: "/P entry is out of i32 range".into(),
        }
        .into()
    })
}

fn r6_perms_warning_from_handle(
    encrypt: &ObjectHandle,
    file_key: &[u8],
    permissions: Permissions,
    encrypt_metadata: bool,
) -> Result<Option<String>> {
    let Some(entries) = encrypt.try_as_dictionary()? else {
        return Ok(None);
    };
    let Some(perms) = entries.get(b"/Perms".as_slice()).cloned() else {
        return Ok(None);
    };
    perms.try_dereference()?;
    let Some(bytes) = perms.as_string() else {
        return Ok(Some("R=6 /Perms entry is not a string".into()));
    };
    let Ok(mut block) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return Ok(Some("R=6 /Perms entry is not 16 bytes".into()));
    };
    let Ok(file_key) = <&[u8; 32]>::try_from(file_key) else {
        return Ok(Some(
            "R=6 /Perms cannot be verified with non-256-bit file key".into(),
        ));
    };
    super::primitives::aes256_ecb_decrypt_block(file_key, &mut block);
    let perms_p = i32::from_le_bytes(block[..4].try_into().expect("slice length checked"));
    let perms_metadata = match block[8] {
        b'T' => true,
        b'F' => false,
        _ => {
            return Ok(Some(
                "R=6 /Perms encrypted-metadata flag is not T or F".into(),
            ))
        }
    };
    if perms_p != permissions.raw() {
        return Ok(Some(format!(
            "R=6 /Perms permissions value {perms_p} does not match /P {}",
            permissions.raw()
        )));
    }
    if block[4..8] != [0xff; 4] {
        return Ok(Some("R=6 /Perms reserved bytes are invalid".into()));
    }
    if perms_metadata != encrypt_metadata {
        return Ok(Some(
            "R=6 /Perms encrypted-metadata flag does not match /EncryptMetadata".into(),
        ));
    }
    if &block[9..12] != b"adb" {
        return Ok(Some("R=6 /Perms magic bytes are not 'adb'".into()));
    }
    Ok(None)
}

/// qpdf's `/ID[0]` value and whether the trailer satisfied its two-element
/// array contract. An invalid ID still carries a real empty fallback value;
/// `valid` keeps that value distinct from a valid empty string so the reader
/// can emit qpdf's warning exactly once during encryption initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FirstFileId {
    pub(crate) value: Vec<u8>,
    pub(crate) valid: bool,
}

/// Read qpdf's `/ID[0]` value from the already-selected canonical trailer key.
/// The caller supplies `QPDF::getTrailer().getKey("/ID")` rather than the
/// whole trailer so unrelated entries do not affect this one-value lookup.
///
/// qpdf accepts exactly two array elements and only requires the first element
/// to be a string (`QPDF_encryption.cc:738-746`). Any other shape is a
/// non-fatal warning with an empty `id1` fallback. The second element is not
/// inspected because qpdf does not inspect it either.
pub(crate) fn first_file_id_handle_with_status(id: &ObjectHandle) -> Result<FirstFileId> {
    let Some(ids) = id.try_as_array()? else {
        return Ok(FirstFileId {
            value: Vec::new(),
            valid: false,
        });
    };
    if ids.len() != 2 {
        return Ok(FirstFileId {
            value: Vec::new(),
            valid: false,
        });
    }
    let first = &ids[0];
    first.try_dereference()?;
    let Some(value) = first.as_string() else {
        return Ok(FirstFileId {
            value: Vec::new(),
            valid: false,
        });
    };
    Ok(FirstFileId { value, valid: true })
}

/// Return only qpdf's effective `id1` value for callers that do not own the
/// document warning sink. Invalid trailer shapes intentionally return the
/// empty value selected by qpdf rather than an error.
pub(crate) fn first_file_id_handle(id: &ObjectHandle) -> Result<Vec<u8>> {
    Ok(first_file_id_handle_with_status(id)?.value)
}

#[cfg(test)]
mod tests {
    use super::parse_inspection_state;
    use crate::ObjectHandle;

    #[test]
    fn v2_missing_length_uses_qpdf_128_bit_fallback() {
        let encrypt = ObjectHandle::dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::name(b"Standard".to_vec()),
            ),
            (b"/V".to_vec(), ObjectHandle::integer(2)),
            (b"/R".to_vec(), ObjectHandle::integer(3)),
            (b"/P".to_vec(), ObjectHandle::integer(-4)),
        ]);

        let state = parse_inspection_state(&encrypt).expect("parse encryption dictionary");

        assert_eq!(state.length_bits, 128);
    }
}
