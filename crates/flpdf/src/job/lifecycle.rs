//! qpdf correspondence: `QPDFJob` shared state and completion boundary.
//!
//! This module owns the state that qpdf keeps on `QPDFJob` itself rather than
//! on an individual CLI route: the message prefix, logger, progress callback,
//! warning aggregation, and the single warning-completion summary. JSON and
//! ordinary page-inspection dispatch are layered on top of this state; write,
//! page-transform, and remaining inspection consumers are later job slices.

use super::attachments::{AttachmentAddOptions, AttachmentCopyOptions};
use super::json::{JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData};
use super::overlay::{apply_overlay_specs, OverlayKind, OverlaySpec};
use super::page_range::PageRange;
use super::page_specs::PageSpecInput;
use super::page_split::SplitPageOptions;
use super::resource_pruning::RemoveUnreferencedResources;
use super::rotate::apply_rotate_to_pages;
use super::rotate_spec::RotateSpec;
use crate::encryption::{EncryptMethod, EncryptParams};
use crate::json_inspect::{DecodeLevel as JsonDecodeLevel, JsonKey, JsonObjectSelector};
use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
use crate::{
    AcroFormDocumentHelper, Error, ObjectStreamMode, PageDocumentHelper, Pdf, PdfOpenOptions,
    PdfWriter, QPDFLogger, ReadSeek, Result, Severity, UsageError, WriterConfiguration,
};
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

type ProgressHandler = Box<dyn FnMut(u8) -> Result<()> + 'static>;
type SharedProgressHandler = Rc<RefCell<ProgressHandler>>;

/// The single document type owned by a qpdf job.
///
/// The erased reader preserves lazy file/JSON reads while allowing qpdf's
/// file, empty, JSON, and page-selection inputs to share one lifecycle.
pub type JobDocument = Pdf<Box<dyn ReadSeek>>;

struct JobOutputPipeline(PipelineHandle);

impl Pipeline for JobOutputPipeline {
    fn identifier(&self) -> &str {
        "qpdf job output"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.0.write(data)
    }

    fn finish(&mut self) -> PipelineResult<()> {
        self.0.finish()
    }
}

struct JobOutputWriter(PipelineHandle);

impl Write for JobOutputWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.write(data).map_err(std::io::Error::other)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Portable writer/input state populated by the qpdf job argv/JSON boundary.
///
/// This is deliberately smaller than the CLI's clap model. It owns the
/// settings exercised by `qpdf/qpdfjob-ctest.c`; full command-line transform
/// dispatch remains in the operation-specific job slices.
#[derive(Debug, Clone, Default)]
struct JobConfiguration {
    input_file: Option<PathBuf>,
    empty_input: bool,
    output_file: Option<PathBuf>,
    password: Vec<u8>,
    verbose: bool,
    json_input: bool,
    update_from_json: Option<PathBuf>,
    replace_input: bool,
    check: bool,
    require_output: bool,
    progress: bool,
    split_pages: Option<usize>,
    rotations: Vec<RotateSpec>,
    remove_restrictions: bool,
    writer: WriterConfiguration,
    linearize: bool,
    linearize_pass1: Option<PathBuf>,
    allow_weak_crypto: bool,
    page_specs: Vec<JobPageConfig>,
    collate: Option<usize>,
    overlays: Vec<JobOverlayConfig>,
    underlays: Vec<JobOverlayConfig>,
    attachments_to_add: Vec<AttachmentAddOptions>,
    attachments_to_copy: Vec<JobCopyAttachmentsConfig>,
    attachments_to_remove: Vec<Vec<u8>>,
    remove_unreferenced_resources: RemoveUnreferencedResources,
    set_page_labels: Option<Vec<String>>,
    remove_page_labels: bool,
    json_version: Option<i32>,
    json_output: bool,
    json_decode_level: crate::writer::DecodeLevel,
    json_decode_level_set: bool,
    json_keys: Vec<JsonKey>,
    json_objects: Vec<JsonObjectSelector>,
    json_stream_data: JsonStreamData,
    json_stream_prefix: Option<String>,
    test_json_schema: bool,
    show_encryption_key: bool,
    show_encryption: bool,
}

#[derive(Debug, Clone)]
struct JobPageConfig {
    path: PathBuf,
    password: Vec<u8>,
    range: PageRange,
}

#[derive(Debug, Clone)]
struct JobOverlayConfig {
    path: PathBuf,
    password: Vec<u8>,
    from: PageRange,
    to: PageRange,
    repeat: Option<PageRange>,
    kind: OverlayKind,
}

#[derive(Debug, Clone)]
struct JobCopyAttachmentsConfig {
    path: PathBuf,
    password: Vec<u8>,
    prefix: Vec<u8>,
}

fn job_schema_scalar() -> crate::json::Json {
    crate::json::Json::make_string("qpdf job option")
}

fn job_schema_dictionary(
    entries: impl IntoIterator<Item = (&'static str, crate::json::Json)>,
) -> crate::json::Json {
    let dictionary = crate::json::Json::make_dictionary();
    for (key, value) in entries {
        dictionary
            .add_dictionary_member(key, value)
            .expect("static qpdf job schema dictionary is valid");
    }
    dictionary
}

fn job_schema_array(item: crate::json::Json) -> crate::json::Json {
    let array = crate::json::Json::make_array();
    array
        .add_array_element(item)
        .expect("static qpdf job schema array is valid");
    array
}

/// Build qpdf 11.9.0's generated `JOB_SCHEMA` shape. The leaf strings are
/// descriptions in qpdf and deliberately accept any JSON scalar; concrete
/// types and choices are checked by the generated handler semantics below.
fn job_json_schema() -> crate::json::Json {
    let scalar = job_schema_scalar();
    let schema = crate::json::Json::make_dictionary();
    for key in [
        "inputFile",
        "password",
        "passwordFile",
        "empty",
        "jsonInput",
        "outputFile",
        "replaceInput",
        "qdf",
        "preserveUnreferenced",
        "newlineBeforeEndstream",
        "normalizeContent",
        "streamData",
        "compressStreams",
        "recompressFlate",
        "decodeLevel",
        "decrypt",
        "deterministicId",
        "staticAesIv",
        "staticId",
        "noOriginalObjectIds",
        "copyEncryption",
        "encryptionFilePassword",
        "linearize",
        "linearizePass1",
        "objectStreams",
        "minVersion",
        "forceVersion",
        "progress",
        "splitPages",
        "jsonOutput",
        "removeRestrictions",
        "check",
        "checkLinearization",
        "filteredStreamData",
        "rawStreamData",
        "showEncryption",
        "showEncryptionKey",
        "showLinearization",
        "showNpages",
        "showObject",
        "showPages",
        "showXref",
        "withImages",
        "listAttachments",
        "json",
        "jsonStreamData",
        "jsonStreamPrefix",
        "updateFromJson",
        "allowWeakCrypto",
        "keepFilesOpen",
        "keepFilesOpenThreshold",
        "noWarn",
        "verbose",
        "testJsonSchema",
        "ignoreXrefStreams",
        "passwordIsHexKey",
        "passwordMode",
        "suppressPasswordRecovery",
        "suppressRecovery",
        "coalesceContents",
        "compressionLevel",
        "externalizeInlineImages",
        "iiMinBytes",
        "oiMinArea",
        "oiMinHeight",
        "oiMinWidth",
        "removeUnreferencedResources",
        "preserveUnreferencedResources",
        "requiresPassword",
        "isEncrypted",
        "keepInlineImages",
        "optimizeImages",
        "removePageLabels",
        "reportMemoryUsage",
        "rotate",
        "collate",
        "flattenAnnotations",
        "flattenRotation",
        "generateAppearances",
        "warningExit0",
        "jobJsonFile",
    ] {
        schema
            .add_dictionary_member(key, scalar.clone())
            .expect("static qpdf job schema key is valid");
    }

    schema
        .add_dictionary_member("jsonKey", job_schema_array(scalar.clone()))
        .expect("static qpdf job schema jsonKey is valid");
    schema
        .add_dictionary_member("jsonObject", job_schema_array(scalar.clone()))
        .expect("static qpdf job schema jsonObject is valid");
    schema
        .add_dictionary_member("removeAttachment", job_schema_array(scalar.clone()))
        .expect("static qpdf job schema removeAttachment is valid");
    schema
        .add_dictionary_member("setPageLabels", job_schema_array(scalar.clone()))
        .expect("static qpdf job schema setPageLabels is valid");

    let attachment = job_schema_dictionary([
        ("file", scalar.clone()),
        ("creationdate", scalar.clone()),
        ("description", scalar.clone()),
        ("filename", scalar.clone()),
        ("key", scalar.clone()),
        ("mimetype", scalar.clone()),
        ("moddate", scalar.clone()),
        ("replace", scalar.clone()),
    ]);
    schema
        .add_dictionary_member("addAttachment", job_schema_array(attachment))
        .expect("static qpdf job schema addAttachment is valid");

    let copy_attachments = job_schema_dictionary([
        ("file", scalar.clone()),
        ("password", scalar.clone()),
        ("prefix", scalar.clone()),
    ]);
    schema
        .add_dictionary_member("copyAttachmentsFrom", job_schema_array(copy_attachments))
        .expect("static qpdf job schema copyAttachmentsFrom is valid");

    let pages = job_schema_dictionary([
        ("file", scalar.clone()),
        ("password", scalar.clone()),
        ("range", scalar.clone()),
    ]);
    schema
        .add_dictionary_member("pages", job_schema_array(pages))
        .expect("static qpdf job schema pages is valid");

    let under_overlay = job_schema_dictionary([
        ("file", scalar.clone()),
        ("password", scalar.clone()),
        ("from", scalar.clone()),
        ("repeat", scalar.clone()),
        ("to", scalar.clone()),
    ]);
    schema
        .add_dictionary_member("overlay", job_schema_array(under_overlay.clone()))
        .expect("static qpdf job schema overlay is valid");
    schema
        .add_dictionary_member("underlay", job_schema_array(under_overlay))
        .expect("static qpdf job schema underlay is valid");

    let encrypt_40 = job_schema_dictionary([
        ("annotate", scalar.clone()),
        ("extract", scalar.clone()),
        ("modify", scalar.clone()),
        ("print", scalar.clone()),
    ]);
    let encrypt_128 = job_schema_dictionary([
        ("accessibility", scalar.clone()),
        ("annotate", scalar.clone()),
        ("assemble", scalar.clone()),
        ("cleartextMetadata", scalar.clone()),
        ("extract", scalar.clone()),
        ("form", scalar.clone()),
        ("modifyOther", scalar.clone()),
        ("modify", scalar.clone()),
        ("print", scalar.clone()),
        ("forceV4", scalar.clone()),
        ("useAes", scalar.clone()),
    ]);
    let encrypt_256 = job_schema_dictionary([
        ("accessibility", scalar.clone()),
        ("annotate", scalar.clone()),
        ("assemble", scalar.clone()),
        ("cleartextMetadata", scalar.clone()),
        ("extract", scalar.clone()),
        ("form", scalar.clone()),
        ("modifyOther", scalar.clone()),
        ("modify", scalar.clone()),
        ("print", scalar.clone()),
        ("allowInsecure", scalar.clone()),
        ("forceR5", scalar.clone()),
    ]);
    let encrypt = job_schema_dictionary([
        ("userPassword", scalar.clone()),
        ("ownerPassword", scalar.clone()),
        ("Bits", crate::json::Json::make_null()),
        ("40bit", encrypt_40),
        ("128bit", encrypt_128),
        ("256bit", encrypt_256),
    ]);
    schema
        .add_dictionary_member("encrypt", encrypt)
        .expect("static qpdf job schema encrypt is valid");
    schema
}

fn validate_job_json_schema(value: &crate::json::Json) -> Result<()> {
    let mut errors = Vec::new();
    if value.check_schema_with_flags(
        &job_json_schema(),
        crate::json::SchemaFlags::OPTIONAL,
        &mut errors,
    ) {
        return Ok(());
    }
    let mut message = "qpdf: job json has errors:".to_owned();
    for error in errors {
        message.push_str("\n  ");
        message.push_str(&error.to_string());
    }
    Err(Error::Usage(UsageError::new(message)))
}

fn job_json_members(
    value: &crate::json::Json,
) -> std::collections::BTreeMap<Vec<u8>, crate::json::Json> {
    let mut members = std::collections::BTreeMap::new();
    value.for_each_dict_item(|key, item| {
        members.insert(key.to_vec(), item);
    });
    members
}

fn job_json_string(
    members: &std::collections::BTreeMap<Vec<u8>, crate::json::Json>,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    let Some(value) = members.get(key) else {
        return Ok(None);
    };
    value.get_string().map(Some).ok_or_else(|| {
        Error::Usage(UsageError::new(format!(
            ".{}: value must be a string",
            String::from_utf8_lossy(key)
        )))
    })
}

fn job_json_bare(
    members: &std::collections::BTreeMap<Vec<u8>, crate::json::Json>,
    key: &[u8],
) -> Result<bool> {
    let Some(value) = members.get(key) else {
        return Ok(false);
    };
    let path = format!(".{}", String::from_utf8_lossy(key));
    match value.get_string() {
        Some(value) if value.is_empty() => Ok(true),
        Some(_) => Err(Error::Usage(UsageError::new(format!(
            "{path}: value must be the empty string"
        )))),
        None => Err(Error::Usage(UsageError::new(format!(
            "{path}: value must be a string"
        )))),
    }
}

fn job_json_choice(
    members: &std::collections::BTreeMap<Vec<u8>, crate::json::Json>,
    key: &[u8],
    choices: &[&str],
    required: bool,
) -> Result<Option<String>> {
    let Some(value) = members.get(key) else {
        return Ok(None);
    };
    let path = format!(".{}", String::from_utf8_lossy(key));
    let value = value
        .get_string()
        .ok_or_else(|| Error::Usage(UsageError::new(format!("{path}: value must be a string"))))?;
    let value = String::from_utf8_lossy(&value).into_owned();
    if !required && value.is_empty() {
        return Ok(Some(value));
    }
    if choices.iter().any(|choice| *choice == value) {
        return Ok(Some(value));
    }
    Err(Error::Usage(UsageError::new(format!(
        "{path}: unexpected value; expected one of {}",
        choices.join(", ")
    ))))
}

fn job_json_items(value: &crate::json::Json) -> Vec<crate::json::Json> {
    let mut items = Vec::new();
    if value.for_each_array_item(|item| items.push(item)) {
        items
    } else {
        vec![value.clone()]
    }
}

fn job_json_required_string(
    members: &std::collections::BTreeMap<Vec<u8>, crate::json::Json>,
    key: &[u8],
    path: &str,
) -> Result<Vec<u8>> {
    job_json_string(members, key)?
        .ok_or_else(|| Error::Usage(UsageError::new(format!("{path}: value must be a string"))))
}

fn job_json_range(value: Option<&crate::json::Json>, path: &str) -> Result<PageRange> {
    let bytes = value
        .map(|value| {
            value.get_string().ok_or_else(|| {
                Error::Usage(UsageError::new(format!("{path}: value must be a string")))
            })
        })
        .transpose()?
        .unwrap_or_default();
    let value = String::from_utf8_lossy(&bytes);
    PageRange::parse(&value)
        .map_err(|error| Error::Usage(UsageError::new(format!("{path}: {error}"))))
}

fn job_json_yn(
    members: &std::collections::BTreeMap<Vec<u8>, crate::json::Json>,
    key: &[u8],
) -> Result<Option<bool>> {
    Ok(job_json_choice(members, key, &["y", "n"], true)?.map(|value| value == "y"))
}

fn job_json_modify_permission(
    value: &str,
    permissions: &mut crate::PermissionsConfig,
) -> Result<()> {
    let (modify, annotate, forms, assembly) = match value {
        "all" => (true, true, true, true),
        "annotate" => (false, true, true, true),
        "form" => (false, false, true, true),
        "assembly" => (false, false, false, true),
        "none" => (false, false, false, false),
        other => {
            return Err(Error::Usage(UsageError::new(format!(
                ".encrypt: unexpected value; expected one of all, annotate, form, assembly, none (got {other})"
            ))))
        }
    };
    permissions.modify_contents = modify;
    permissions.annotate = annotate;
    permissions.fill_forms = forms;
    permissions.assemble = assembly;
    Ok(())
}

fn job_json_print_permission(
    value: &str,
    permissions: &mut crate::PermissionsConfig,
) -> Result<()> {
    permissions.print = match value {
        "full" => crate::PrintPermission::High,
        "low" => crate::PrintPermission::Low,
        "none" => crate::PrintPermission::None,
        other => {
            return Err(Error::Usage(UsageError::new(format!(
                ".encrypt: unexpected value; expected one of full, low, none (got {other})"
            ))))
        }
    };
    Ok(())
}

fn parse_job_encrypt(value: &crate::json::Json, allow_weak_crypto: bool) -> Result<EncryptParams> {
    let members = job_json_members(value);
    let user_password = job_json_string(&members, b"userPassword")?;
    let owner_password = job_json_string(&members, b"ownerPassword")?;
    let (Some(user_password), Some(owner_password)) = (user_password, owner_password) else {
        return Err(Error::Usage(UsageError::new(
            "the user and owner password are both required; use the empty string for the user password if you don't want a password",
        )));
    };

    let key_lengths = ["40bit", "128bit", "256bit"]
        .into_iter()
        .filter(|key| members.contains_key(key.as_bytes()))
        .collect::<Vec<_>>();
    if key_lengths.len() > 1 {
        return Err(Error::Usage(UsageError::new(
            "exactly one of 40bit, 128bit, or 256bit must be given",
        )));
    }
    let Some(key_length) = key_lengths.first().copied() else {
        return Err(Error::Usage(UsageError::new(
            "exactly one of 40bit, 128bit, or 256bit must be given; an empty dictionary may be supplied for one of them to set the key length without imposing any restrictions",
        )));
    };
    let settings = members
        .get(key_length.as_bytes())
        .expect("key length was found in the encryption dictionary");
    let settings = job_json_members(settings);
    let mut permissions = crate::PermissionsConfig::default();
    if let Some(value) = job_json_yn(&settings, b"accessibility")? {
        permissions.accessibility = value;
    }
    if let Some(value) = job_json_yn(&settings, b"annotate")? {
        permissions.annotate = value;
    }
    if let Some(value) = job_json_yn(&settings, b"assemble")? {
        permissions.assemble = value;
    }
    if let Some(value) = job_json_yn(&settings, b"extract")? {
        permissions.extract = value;
    }
    if let Some(value) = job_json_yn(&settings, b"form")? {
        permissions.fill_forms = value;
    }
    if let Some(value) = job_json_choice(
        &settings,
        b"modify",
        &["all", "annotate", "form", "assembly", "none"],
        true,
        // cov:ignore-start: llvm-cov attributes this successful choice continuation to the match body
    )? {
        // cov:ignore-end
        job_json_modify_permission(&value, &mut permissions)?;
    }
    if let Some(value) = job_json_yn(&settings, b"modifyOther")? {
        permissions.modify_contents = value;
    }
    if let Some(value) = job_json_choice(&settings, b"print", &["full", "low", "none"], true)? {
        job_json_print_permission(&value, &mut permissions)?;
    }

    let mut params = match key_length {
        "40bit" => EncryptParams::rc4(EncryptMethod::V1Rc440, user_password, owner_password),
        "128bit" => {
            let use_aes = job_json_choice(&settings, b"useAes", &["y", "n"], true)?;
            if use_aes.as_deref() == Some("y") {
                EncryptParams::v4_aes128(user_password, owner_password)
            } else if job_json_bare(&settings, b"forceV4")? {
                EncryptParams::rc4(EncryptMethod::V4Rc4128, user_password, owner_password)
            } else {
                EncryptParams::rc4(EncryptMethod::V2Rc4128, user_password, owner_password)
            }
        }
        "256bit" => {
            if job_json_bare(&settings, b"forceR5")? {
                EncryptParams::v5_r5(user_password, owner_password)
            } else {
                EncryptParams::v5_r6(user_password, owner_password)
            }
        }
        _ => unreachable!("key length was validated above"), // cov:ignore: key length comes only from the validated qpdf job schema choices
    };
    params.permissions = permissions;
    if matches!(
        params.method,
        EncryptMethod::V4Aes128
            | EncryptMethod::V4Rc4128
            | EncryptMethod::V5R5Aes256
            | EncryptMethod::V5R6Aes256
    ) {
        params.permissions.accessibility = true;
    }
    if job_json_bare(&settings, b"cleartextMetadata")? {
        params.encrypt_metadata = false;
    }
    if (params.is_weak_rc4() || params.is_deprecated_r5()) && !allow_weak_crypto {
        return Err(Error::Usage(UsageError::new(
            "refusing to write a file with weak or deprecated encryption without allowWeakCrypto",
        )));
    }
    Ok(params)
}

fn parse_json_decode_level(value: &str) -> crate::writer::DecodeLevel {
    match value {
        "none" => crate::writer::DecodeLevel::None,
        "generalized" => crate::writer::DecodeLevel::Generalized,
        "specialized" => crate::writer::DecodeLevel::Specialized,
        "all" => crate::writer::DecodeLevel::All,
        _ => unreachable!("decode level was validated before conversion"), // cov:ignore: decode level comes only from the validated qpdf job schema choices
    }
}

fn json_decode_level_for_output(value: crate::writer::DecodeLevel) -> JsonDecodeLevel {
    match value {
        crate::writer::DecodeLevel::None => JsonDecodeLevel::None,
        crate::writer::DecodeLevel::Generalized => JsonDecodeLevel::Generalized,
        crate::writer::DecodeLevel::Specialized => JsonDecodeLevel::Specialized,
        crate::writer::DecodeLevel::All => JsonDecodeLevel::All,
    }
}

fn parse_json_version(value: &str) -> i32 {
    match value {
        "1" => 1,
        "2" | "latest" | "" => 2,
        _ => unreachable!("JSON version was validated before conversion"), // cov:ignore: JSON version comes only from the validated qpdf job schema choices
    }
}

fn parse_job_version(value: &str, path: &str) -> Result<(String, i64)> {
    crate::parse_pdf_version_spec(value)
        .ok_or_else(|| Error::Usage(UsageError::new(format!("{path}: invalid version {value}"))))
}

fn parse_positive_usize(value: &[u8], path: &str) -> Result<usize> {
    let value = String::from_utf8_lossy(value);
    let parsed = value.parse::<usize>().map_err(|_| {
        Error::Usage(UsageError::new(format!(
            "{path}: invalid positive integer {value}"
        )))
    })?;
    if parsed == 0 {
        return Err(Error::Usage(UsageError::new(format!(
            "{path}: value must be greater than zero"
        ))));
    }
    Ok(parsed)
}

fn parse_job_split_pages(value: &[u8]) -> Result<usize> {
    // qpdf's Config::splitPages treats an empty parameter as one page
    // (`libqpdf/QPDFJob_config.cc:597-609`); preserve that generated-handler
    // default instead of treating an empty JSON string as an absent option.
    if value.is_empty() {
        return Ok(1);
    }
    let value = String::from_utf8_lossy(value);
    value.parse::<usize>().map_err(|_| {
        Error::Usage(UsageError::new(format!(
            ".splitPages: invalid page count {value}"
        )))
    })
}

fn parse_job_attachment(value: &crate::json::Json, path: &str) -> Result<AttachmentAddOptions> {
    let members = job_json_members(value);
    let file = job_json_required_string(&members, b"file", &format!("{path}.file"))?;
    let path = PathBuf::from(String::from_utf8_lossy(&file).into_owned());
    let basename = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::Usage(UsageError::new(
                "file for --add-attachment may not be empty",
            ))
        })?;
    let filename =
        job_json_string(&members, b"filename")?.unwrap_or_else(|| basename.as_bytes().to_vec());
    let key = job_json_string(&members, b"key")?.unwrap_or_else(|| basename.as_bytes().to_vec());
    let creation_date = job_json_string(&members, b"creationdate")?;
    let modification_date = job_json_string(&members, b"moddate")?;
    Ok(AttachmentAddOptions {
        path,
        key,
        filename,
        mimetype: job_json_string(&members, b"mimetype")?,
        description: job_json_string(&members, b"description")?,
        creation_date,
        modification_date,
        replace: job_json_bare(&members, b"replace")?,
        verbose: false,
    })
}

fn parse_job_overlay_specs(
    destination: &mut Vec<JobOverlayConfig>,
    value: &crate::json::Json,
    kind: OverlayKind,
) -> Result<()> {
    for (index, item) in job_json_items(value).into_iter().enumerate() {
        let members = job_json_members(&item);
        let file = job_json_string(&members, b"file")?.ok_or_else(|| {
            Error::Usage(UsageError::new(
                "file is required in underlay/overlay specification",
            ))
        })?;
        let from = job_json_range(
            members.get(b"from".as_slice()),
            &format!(
                ".{}[{index}].from",
                match kind {
                    OverlayKind::Overlay => "overlay",
                    OverlayKind::Underlay => "underlay",
                }
            ),
        )?; // cov:ignore: llvm-cov attributes this successful range conversion to the opening call lines
        let to = job_json_range(
            members.get(b"to".as_slice()),
            &format!(
                ".{}[{index}].to",
                match kind {
                    OverlayKind::Overlay => "overlay",
                    OverlayKind::Underlay => "underlay",
                }
            ),
        )?; // cov:ignore: llvm-cov attributes this successful range conversion to the opening call lines
        let repeat = members
            .get(b"repeat".as_slice())
            .map(|value| job_json_range(Some(value), "underlay/overlay repeat"))
            .transpose()?;
        destination.push(JobOverlayConfig {
            path: PathBuf::from(String::from_utf8_lossy(&file).into_owned()),
            password: job_json_string(&members, b"password")?.unwrap_or_default(),
            from,
            to,
            repeat,
            kind,
        });
    }
    Ok(())
}

fn parse_job_page_labels(
    specs: &[String],
    page_count: usize,
) -> Result<Vec<(i64, crate::page_label_document_helper::LabelRange)>> {
    use crate::page_label_document_helper::{LabelRange, LabelStyle};

    let page_count = i64::try_from(page_count)
        .map_err(|_| Error::Unsupported("page count exceeds qpdf's range".to_owned()))?;
    let mut entries = Vec::with_capacity(specs.len());
    let mut last_page = 0i64;
    for spec in specs {
        let (first_page, label_spec) = spec.split_once(':').ok_or_else(|| {
            Error::Usage(UsageError::new(
                "page label spec must be n:[D|a|A|r|R][/start[/prefix]]",
            ))
        })?;
        let first_page = if first_page == "z" {
            page_count
        } else if let Some(value) = first_page.strip_prefix('r') {
            let value = value.parse::<i64>().map_err(|_| {
                Error::Usage(UsageError::new(
                    "page label spec must be n:[D|a|A|r|R][/start[/prefix]]",
                ))
            })?;
            page_count + 1 - value
        } else {
            first_page.parse::<i64>().map_err(|_| {
                Error::Usage(UsageError::new(
                    "page label spec must be n:[D|a|A|r|R][/start[/prefix]]",
                ))
            })?
        };
        if entries.is_empty() {
            if first_page != 1 {
                return Err(Error::Usage(UsageError::new(
                    "the first page label specification must start with page 1",
                )));
            }
        } else if first_page <= last_page {
            return Err(Error::Usage(UsageError::new(
                "page label specifications must be in order by first page",
            )));
        }
        if first_page < 1 || first_page > page_count {
            return Err(Error::Usage(UsageError::new(format!(
                "page label spec: page {first_page} is more than the total number of pages ({page_count})"
            ))));
        }

        let mut parts = label_spec.splitn(3, '/');
        let style = match parts.next().unwrap_or_default() {
            "" => LabelStyle::None,
            "D" => LabelStyle::Decimal,
            "a" => LabelStyle::AlphaLower,
            "A" => LabelStyle::AlphaUpper,
            "r" => LabelStyle::RomanLower,
            "R" => LabelStyle::RomanUpper,
            _ => {
                return Err(Error::Usage(UsageError::new(
                    "page label spec must be n:[D|a|A|r|R][/start[/prefix]]",
                )))
            }
        };
        let start = match parts.next() {
            None | Some("") => 1,
            Some(value) => value
                .parse::<i64>()
                .map_err(|_| Error::Usage(UsageError::new("starting page number must be >= 1")))?,
        };
        if start < 1 {
            return Err(Error::Usage(UsageError::new(
                "starting page number must be >= 1",
            )));
        }
        let prefix = parts.next().unwrap_or_default().to_owned();
        entries.push((
            first_page - 1,
            LabelRange {
                style,
                prefix,
                start,
            },
        ));
        last_page = first_page;
    }
    Ok(entries)
}

/// qpdf-compatible status returned by a completed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JobExitCode {
    /// No warning was recorded, or warnings were explicitly configured to be
    /// exit-zero.
    Success = 0,
    /// The job could not create or write its requested document.
    Error = 2,
    /// Warnings were recorded and the job was not configured to suppress the
    /// warning exit status.
    Warning = 3,
}

impl JobExitCode {
    /// Return the process status value used by qpdf.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Shared qpdf-shaped state for the production job lifecycle.
///
/// qpdf's `QPDFJob` keeps this state across setup, document creation, and
/// output/inspection. The operation-specific stages are intentionally not
/// duplicated here; they consume this one state object so warning summaries
/// and progress callbacks cannot diverge between CLI and library consumers.
pub struct QPDFJob {
    logger: QPDFLogger,
    input_name: String,
    message_prefix: String,
    warnings: bool,
    suppress_warnings: bool,
    warnings_exit_zero: bool,
    progress_handler: Option<SharedProgressHandler>,
    configuration: JobConfiguration,
}

impl Default for QPDFJob {
    fn default() -> Self {
        Self::new()
    }
}

impl QPDFJob {
    /// Construct a job with qpdf's default message prefix and logger.
    ///
    /// Corresponds to `QPDFJob::QPDFJob` (`libqpdf/QPDFJob.cc:290-293`), whose
    /// `Members` default-constructs the shared logger
    /// (`libqpdf/QPDFJob.cc:286-289`); the remaining field defaults are the
    /// `Members` in-class initializers in qpdf 11.9.0
    /// (`include/qpdf/QPDFJob.hh:588-601`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            logger: QPDFLogger::default_logger(),
            input_name: String::new(),
            message_prefix: "qpdf".to_owned(),
            warnings: false,
            suppress_warnings: false,
            warnings_exit_zero: false,
            progress_handler: None,
            configuration: JobConfiguration::default(),
        }
    }

    /// Return the logger shared by this job and documents it creates.
    #[must_use]
    pub fn logger(&self) -> QPDFLogger {
        self.logger.clone()
    }

    /// Replace the logger used for subsequent job and document output.
    pub fn set_logger(&mut self, logger: QPDFLogger) {
        self.logger = logger;
    }

    /// Set the prefix used for job-generated diagnostics.
    ///
    /// Mirrors `QPDFJob::setMessagePrefix` (`QPDFJob.cc:303-307`).
    pub fn set_message_prefix(&mut self, message_prefix: impl Into<String>) {
        self.message_prefix = message_prefix.into();
    }

    /// Set the qpdf input name used by inspection diagnostics.
    ///
    /// This corresponds to the `QPDFJob` input filename retained by
    /// `QPDFJob::doListAttachments` for its no-embedded-files branch
    /// (`libqpdf/QPDFJob.cc:909`). `open` and `create_from_json` set it
    /// automatically; `update_from_json` keeps the primary input name while
    /// using its own source name only for the update source. Callers that open
    /// a document outside this job may set it explicitly before an inspection.
    pub fn set_input_name(&mut self, input_name: impl Into<String>) {
        self.input_name = input_name.into();
    }

    /// Return the input name retained by this job.
    #[must_use]
    pub fn input_name(&self) -> &str {
        &self.input_name
    }

    /// Supply an input filename from the surrounding argv boundary before or
    /// after a partial job-JSON file is applied.
    pub fn set_input_file(&mut self, input_file: impl Into<PathBuf>) -> Result<()> {
        if self.configuration.input_file.is_some() || self.configuration.empty_input {
            return Err(Error::Usage(UsageError::new(
                "input file has already been given",
            )));
        }
        let input_file = input_file.into();
        self.input_name = input_file.display().to_string();
        self.configuration.input_file = Some(input_file);
        Ok(())
    }

    /// Supply an output filename from the surrounding argv boundary.
    pub fn set_output_file(&mut self, output_file: impl Into<PathBuf>) -> Result<()> {
        if self.configuration.output_file.is_some() || self.configuration.replace_input {
            return Err(Error::Usage(UsageError::new(
                "output file has already been given",
            )));
        }
        self.configuration.output_file = Some(output_file.into());
        Ok(())
    }

    /// Override the primary input password at the argv configuration boundary.
    pub fn set_password(&mut self, password: impl Into<Vec<u8>>) {
        self.configuration.password = password.into();
    }

    /// Request qpdf writer progress reporting for writers configured by this job.
    ///
    /// Corresponds to `QPDFJob::Config::progress` (`libqpdf/QPDFJob_config.cc:478-481`).
    /// The existing [`Self::configure_writer_progress`] method remains the sole
    /// owner of the default logger-backed reporter construction.
    pub fn set_progress(&mut self, value: bool) {
        self.configuration.progress = value;
    }

    /// Return the current diagnostic prefix.
    #[must_use]
    pub fn message_prefix(&self) -> &str {
        &self.message_prefix
    }

    /// Register qpdf's progress callback for writers configured by this job.
    ///
    /// The callback is shared rather than moved into one writer so the same
    /// job can configure multiple output stages while retaining one callback
    /// registration. The writer owns the qpdf event accounting and invokes
    /// this callback only after its internal borrow is released. A callback
    /// error aborts the active writer, matching qpdf's exception propagation
    /// from QPDFWriter::indicateProgress.
    pub fn register_progress_reporter<F>(&mut self, reporter: F)
    where
        F: FnMut(u8) -> Result<()> + 'static,
    {
        self.progress_handler = Some(Rc::new(RefCell::new(Box::new(reporter))));
    }

    /// Attach the registered progress reporter to one qpdf-shaped writer.
    pub fn configure_writer_progress<R>(&self, writer: &mut PdfWriter<'_, R>)
    where
        R: Read + Seek + 'static,
    {
        // QPDFJob::setWriterOptions uses the custom handler when one was
        // registered and otherwise constructs this default reporter from the
        // job logger (`libqpdf/QPDFJob.cc:2926-2935`).
        let reporter = match self.progress_handler.as_ref() {
            Some(reporter) => Rc::clone(reporter),
            None if self.configuration.progress => {
                let logger = self.logger.clone();
                let prefix = self.message_prefix.clone();
                let output_name = self
                    .configuration
                    .output_file
                    .as_deref()
                    .filter(|path| *path != Path::new("-"))
                    .map_or_else(
                        || "standard output".to_owned(),
                        |path| path.display().to_string(),
                    );
                let callback: ProgressHandler = Box::new(move |percent| {
                    logger.info(format!(
                        "{prefix}: {output_name}: write progress: {percent}%\n"
                    ))
                });
                Rc::new(RefCell::new(callback))
            }
            None => return,
        };
        writer
            .register_progress_reporter(Box::new(move |percent| (reporter.borrow_mut())(percent)));
    }

    /// Initialize the portable qpdf-job argument surface used by qtest.
    ///
    /// This mirrors `QPDFJob::initializeFromArgv` for the arguments owned by
    /// `qpdfjob-ctest.c`: one input, one output, deterministic/static IDs,
    /// object-stream mode, password, decrypt, and check. The full CLI parser
    /// remains outside this production library boundary.
    pub fn initialize_from_argv(&mut self, argv: &[String]) -> Result<()> {
        let mut configuration = JobConfiguration::default();
        let mut positionals = Vec::new();
        let mut parse_options = true;

        for argument in argv.iter().skip(1) {
            if parse_options && argument == "--" {
                parse_options = false;
                continue;
            }
            if parse_options && argument.starts_with("--") {
                match argument.as_str() {
                    "--deterministic-id" => configuration.writer.set_deterministic_id(true),
                    "--static-id" => configuration.writer.set_static_id(true),
                    "--decrypt" => {
                        configuration.writer.set_preserve_encryption(false);
                    }
                    "--progress" => configuration.progress = true,
                    "--check" => configuration.check = true,
                    _ if argument.starts_with("--password=") => {
                        configuration.password = argument.as_bytes()[11..].to_vec();
                    }
                    _ if argument.starts_with("--object-streams=") => {
                        configuration
                            .writer
                            .set_object_stream_mode(parse_object_stream_mode(&argument[17..])?);
                    }
                    _ => {
                        return Err(
                            UsageError::new(format!("unrecognized argument {argument}")).into()
                        );
                    }
                }
            } else if parse_options && argument.starts_with('-') {
                return Err(UsageError::new(format!("unrecognized argument {argument}")).into());
            } else {
                positionals.push(argument.clone());
            }
        }

        if positionals.len() > 2 {
            return Err(UsageError::new(format!("unknown argument {}", positionals[2])).into());
        }
        configuration.input_file = positionals.first().map(PathBuf::from);
        configuration.output_file = positionals.get(1).map(PathBuf::from);
        if configuration.input_file.is_none() && !configuration.check {
            return Err(UsageError::new("an input file name is required").into());
        }

        self.configuration = configuration;
        self.input_name = self
            .configuration
            .input_file
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        self.warnings = false;
        Ok(())
    }

    /// Initialize the portable qpdf-job JSON surface used by qtest.
    ///
    /// This implements the qpdf job-JSON fields currently owned by this
    /// lifecycle, including input/output setup, writer settings, page
    /// transformations (`splitPages`, `rotate`, and `removeRestrictions`),
    /// attachments, page selection, and JSON output.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] if `json` does not parse as a
    /// dictionary, if `inputFile` is missing or empty, if `outputFile` is
    /// missing, or if the dictionary contains a top-level key outside the
    /// subset above (qpdf's full job JSON schema key, but not yet
    /// implemented by this crate).
    pub fn initialize_from_json(&mut self, json: &str) -> Result<()> {
        self.initialize_from_json_with_partial(json, false)
    }

    /// Initialize a job JSON document for the CLI's partial job-json-file
    /// route. Configuration checks that require command-line input/output
    /// values are deferred to [`QPDFJob::run`], matching qpdf's
    /// `QPDFJob::Config::jobJsonFile` call to `initializeFromJson(..., true)`
    /// (`libqpdf/QPDFJob_config.cc:774-784`).
    pub fn initialize_from_json_partial(&mut self, json: &str) -> Result<()> {
        self.initialize_from_json_with_partial(json, true)
    }

    fn initialize_from_json_with_partial(&mut self, json: &str, partial: bool) -> Result<()> {
        // The qpdf C API sets this prefix before parsing JSON
        // (`libqpdf/qpdfjob-c.cc:79-87`), so initialization and run-time
        // configuration errors share the same observable source name.
        self.set_message_prefix("qpdfjob json");
        let value = crate::json::Json::parse(json.as_bytes())
            .map_err(|error| Error::System(error.to_string()))?;
        if !value.is_dictionary() {
            return Err(Error::Usage(UsageError::new(
                "top-level object is supposed to be a dictionary",
            )));
        }
        // qpdf validates the full schema before dispatching any generated
        // handler (`QPDFJob_json.cc:611-625`). This must happen before any
        // configuration mutation so a schema failure cannot leave a partially
        // initialized job behind.
        validate_job_json_schema(&value)?;
        let members = job_json_members(&value);
        let mut configuration = JobConfiguration {
            require_output: true,
            json_decode_level: crate::writer::DecodeLevel::Generalized,
            ..JobConfiguration::default()
        };

        if let Some(input) = job_json_string(&members, b"inputFile")? {
            if input.is_empty() {
                configuration.empty_input = true;
            } else {
                configuration.input_file =
                    Some(PathBuf::from(String::from_utf8_lossy(&input).into_owned()));
            }
        }
        if job_json_bare(&members, b"empty")? {
            if configuration.input_file.is_some() || configuration.empty_input {
                return Err(Error::Usage(UsageError::new(
                    "empty input can't be used since input file has already been given",
                )));
            }
            configuration.empty_input = true;
        }
        if let Some(output) = job_json_string(&members, b"outputFile")? {
            configuration.output_file =
                Some(PathBuf::from(String::from_utf8_lossy(&output).into_owned()));
        }
        configuration.replace_input = job_json_bare(&members, b"replaceInput")?;
        configuration.password = job_json_string(&members, b"password")?.unwrap_or_default();
        if let Some(password_file) = job_json_string(&members, b"passwordFile")? {
            let path = PathBuf::from(String::from_utf8_lossy(&password_file).into_owned());
            // Byte-preserving, first-line-only contract, matching qpdf's
            // `QUtil::read_lines_from_file` + `lines.front()`
            // (`QUtil.cc:1231-1286`, `QPDFJob_config.cc:661-679`): split on
            // raw `\n` bytes, stripping a preceding `\r`, and use the first
            // line's bytes as the password verbatim. A password need not be
            // valid UTF-8, so this reads raw bytes rather than
            // `read_to_string` + `.lines().next()`, which both rejects
            // non-UTF-8 password bytes and (for a file with no trailing
            // newline at all) can differ on whether a lone final line counts.
            let bytes = std::fs::read(&path)
                .map_err(|error| Error::file_io("read password file", path.clone(), error))?;
            let first_line_len = bytes
                .iter()
                .position(|&byte| byte == b'\n')
                .unwrap_or(bytes.len());
            let mut first_line = bytes[..first_line_len].to_vec();
            if first_line.ends_with(b"\r") {
                first_line.pop();
            }
            configuration.password = first_line;
        }
        configuration.json_input = job_json_bare(&members, b"jsonInput")?;

        if job_json_bare(&members, b"qdf")? {
            configuration.writer.set_qdf_mode(true);
        }
        if job_json_bare(&members, b"preserveUnreferenced")? {
            configuration.writer.set_preserve_unreferenced_objects(true);
        }
        if job_json_bare(&members, b"newlineBeforeEndstream")? {
            configuration.writer.set_newline_before_endstream(true);
        }
        if let Some(value) = job_json_choice(&members, b"normalizeContent", &["y", "n"], true)? {
            configuration.writer.set_content_normalization(value == "y");
        }
        if let Some(value) = job_json_choice(
            &members,
            b"streamData",
            &["compress", "preserve", "uncompress"],
            true,
            // cov:ignore-start: llvm-cov attributes this successful choice continuation to the match body
        )? {
            // cov:ignore-end
            configuration
                .writer
                .set_stream_data_mode(match value.as_str() {
                    "compress" => crate::StreamDataMode::Compress,
                    "preserve" => crate::StreamDataMode::Preserve,
                    "uncompress" => crate::StreamDataMode::Uncompress,
                    _ => unreachable!("streamData was validated above"), // cov:ignore: streamData comes only from the validated qpdf job schema choices
                });
        }
        if let Some(value) = job_json_choice(&members, b"compressStreams", &["y", "n"], true)? {
            configuration.writer.set_compress_streams(value == "y");
        }
        if job_json_bare(&members, b"recompressFlate")? {
            configuration.writer.set_recompress_flate(true);
        }
        if let Some(value) = job_json_choice(
            &members,
            b"decodeLevel",
            &["none", "generalized", "specialized", "all"],
            true,
        )? {
            let level = parse_json_decode_level(&value);
            configuration.writer.set_decode_level(level);
            configuration.json_decode_level = level;
            configuration.json_decode_level_set = true;
        }
        if job_json_bare(&members, b"decrypt")? {
            configuration.writer.set_preserve_encryption(false);
        }
        if job_json_bare(&members, b"deterministicId")? {
            configuration.writer.set_deterministic_id(true);
        }
        if job_json_bare(&members, b"staticAesIv")? {
            configuration.writer.set_static_aes_iv(true);
        }
        if job_json_bare(&members, b"staticId")? {
            configuration.writer.set_static_id(true);
        }
        if job_json_bare(&members, b"noOriginalObjectIds")? {
            configuration.writer.set_suppress_original_object_ids(true);
        }
        configuration.allow_weak_crypto = job_json_bare(&members, b"allowWeakCrypto")?;
        configuration.progress = job_json_bare(&members, b"progress")?;
        configuration.verbose = job_json_bare(&members, b"verbose")?;
        if let Some(value) = job_json_string(&members, b"splitPages")? {
            configuration.split_pages = Some(parse_job_split_pages(&value)?);
        }
        if let Some(value) = job_json_string(&members, b"rotate")? {
            let value = String::from_utf8_lossy(&value);
            let rotation = RotateSpec::parse(&value)
                .map_err(|error| Error::Usage(UsageError::new(format!(".rotate: {error}"))))?;
            configuration.rotations.push(rotation);
        }
        configuration.remove_restrictions = job_json_bare(&members, b"removeRestrictions")?;
        if let Some(value) = job_json_choice(
            &members,
            b"objectStreams",
            &["disable", "preserve", "generate"],
            true,
        )? {
            configuration
                .writer
                .set_object_stream_mode(parse_object_stream_mode(&value)?);
        }
        if let Some(value) = job_json_string(&members, b"minVersion")? {
            let value = String::from_utf8_lossy(&value);
            let (version, extension) = parse_job_version(&value, ".minVersion")?;
            configuration
                .writer
                .set_minimum_pdf_version(version, extension);
        }
        if let Some(value) = job_json_string(&members, b"forceVersion")? {
            let value = String::from_utf8_lossy(&value);
            let (version, extension) = parse_job_version(&value, ".forceVersion")?;
            configuration.writer.force_pdf_version(version, extension);
        }
        if let Some(value) = job_json_string(&members, b"linearizePass1")? {
            configuration.linearize_pass1 =
                Some(PathBuf::from(String::from_utf8_lossy(&value).into_owned()));
        }
        configuration.linearize = job_json_bare(&members, b"linearize")?;
        if let Some(value) = job_json_string(&members, b"updateFromJson")? {
            configuration.update_from_json =
                Some(PathBuf::from(String::from_utf8_lossy(&value).into_owned()));
        }
        if let Some(value) = job_json_string(&members, b"collate")? {
            configuration.collate = Some(parse_positive_usize(&value, ".collate")?);
        }

        if let Some(value) = job_json_choice(&members, b"json", &["1", "2", "latest"], false)? {
            configuration.json_version = Some(parse_json_version(&value));
            configuration.require_output = false;
        }
        if let Some(value) = job_json_choice(&members, b"jsonOutput", &["2", "latest"], false)? {
            configuration.json_output = true;
            configuration.json_version = Some(parse_json_version(&value));
            configuration.json_stream_data = JsonStreamData::Inline;
            if !configuration.json_decode_level_set {
                configuration.json_decode_level = crate::writer::DecodeLevel::None;
            }
            configuration.require_output = false;
            configuration.json_keys.push(JsonKey::Qpdf);
        }
        if let Some(value) = job_json_string(&members, b"jsonStreamPrefix")? {
            configuration.json_stream_prefix = Some(String::from_utf8_lossy(&value).into_owned());
        }
        if let Some(value) = job_json_choice(
            &members,
            b"jsonStreamData",
            &["none", "inline", "file"],
            true,
            // cov:ignore-start: llvm-cov attributes this successful choice continuation to the match body
        )? {
            // cov:ignore-end
            configuration.json_stream_data = match value.as_str() {
                "none" => JsonStreamData::None,
                "inline" => JsonStreamData::Inline,
                "file" => JsonStreamData::File,
                _ => unreachable!("jsonStreamData was validated above"), // cov:ignore: jsonStreamData comes only from the validated qpdf job schema choices
            };
        }
        if let Some(value) = members.get(b"jsonKey".as_slice()) {
            for item in job_json_items(value) {
                let item = item.get_string().ok_or_else(|| {
                    Error::Usage(UsageError::new(".jsonKey: value must be a string"))
                })?;
                let item = String::from_utf8_lossy(&item);
                let key = JsonKey::from_str(&item).ok_or_else(|| {
                    Error::Usage(UsageError::new(
                        ".jsonKey: unexpected value; expected one of acroform, attachments, encrypt, objectinfo, objects, outlines, pagelabels, pages, qpdf".to_owned(),
                    ))
                })?;
                configuration.json_keys.push(key);
            }
        }
        if let Some(value) = members.get(b"jsonObject".as_slice()) {
            for item in job_json_items(value) {
                let item = item.get_string().ok_or_else(|| {
                    Error::Usage(UsageError::new(".jsonObject: value must be a string"))
                })?;
                let item = String::from_utf8_lossy(&item);
                let selector = JsonObjectSelector::from_str(&item).ok_or_else(|| {
                    Error::Usage(UsageError::new(format!(
                        ".jsonObject: invalid object selector {item}"
                    )))
                })?;
                configuration.json_objects.push(selector);
            }
        }
        configuration.test_json_schema = job_json_bare(&members, b"testJsonSchema")?;
        configuration.show_encryption_key = job_json_bare(&members, b"showEncryptionKey")?;
        if job_json_bare(&members, b"noWarn")? {
            self.suppress_warnings = true;
        }
        if job_json_bare(&members, b"warningExit0")? {
            self.warnings_exit_zero = true;
        }
        if job_json_bare(&members, b"check")? {
            configuration.check = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"showEncryption")? {
            configuration.show_encryption = true;
            configuration.require_output = false;
        }

        if let Some(value) = members.get(b"encrypt".as_slice()) {
            configuration
                .writer
                .set_encryption_parameters(parse_job_encrypt(
                    value,
                    configuration.allow_weak_crypto,
                )?); // cov:ignore: llvm-cov attributes this successful encryption parse continuation to its opening expressions
        }

        if let Some(value) = members.get(b"pages".as_slice()) {
            for (index, item) in job_json_items(value).into_iter().enumerate() {
                let item_members = job_json_members(&item);
                let file = job_json_string(&item_members, b"file")?.ok_or_else(|| {
                    Error::Usage(UsageError::new("file is required in page specification"))
                })?;
                let range = job_json_range(
                    item_members.get(b"range".as_slice()),
                    &format!(".pages[{index}].range"),
                )?; // cov:ignore: llvm-cov attributes this successful page range conversion to the opening call lines
                configuration.page_specs.push(JobPageConfig {
                    path: PathBuf::from(String::from_utf8_lossy(&file).into_owned()),
                    password: job_json_string(&item_members, b"password")?.unwrap_or_default(),
                    range,
                }); // cov:ignore: llvm-cov attributes this successful page configuration to its field expressions
            }
        }
        if let Some(value) = members.get(b"overlay".as_slice()) {
            parse_job_overlay_specs(&mut configuration.overlays, value, OverlayKind::Overlay)?;
        }
        if let Some(value) = members.get(b"underlay".as_slice()) {
            parse_job_overlay_specs(&mut configuration.underlays, value, OverlayKind::Underlay)?;
        }
        if let Some(value) = members.get(b"addAttachment".as_slice()) {
            for (index, item) in job_json_items(value).into_iter().enumerate() {
                configuration.attachments_to_add.push(parse_job_attachment(
                    &item,
                    &format!(".addAttachment[{index}]"),
                )?);
            }
        }
        if let Some(value) = members.get(b"copyAttachmentsFrom".as_slice()) {
            for (index, item) in job_json_items(value).into_iter().enumerate() {
                let item_members = job_json_members(&item);
                let file = job_json_required_string(
                    &item_members,
                    b"file",
                    &format!(".copyAttachmentsFrom[{index}].file"),
                )?; // cov:ignore: llvm-cov attributes this successful page range conversion to the opening call lines
                configuration
                    .attachments_to_copy
                    .push(JobCopyAttachmentsConfig {
                        path: PathBuf::from(String::from_utf8_lossy(&file).into_owned()),
                        password: job_json_string(&item_members, b"password")?.unwrap_or_default(),
                        prefix: job_json_string(&item_members, b"prefix")?.unwrap_or_default(),
                    }); // cov:ignore: llvm-cov attributes this successful attachment configuration to its field expressions
            }
        }
        if let Some(value) = members.get(b"removeAttachment".as_slice()) {
            for item in job_json_items(value) {
                configuration
                    .attachments_to_remove
                    .push(item.get_string().ok_or_else(|| {
                        Error::Usage(UsageError::new(".removeAttachment: value must be a string"))
                    })?);
            }
        }
        if let Some(value) = members.get(b"setPageLabels".as_slice()) {
            let mut labels = Vec::new();
            for item in job_json_items(value) {
                let label = item.get_string().ok_or_else(|| {
                    Error::Usage(UsageError::new(".setPageLabels: value must be a string"))
                })?;
                labels.push(String::from_utf8_lossy(&label).into_owned());
            }
            configuration.set_page_labels = Some(labels);
        }
        if job_json_bare(&members, b"removePageLabels")? {
            configuration.remove_page_labels = true;
        }
        if let Some(value) = job_json_choice(
            &members,
            b"removeUnreferencedResources",
            &["auto", "yes", "no"],
            true,
            // cov:ignore-start: llvm-cov attributes this successful choice continuation to the match body
        )? {
            // cov:ignore-end
            configuration.remove_unreferenced_resources = match value.as_str() {
                "auto" => RemoveUnreferencedResources::Auto,
                "yes" => RemoveUnreferencedResources::Yes,
                "no" => RemoveUnreferencedResources::No,
                _ => unreachable!("removeUnreferencedResources was validated above"), // cov:ignore: removeUnreferencedResources comes only from the validated qpdf job schema choices
            };
        }
        if job_json_bare(&members, b"preserveUnreferencedResources")? {
            configuration.remove_unreferenced_resources = RemoveUnreferencedResources::No;
        }

        self.configuration = configuration;
        self.input_name = self
            .configuration
            .input_file
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        self.warnings = false;
        if !partial {
            self.check_configuration()?;
        }
        Ok(())
    }

    /// Open one job-owned document through the erased qpdf input boundary.
    ///
    /// The concrete reader remains lazy and owned by the document resolver,
    /// but callers no longer need a different `Pdf<R>` type for a file,
    /// generated seed, or another seekable source. This is the Rust shape of
    /// qpdf's single `QPDF` document returned by `createQPDF`
    /// (`QPDFJob.cc:428-535`).
    pub fn open_document<R>(
        &mut self,
        source: R,
        input_name: impl Into<String>,
        mut options: PdfOpenOptions,
    ) -> Result<JobDocument>
    where
        R: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        // qpdf's noWarn (`Config::noWarn`, `QPDFJob_config.cc:407-410`)
        // applies `pdf.setSuppressWarnings(true)` to every QPDF this job
        // opens (`QPDFJob.cc:663-665`), not just the final completion
        // summary that `self.suppress_warnings` alone gates in `complete()`.
        // OR rather than overwrite: a caller that already asked for
        // suppression on this specific open must keep it regardless of the
        // job's own setting.
        options.suppress_warnings |= self.suppress_warnings;
        let mut pdf = Pdf::<Box<dyn ReadSeek>>::open_with_options(Box::new(source), options)?;
        // qpdf's createQPDF resolves the root while establishing the
        // document's version/extension state before operation dispatch
        // (`QPDFJob.cc:429-480,1696-1716`).
        pdf.root_handle()?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Create qpdf's canonical empty document through the same job document
    /// boundary as file and JSON input.
    pub fn create_empty_document(&mut self) -> Result<JobDocument> {
        // qpdf's `setQPDFOptions` (`QPDFJob.cc:651-665`) runs unconditionally
        // right after `QPDF` construction, before dispatching to empty,
        // JSON-input, or file-based creation (`QPDFJob.cc:1701-1710`), so
        // `noWarn` suppresses warnings for an empty document exactly like the
        // other two creation kinds.
        let mut pdf = crate::engine::open_empty_with_options_erased(PdfOpenOptions {
            logger: Some(self.logger.clone()),
            suppress_warnings: self.suppress_warnings,
            ..PdfOpenOptions::default()
        })?;
        self.input_name.clear();
        pdf.root_handle()?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Create a complete JSON-input document through the same job document
    /// boundary as file and empty input.
    pub fn create_from_json_document<S>(
        &mut self,
        source: S,
        input_name: impl Into<String>,
    ) -> Result<JobDocument>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        // See `create_empty_document`: qpdf applies `noWarn` to every
        // creation kind uniformly, including JSON-input.
        let pdf = crate::json::create_from_json_erased(
            source,
            input_name,
            PdfOpenOptions {
                logger: Some(self.logger.clone()),
                suppress_warnings: self.suppress_warnings,
                ..PdfOpenOptions::default()
            },
        )?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Create the configured input document, returning `None` after qpdf-style
    /// error reporting for a missing or malformed input.
    pub fn create_qpdf(&mut self) -> Result<Option<JobDocument>> {
        match self.check_configuration() {
            Ok(()) => {}
            Err(error @ Error::Usage(_)) => return Err(error),
            Err(error) => {
                self.report_job_error(&error)?;
                return Ok(None);
            }
        }
        if self.configuration.empty_input {
            return self.create_empty_document().map(Some);
        }
        let Some(input) = self.configuration.input_file.clone() else {
            let error = Error::Unsupported("qpdfjob input file is not configured".to_owned());
            self.report_job_error(&error)?;
            return Ok(None);
        };
        let file = match File::open(&input) {
            Ok(file) => file,
            Err(error) => {
                let error = Error::file_io("open", input.clone(), error);
                self.report_job_error(&error)?;
                return Ok(None);
            }
        };
        if self.configuration.json_input {
            return match self.create_from_json_document(file, input.display().to_string()) {
                Ok(pdf) => Ok(Some(pdf)),
                Err(error) => {
                    self.report_job_error(&error)?;
                    Ok(None)
                }
            };
        }
        match self.open_document(
            BufReader::new(file),
            input.display().to_string(),
            PdfOpenOptions {
                password: self.configuration.password.clone(),
                ..PdfOpenOptions::default()
            },
        ) {
            Ok(pdf) => Ok(Some(pdf)),
            Err(error) => {
                self.report_job_error(&error)?;
                Ok(None)
            }
        }
    }

    /// Write a created document through the configured qpdf writer and
    /// complete the shared warning/status boundary.
    pub fn write_qpdf<R>(&mut self, pdf: &mut Pdf<R>) -> Result<JobExitCode>
    where
        R: Read + Seek + 'static,
    {
        let Some(output) = self.configuration.output_file.clone().or_else(|| {
            self.configuration
                .replace_input
                .then(|| self.replace_input_path())
                .flatten()
        }) else {
            return Ok(JobExitCode::Error);
        };
        let mut writer_configuration = self.configuration.writer.clone();
        writer_configuration.set_linearization(self.configuration.linearize);
        if let Some(path) = self.configuration.linearize_pass1.as_deref() {
            writer_configuration.set_linearization_pass1_filename(path.to_path_buf());
        }
        let progress_requested = self.configuration.progress;
        let write_result =
            if let Some(chunk_size) = self.configuration.split_pages.filter(|size| *size > 0) {
                let mut split_options = SplitPageOptions::new(chunk_size, output.clone())
                    .with_writer_configuration(writer_configuration.clone());
                if let Some(input) = self.configuration.input_file.clone() {
                    split_options = split_options.with_input_path(input);
                }
                self.split_pages(pdf, split_options).map(|_| ())
            } else {
                (|| {
                    let mut writer = PdfWriter::new(pdf);
                    writer_configuration.apply_to(&mut writer);
                    if progress_requested {
                        self.configure_writer_progress(&mut writer);
                    }
                    if output == Path::new("-") {
                        self.logger.save_to_standard_output(true)?;
                        writer.set_output_pipeline(JobOutputPipeline(self.logger.get_save()?))?;
                    } else {
                        writer.set_output_file(&output)?;
                    }
                    writer.write()
                })()
            };
        match write_result {
            Ok(()) => {
                self.record_document_warnings(pdf);
                if self.configuration.verbose && output != Path::new("-") {
                    self.logger.info(format!(
                        "{}: wrote file {}\n",
                        self.message_prefix,
                        output.display()
                    ))?; // cov:ignore: llvm-cov attributes this successful logger write to its opening expressions
                }
                self.complete(true)
            }
            Err(error) => {
                self.report_job_error(&error)?;
                Ok(JobExitCode::Error)
            }
        }
    }

    /// Run the configured create/write or check lifecycle.
    pub fn run(&mut self) -> Result<JobExitCode> {
        let Some(pdf) = self.create_qpdf()? else {
            return Ok(JobExitCode::Error);
        };

        let configuration = self.configuration.clone();
        let status = match self.run_document_erased(pdf, &configuration) {
            Ok(status) => status,
            Err(error) => {
                self.report_job_error(&error)?;
                JobExitCode::Error
            }
        };
        if configuration.replace_input {
            if status == JobExitCode::Error {
                self.remove_replace_input_temp();
            } else {
                self.finish_replace_input()?;
            }
        }
        Ok(status)
    }

    fn run_document_erased(
        &mut self,
        mut primary: JobDocument,
        configuration: &JobConfiguration,
    ) -> Result<JobExitCode> {
        if let Some(update_path) = configuration.update_from_json.as_deref() {
            let update_file = File::open(update_path).map_err(|error| {
                Error::file_io("open update JSON", update_path.to_path_buf(), error)
            })?;
            self.update_from_json(
                &mut primary,
                BufReader::new(update_file),
                update_path.display().to_string(),
            )?; // cov:ignore: llvm-cov attributes this successful update continuation to its opening call lines
        }

        if configuration.page_specs.is_empty() {
            self.apply_configured_rotations(&mut primary, configuration)?;
            self.run_document_stages(&mut primary, configuration)
        } else {
            let mut page_sources = vec![primary];
            let mut specs = Vec::with_capacity(configuration.page_specs.len());
            for page in &configuration.page_specs {
                let source_index = if page.path == Path::new(".")
                    || self.configuration.input_file.as_deref() == Some(page.path.as_path())
                {
                    0
                } else {
                    let source = self.open_job_source(&page.path, &page.password)?;
                    page_sources.push(source);
                    page_sources.len() - 1
                };
                specs.push(PageSpecInput::new(source_index, page.range.clone()));
            }
            let mut merged = self.handle_page_specs(
                &mut page_sources,
                &specs,
                configuration.collate,
                configuration.remove_unreferenced_resources,
                configuration.writer.preserves_unreferenced_objects(),
            )?; // cov:ignore: llvm-cov attributes this successful page merge continuation to its opening call lines
            self.apply_configured_rotations(&mut merged, configuration)?;
            let status = self.run_document_stages(&mut merged, configuration);
            // `merged` may retain provider-backed objects from page_sources;
            // both are deliberately alive until every output byte is written.
            drop(merged);
            drop(page_sources);
            status
        }
    }

    fn apply_configured_rotations<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<()>
    where
        R: Read + Seek,
    {
        if configuration.rotations.is_empty() {
            return Ok(());
        }
        let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
        let page_count = u32::try_from(page_refs.len())
            .map_err(|_| Error::Unsupported("page count exceeds qpdf's range".to_owned()))?;
        for rotation in &configuration.rotations {
            let selected = rotation.range.resolve(page_count)?;
            let selected_refs = selected
                .into_iter()
                .map(|page| {
                    let index = usize::try_from(page - 1).map_err(|_| {
                        Error::Unsupported("rotation page index underflow".to_owned())
                    })?;
                    page_refs.get(index).copied().ok_or_else(|| {
                        Error::Unsupported("rotation page index out of range".to_owned())
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            apply_rotate_to_pages(pdf, &selected_refs, &rotation.op)?;
        }
        Ok(())
    }

    fn run_document_stages<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<JobExitCode>
    where
        R: Read + Seek + 'static,
    {
        let mut overlay_specs =
            Vec::with_capacity(configuration.overlays.len() + configuration.underlays.len());
        for overlay in configuration
            .underlays
            .iter()
            .chain(configuration.overlays.iter())
        {
            let source = self.open_job_source(&overlay.path, &overlay.password)?;
            overlay_specs.push(OverlaySpec {
                source,
                kind: overlay.kind,
                from: overlay.from.clone(),
                to: overlay.to.clone(),
                repeat: overlay.repeat.clone(),
            });
        }
        apply_overlay_specs(pdf, &mut overlay_specs)?;

        // qpdf's `handleTransformations` applies `removeRestrictions` after
        // underlay/overlay handling and delegates the mutation to
        // `QPDFAcroFormDocumentHelper::disableDigitalSignatures`
        // (`libqpdf/QPDFJob.cc:2137-2150`). Keep the same document-level
        // /Perms, /SigFlags, and signature-field boundary; do not reuse the
        // CLI's separate rewrite policy.
        if configuration.remove_restrictions {
            let mut acroform = AcroFormDocumentHelper::new(pdf)?;
            let _ = acroform.disable_digital_signatures()?;
        }

        self.apply_page_label_transformations(pdf, configuration)?;
        for key in &configuration.attachments_to_remove {
            if !pdf.embedded_files().remove_embedded_file(key)? {
                return Err(Error::System(format!(
                    "attachment {} not found",
                    String::from_utf8_lossy(key)
                )));
            }
            if configuration.verbose {
                self.logger.info(format!(
                    "{}: removed attachment {}\n",
                    self.message_prefix,
                    String::from_utf8_lossy(key)
                ))?; // cov:ignore: llvm-cov attributes this successful logger write to its opening expressions
            } // cov:ignore: llvm-cov attributes this successful attachment branch continuation
        }
        let attachments_to_add = configuration
            .attachments_to_add
            .iter()
            .cloned()
            .map(|mut option| {
                option.verbose = configuration.verbose;
                option
            })
            .collect::<Vec<_>>();
        self.add_attachments(pdf, &attachments_to_add)?;

        let mut attachment_sources = Vec::with_capacity(configuration.attachments_to_copy.len());
        for copy in &configuration.attachments_to_copy {
            let mut source = self.open_job_source(&copy.path, &copy.password)?;
            self.copy_attachments(
                pdf,
                &mut source,
                &AttachmentCopyOptions {
                    path: copy.path.clone(),
                    prefix: copy.prefix.clone(),
                    verbose: configuration.verbose,
                },
            )?; // cov:ignore: llvm-cov attributes this successful attachment copy continuation to its opening call lines
            attachment_sources.push(source);
        }

        if configuration.show_encryption {
            self.show_encryption(pdf, false)?;
            self.record_document_warnings(pdf);
            return self.complete(false);
        }
        if configuration.json_version.is_some() {
            return self.write_configured_json(pdf, configuration);
        }
        if configuration.check
            || (configuration.output_file.is_none() && !configuration.replace_input)
        {
            let check_result = self.check(pdf);
            let status = self.map_check_result(check_result);
            drop(attachment_sources);
            drop(overlay_specs);
            return status;
        }
        let status = self.write_qpdf(pdf);
        drop(attachment_sources);
        drop(overlay_specs);
        status
    }

    fn open_job_source(&mut self, path: &Path, password: &[u8]) -> Result<JobDocument> {
        let file =
            File::open(path).map_err(|error| Error::file_io("open", path.to_path_buf(), error))?;
        let primary_name = self.input_name.clone();
        let result = self.open_document(
            BufReader::new(file),
            path.display().to_string(),
            PdfOpenOptions {
                password: password.to_vec(),
                ..PdfOpenOptions::default()
            },
        );
        self.input_name = primary_name;
        result
    }

    fn write_configured_json<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<JobExitCode>
    where
        R: Read + Seek + 'static,
    {
        let version = configuration
            .json_version
            .expect("json_version is present for configured JSON output");
        let options = JsonJobOptions {
            decode_level: json_decode_level_for_output(configuration.json_decode_level),
            stream_data: configuration.json_stream_data,
            stream_prefix: configuration.json_stream_prefix.as_deref(),
            keys: &configuration.json_keys,
            objects: &configuration.json_objects,
        };
        if let Some(path) = configuration
            .output_file
            .as_deref()
            .filter(|path| *path != Path::new("-"))
        {
            let mut file = File::create(path)
                .map_err(|error| Error::file_io("open JSON output", path.to_path_buf(), error))?;
            return self
                .write_json_with_version(
                    pdf,
                    version,
                    configuration.test_json_schema,
                    configuration.json_output,
                    configuration.show_encryption_key,
                    options,
                    JsonJobOutput::File {
                        filename: path,
                        writer: &mut file,
                    },
                )
                .map_err(|error| Error::System(error.to_string()));
        }

        self.logger.save_to_standard_output(true)?;
        let mut output = JobOutputWriter(self.logger.get_save()?);
        self.write_json_with_version(
            pdf,
            version,
            configuration.test_json_schema,
            configuration.json_output,
            configuration.show_encryption_key,
            options,
            JsonJobOutput::Stdout(&mut output),
        )
        .map_err(|error| Error::System(error.to_string()))
    }

    fn apply_page_label_transformations<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<()>
    where
        R: Read + Seek,
    {
        if configuration.remove_page_labels {
            if let Some(root_ref) = pdf.root_ref() {
                let root = pdf.get_object_handle(root_ref);
                root.remove_key(b"/PageLabels");
                pdf.mark_object_handle_dirty(&root)?;
            } // cov:ignore: llvm-cov attributes this successful page-label removal continuation to its root mutation expressions
        }
        let Some(specs) = configuration.set_page_labels.as_deref() else {
            return Ok(());
        };
        let page_count = crate::page_document_helper::PageDocumentHelper::new(pdf)
            .get_all_pages()?
            .len();
        let entries = parse_job_page_labels(specs, page_count)?;
        pdf.page_labels().write_reconstructed_labels(&entries)
    }

    fn replace_input_path(&self) -> Option<PathBuf> {
        self.configuration
            .input_file
            .as_ref()
            .map(|path| PathBuf::from(format!("{}.~qpdf-temp#", path.display())))
    }

    fn remove_replace_input_temp(&self) {
        if let Some(path) = self.replace_input_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn finish_replace_input(&self) -> Result<()> {
        let input = self.configuration.input_file.as_ref().ok_or_else(|| {
            // cov:ignore-start: successful replace-input completion has the validated input path
            Error::Usage(UsageError::new("--replace-input requires an input file"))
            // cov:ignore-end
        })?; // cov:ignore: successful replace-input completion has the validated input path
        let temp = self
            .replace_input_path()
            .ok_or_else(|| Error::System("replace-input temporary path is missing".to_owned()))?;
        let backup = PathBuf::from(format!(
            "{}.~qpdf-orig{}",
            input.display(),
            if self.warnings { "" } else { "#" }
        ));
        std::fs::rename(input, &backup)
            .map_err(|error| Error::file_io("rename original input", input.clone(), error))?;
        if let Err(error) = std::fs::rename(&temp, input) {
            // cov:ignore-start: the writer-success boundary guarantees its temporary output exists; external deletion is not a deterministic portable test
            let _ = std::fs::rename(&backup, input);
            return Err(Error::file_io("replace input", input.clone(), error));
            // cov:ignore-end
        }
        if self.warnings {
            self.logger.error(format!(
                "{}: there are warnings; original file kept in {}\n",
                self.message_prefix,
                backup.display()
            ))?; // cov:ignore: llvm-cov attributes this successful warning logger write to its opening expressions
        } else if let Err(error) = std::fs::remove_file(&backup) {
            // cov:ignore-start: backup deletion failure depends on external filesystem permissions or races
            self.logger.error(format!(
                "{}: unable to delete original file ({}); original file left in {}, but the input was successfully replaced\n",
                self.message_prefix,
                error,
                backup.display()
            ))?;
            // cov:ignore-end
        }
        Ok(())
    }

    fn map_check_result(
        &self,
        result: std::result::Result<JobExitCode, super::check::CheckError>,
    ) -> Result<JobExitCode> {
        match result {
            Ok(status) => Ok(status),
            Err(super::check::CheckError::ErrorsDetected) => Ok(JobExitCode::Error),
            Err(super::check::CheckError::Operation(error)) => {
                self.report_job_error(&error)?;
                Ok(JobExitCode::Error)
            }
        }
    }

    /// Apply qpdf's pre-open output destination and file-identity checks.
    ///
    /// This is the portable subset of `QPDFJob::checkConfiguration`
    /// (`libqpdf/QPDFJob.cc:567-631`): stdout is reserved before the input is
    /// opened, and `QUtil::same_file` rejects destructive aliases before the
    /// writer can truncate them.
    fn check_configuration(&self) -> Result<()> {
        if self.configuration.input_file.is_none()
            && !self.configuration.empty_input
            && (self.configuration.require_output
                || self.configuration.check
                || self.configuration.show_encryption
                || self.configuration.output_file.is_some()
                || self.configuration.replace_input)
        {
            return Err(UsageError::new("an input file name is required").into());
        }
        if self.configuration.replace_input {
            if self.configuration.output_file.is_some() {
                return Err(UsageError::new(
                    "--replace-input may not be used when an output file is specified",
                )
                .into());
            }
            if self.configuration.empty_input {
                return Err(UsageError::new("--replace-input may not be used with --empty").into());
            }
            if self.configuration.split_pages.is_some_and(|size| size > 0) {
                return Err(
                    UsageError::new("--split-pages may not be used with --replace-input").into(),
                );
            }
            if self.configuration.json_version.is_some() {
                return Err(UsageError::new("--json may not be used with --replace-input").into());
            }
        }
        let json_output_allowed = self.configuration.json_version.is_some();
        if self.configuration.require_output
            && self.configuration.output_file.is_none()
            && !self.configuration.replace_input
        {
            return Err(UsageError::new(
                "an output file name is required; use - for standard output",
            )
            .into());
        }
        if (self.configuration.check || self.configuration.show_encryption)
            && !json_output_allowed
            && (self.configuration.output_file.is_some() || self.configuration.replace_input)
        {
            return Err(UsageError::new("no output file may be given for this option").into());
        }
        if self.configuration.output_file.as_deref() == Some(Path::new("-")) {
            if self.configuration.split_pages.is_some_and(|size| size > 0) {
                return Err(UsageError::new(
                    "--split-pages may not be used when writing to standard output",
                )
                .into());
            }
            self.logger.save_to_standard_output(true)?;
        }
        if let (Some(input), Some(output)) = (
            self.configuration.input_file.as_deref(),
            self.configuration.output_file.as_deref(),
        ) {
            if !self.configuration.replace_input && crate::qutil::same_file(input, output) {
                return Err(UsageError::new(
                    "input file and output file are the same; use --replace-input to intentionally overwrite the input",
                )
                .into());
            }
        }
        // qpdf validates jsonKey/version compatibility unconditionally, not
        // only when JSON output was requested: `m->json_version` defaults to
        // 0, which falls into the "not version 1" branch below
        // (`QPDFJob.cc:630-637`). Confirmed against live qpdf 11.9.0:
        // `--json-key=objects` alone (no `--json`) still errors with this
        // exact message.
        if self.configuration.json_version == Some(1) {
            if self.configuration.json_keys.contains(&JsonKey::Qpdf) {
                return Err(UsageError::new(
                    "json key \"qpdf\" is only valid for json version > 1",
                )
                .into());
            }
        } else if self
            .configuration
            .json_keys
            .iter()
            .any(|key| matches!(key, JsonKey::Objects | JsonKey::Objectinfo))
        {
            return Err(UsageError::new(
                "json keys \"objects\" and \"objectinfo\" are only valid for json version 1",
            )
            .into());
        }
        Ok(())
    }

    fn report_job_error(&self, error: &Error) -> Result<()> {
        // qpdf's C wrapper streams the prefix, separator, message, and final
        // newline separately (`qpdfjob-c.cc:32-39`). Keeping those writes
        // separate preserves custom-pipeline boundaries as well as bytes.
        let pipeline = self.logger.get_error()?;
        pipeline
            .write(self.message_prefix.as_bytes())
            .map_err(Error::from)?;
        pipeline.write(b": ").map_err(Error::from)?;
        pipeline
            .write(Self::job_error_message(error).as_bytes())
            .map_err(Error::from)?;
        pipeline.write(b"\n").map_err(Error::from)
    }

    fn job_error_message(error: &Error) -> String {
        match error {
            Error::FileIo {
                operation,
                path,
                source,
            } => {
                let source = source.to_string();
                let source = source
                    .split_once(" (os error ")
                    .map_or(source.as_str(), |(message, _)| message);
                format!("{operation} {}: {source}", path.display())
            }
            _ => error.to_string(),
        }
    }

    /// Create a complete JSON-input document with this job's logger already
    /// installed.
    ///
    /// qpdf creates the rootless document and imports JSON before any later
    /// transformations (`QPDFJob.cc:429-482`, `QPDFJob.cc:1708`). Installing
    /// the logger in the seed options preserves import-time warning routing;
    /// replacing it after `Pdf::create_from_json` would lose that boundary.
    pub fn create_from_json<S>(
        &mut self,
        source: S,
        input_name: impl Into<String>,
    ) -> Result<Pdf<Cursor<Vec<u8>>>>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        let pdf = Pdf::create_from_json_with_options(
            source,
            input_name,
            PdfOpenOptions {
                logger: Some(self.logger.clone()),
                ..PdfOpenOptions::default()
            },
        )?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Open a file-backed document with this job's logger installed before
    /// parsing begins.
    ///
    /// This is the ordinary-input half of qpdf's `createQPDF` boundary:
    /// `QPDFJob` applies its document options before `processFile` can emit
    /// repair diagnostics (`QPDFJob.cc:429-462`). The caller supplies policy
    /// options such as repair, weak-crypto allowance, and warning suppression;
    /// the job owns the logger and qpdf-shaped input description.
    pub fn open<R>(
        &mut self,
        source: R,
        input_name: impl Into<String>,
        mut options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        let mut pdf = Pdf::open_with_options(source, options)?;
        // qpdf's createQPDF calls getVersionAsPDFVersion immediately after
        // processFile; that path enters getExtensionLevel and therefore
        // QPDF::getRoot before any job operation emits output
        // (libqpdf/QPDFJob.cc:429-480,1696-1716; QPDF.cc:2306-2368).
        pdf.root_handle()?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Open a file-backed document for qpdf's read-only encryption inspection
    /// path. This is the Rust counterpart of `QPDFJob::createQPDF` retaining
    /// the partially initialized document after `qpdf_e_password` so
    /// `showEncryption` can report the parsed parameters.
    pub fn open_for_encryption_inspection<R>(
        &mut self,
        source: R,
        input_name: impl Into<String>,
        mut options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.into();
        self.input_name = input_name.clone();
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        let mut pdf = Pdf::open_for_encryption_inspection(source, options)?;
        // `--password-is-hex-key` (raw key) authentication intentionally
        // leaves both user/owner password-match flags false on success --
        // it bypasses password derivation entirely -- so those flags alone
        // cannot distinguish a genuinely failed open from a successful
        // raw-key one. `encryption_file_key()` is populated only on
        // successful authentication (any mode); `is_encrypted()` separately
        // distinguishes a plaintext document (no /Encrypt at all, so this
        // must be `false` regardless of key presence) from a genuinely
        // failed encrypted one.
        let authentication_failed = pdf.is_encrypted() && pdf.encryption_file_key().is_none();
        // qpdf's createQPDF returns from its password-error catch before the
        // ordinary post-open root walk. Do not resolve an encrypted root in
        // the same partial state; successful/plaintext opens keep the normal
        // QPDFJob root initialization and warning boundary.
        if !authentication_failed {
            pdf.root_handle()?;
        }
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Apply a partial JSON update before the job's output or inspection
    /// stage, matching qpdf's update-before-transform order.
    pub fn update_from_json<R, S>(
        &mut self,
        pdf: &mut Pdf<R>,
        source: S,
        input_name: impl Into<String>,
    ) -> Result<()>
    where
        R: Read + Seek,
        S: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        pdf.set_logger(self.logger.clone());
        pdf.update_from_json(source, input_name)?;
        self.record_document_warnings(pdf);
        Ok(())
    }

    /// Run one read-only consumer and complete the shared warning/status
    /// boundary after it has finished.
    ///
    /// This mirrors `QPDFJob::writeQPDF` selecting `doInspection` when no
    /// output is created (`QPDFJob.cc:484-516,1646-1693`). The callback owns
    /// the inspection-specific output; the job owns logger identity, lazy
    /// warning collection, and the final status.
    pub fn inspect<R, F, E>(
        &mut self,
        pdf: &mut Pdf<R>,
        inspection: F,
    ) -> std::result::Result<JobExitCode, E>
    where
        R: Read + Seek,
        F: FnOnce(&mut Pdf<R>) -> std::result::Result<(), E>,
        E: From<crate::Error>,
    {
        pdf.set_logger(self.logger.clone());
        inspection(pdf)?;
        self.record_document_warnings(pdf);
        Ok(self.complete(false)?)
    }

    /// Serialize one already-created document and then complete the shared
    /// warning/exit-status boundary.
    ///
    /// JSON construction and stream side-file handling remain owned by the
    /// existing serializer; this method owns only the qpdf `writeQPDF`
    /// lifecycle boundary (`QPDFJob.cc:484-563`). The warning-summary
    /// destination is derived from [`JsonJobOutput`] itself, so callers cannot
    /// provide a destination and an inconsistent `creates_output` flag.
    pub fn write_json<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
    ) -> std::result::Result<JobExitCode, JsonJobError>
    where
        R: Read + Seek,
    {
        self.write_json_with_version(pdf, 2, false, false, false, options, output)
    }

    /// Serialize one already-created document with the requested qpdf JSON
    /// version and optional generated-schema validation.
    #[allow(clippy::too_many_arguments)]
    pub fn write_json_with_version<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        version: i32,
        test_json_schema: bool,
        json_output: bool,
        show_encryption_key: bool,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
    ) -> std::result::Result<JobExitCode, JsonJobError>
    where
        R: Read + Seek,
    {
        let creates_output = matches!(&output, JsonJobOutput::File { .. });
        pdf.set_logger(self.logger.clone());
        super::json::write_json_with_version(
            pdf,
            version,
            test_json_schema,
            json_output,
            show_encryption_key,
            options,
            output,
        )?;
        self.record_document_warnings(pdf);
        Ok(self.complete(creates_output)?)
    }

    /// Record that a stage observed one or more qpdf warnings.
    pub fn record_warnings(&mut self) {
        self.warnings = true;
    }

    /// Record warnings from a parsed document's diagnostic collection.
    pub fn record_document_warnings<R>(&mut self, pdf: &Pdf<R>)
    where
        R: Read + Seek,
    {
        if pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.severity == Severity::Warning)
        {
            self.record_warnings();
        }
    }

    /// Return whether any stage has recorded a warning.
    #[must_use]
    pub fn has_warnings(&self) -> bool {
        self.warnings
    }

    /// Suppress the warning completion message while retaining diagnostics.
    pub fn set_suppress_warnings(&mut self, value: bool) {
        self.suppress_warnings = value;
    }

    /// Return whether warning delivery is suppressed for job-owned documents.
    pub(crate) fn warnings_suppressed(&self) -> bool {
        self.suppress_warnings
    }

    /// Configure qpdf's `warnings-exit-0` behavior.
    pub fn set_warnings_exit_zero(&mut self, value: bool) {
        self.warnings_exit_zero = value;
    }

    /// Complete the shared warning boundary after output or inspection.
    ///
    /// This mirrors `QPDFJob::writeQPDF` and `getExitCode`: all operation
    /// output must be completed by the caller before this method is invoked;
    /// this method emits at most the one qpdf-shaped summary and returns the
    /// corresponding status (`QPDFJob.cc:484-563`).
    pub fn complete(&self, creates_output: bool) -> Result<JobExitCode> {
        if self.warnings && !self.suppress_warnings {
            let suffix = if creates_output {
                "; resulting file may have some problems"
            } else {
                ""
            };
            self.logger.warn(format!(
                "{}: operation succeeded with warnings{suffix}\n",
                self.message_prefix
            ))?;
        }

        if self.warnings && !self.warnings_exit_zero {
            Ok(JobExitCode::Warning)
        } else {
            Ok(JobExitCode::Success)
        }
    }
}

fn parse_object_stream_mode(value: &str) -> Result<ObjectStreamMode> {
    match value {
        "preserve" => Ok(ObjectStreamMode::Preserve),
        "disable" => Ok(ObjectStreamMode::Disable),
        "generate" => Ok(ObjectStreamMode::Generate),
        other => Err(Error::Unsupported(format!(
            "qpdfjob: invalid objectStreams value {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, PdfOpenOptions};
    use std::io::Cursor;

    fn trailer_root_pdf(root: &str) -> Vec<u8> {
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let xref_start = bytes.len();
        bytes.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 1 /Root {root} >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    #[test]
    fn open_rejects_a_dangling_root_before_returning_a_job_document() {
        let mut job = QPDFJob::new();

        assert!(matches!(
            job.open(
                Cursor::new(trailer_root_pdf("99 0 R")),
                "dangling-root.pdf",
                PdfOpenOptions::default(),
            ),
            Err(Error::System(message)) if message == "unable to find /Root dictionary"
        ));
    }

    #[test]
    fn open_rejects_a_non_dictionary_root_before_returning_a_job_document() {
        let mut job = QPDFJob::new();

        assert!(matches!(
            job.open(
                Cursor::new(trailer_root_pdf("42")),
                "wrong-type-root.pdf",
                PdfOpenOptions::default(),
            ),
            Err(Error::System(message)) if message == "unable to find /Root dictionary"
        ));
    }

    #[test]
    fn open_accepts_a_direct_dictionary_root() {
        let mut job = QPDFJob::new();

        assert!(job
            .open(
                Cursor::new(trailer_root_pdf("<< /Type /Catalog >>")),
                "direct-root.pdf",
                PdfOpenOptions::default(),
            )
            .is_ok());
    }

    /// `--password-is-hex-key` authentication intentionally leaves both
    /// user/owner password-match flags false even on success (it bypasses
    /// password derivation entirely). `open_for_encryption_inspection` must
    /// still run the normal post-open root walk for a successful raw-key
    /// open, the same way it does for a successful password-based one --
    /// distinguishing genuine `BadPassword` failure from raw-key success by
    /// whether a file key was installed, not by the password-match flags.
    #[test]
    fn open_for_encryption_inspection_walks_the_root_after_successful_raw_key_auth() {
        let mut source = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/encrypted/v5-aes-256-r6.pdf"),
        )
        .expect("committed encrypted fixture");
        // Corrupt the trailer's /Root reference to a dangling object number
        // (the fixture's /Size is 4, so object 9 cannot exist) -- same
        // length substitution, no xref/offset shift needed. This is the
        // only way to make the raw-key-vs-password-flags distinction this
        // predicate exists to draw actually observable from outside.
        let needle = b"/Root 1 0 R";
        let at = source
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("fixture has a /Root 1 0 R trailer reference");
        source[at..at + needle.len()].copy_from_slice(b"/Root 9 0 R");

        // Known raw file key for this committed fixture (verified with
        // `qpdf --show-encryption-key --password=… tests/fixtures/encrypted/
        // v5-aes-256-r6.pdf`, matching the constant already established in
        // `crates/flpdf-cli/tests/cli_password_hex_key_tests.rs`).
        let hex_key = b"fc459408a5282b7c59daa5162f860e82315679cc04942ef57993bfd287f30290".to_vec();

        let mut job = QPDFJob::new();
        let result = job.open_for_encryption_inspection(
            Cursor::new(source),
            "dangling-root-hex-key.pdf",
            PdfOpenOptions {
                password: hex_key,
                password_is_hex_key: true,
                ..PdfOpenOptions::default()
            },
        );

        match result {
            Err(Error::System(message)) => assert_eq!(message, "unable to find /Root dictionary"),
            // cov:ignore-start: diagnostic panic arms reachable only if this
            // regression test itself starts failing in an unexpected shape;
            // the passing-suite path always takes the arm above.
            Err(other) => {
                panic!("expected the dangling-/Root error, got a different error: {other}")
            }
            Ok(_) => panic!(
                "a successful raw-key open must still surface a dangling /Root, \
                 proving the root walk ran rather than being skipped as a \
                 (mis-detected) authentication failure"
            ),
            // cov:ignore-end
        }
    }

    #[test]
    fn check_result_mapping_preserves_success_and_maps_both_errors() {
        let job = QPDFJob::new();
        assert_eq!(
            job.map_check_result(Ok(JobExitCode::Success)).unwrap(),
            JobExitCode::Success
        );
        assert_eq!(
            job.map_check_result(Err(super::super::check::CheckError::ErrorsDetected))
                .unwrap(),
            JobExitCode::Error
        );
        assert_eq!(
            job.map_check_result(Err(super::super::check::CheckError::Operation(
                Error::Internal("operation failed".to_owned()),
            )))
            .unwrap(),
            JobExitCode::Error
        );
    }

    #[test]
    fn job_output_pipeline_exposes_its_qpdf_identifier() {
        let pipeline = JobOutputPipeline(PipelineHandle::new(crate::pipeline::Discard));

        assert_eq!(pipeline.identifier(), "qpdf job output");
    }

    #[test]
    fn job_output_writer_forwards_bytes_and_flush() {
        let mut writer = JobOutputWriter(PipelineHandle::new(crate::pipeline::Discard));
        std::io::Write::write_all(&mut writer, b"job output").unwrap();
        std::io::Write::flush(&mut writer).unwrap();
    }

    #[test]
    fn job_json_private_handlers_cover_qpdf_scalar_and_choice_shapes() {
        let members = job_json_members(
            &crate::json::Json::parse(
                br#"{"empty":"","choice":"a","string":"text","number":1,"flag":false,"yes":"y"}"#,
            )
            .unwrap(),
        );
        assert!(job_json_string(&members, b"string").unwrap().is_some());
        assert!(job_json_string(&members, b"missing").unwrap().is_none());
        assert!(job_json_bare(&members, b"empty").unwrap());
        assert!(!job_json_bare(&members, b"missing").unwrap());
        assert!(job_json_bare(&members, b"string").is_err());
        assert!(job_json_bare(&members, b"number").is_err());
        assert_eq!(
            job_json_choice(&members, b"choice", &["a", "b"], true).unwrap(),
            Some("a".to_owned())
        );
        assert_eq!(
            job_json_choice(&members, b"missing", &["a", "b"], true).unwrap(),
            None
        );
        assert_eq!(
            job_json_items(&crate::json::Json::parse(br#"["a","b"]"#).unwrap()).len(),
            2
        );
        assert_eq!(
            job_json_items(&crate::json::Json::parse(br#""a""#).unwrap()).len(),
            1
        );
        assert!(job_json_required_string(&members, b"string", ".string").is_ok());
        assert!(job_json_required_string(&members, b"missing", ".missing").is_err());
        assert!(job_json_yn(&members, b"yes").unwrap().unwrap());

        let empty_optional =
            job_json_members(&crate::json::Json::parse(br#"{"choice":""}"#).unwrap());
        assert_eq!(
            job_json_choice(&empty_optional, b"choice", &["a", "b"], false).unwrap(),
            Some(String::new())
        );
        let wrong_type =
            job_json_members(&crate::json::Json::parse(br#"{"choice":false}"#).unwrap());
        assert!(job_json_choice(&wrong_type, b"choice", &["a"], true).is_err());
        let wrong_value =
            job_json_members(&crate::json::Json::parse(br#"{"choice":"c"}"#).unwrap());
        assert!(job_json_choice(&wrong_value, b"choice", &["a"], true).is_err());

        let range_members =
            job_json_members(&crate::json::Json::parse(br#"{"range":"1-2"}"#).unwrap());
        assert!(job_json_range(range_members.get(b"range".as_slice()), ".range").is_ok());
        assert!(job_json_range(None, ".range").is_ok());
        let bad_range_type =
            job_json_members(&crate::json::Json::parse(br#"{"range":false}"#).unwrap());
        assert!(job_json_range(bad_range_type.get(b"range".as_slice()), ".range").is_err());
        let bad_range_syntax =
            job_json_members(&crate::json::Json::parse(br#"{"range":"bad"}"#).unwrap());
        assert!(job_json_range(bad_range_syntax.get(b"range".as_slice()), ".range").is_err());
    }

    #[test]
    fn job_json_private_parsers_cover_encryption_and_writer_choices() {
        for value in ["preserve", "disable", "generate"] {
            assert!(parse_object_stream_mode(value).is_ok());
        }
        assert!(parse_object_stream_mode("other").is_err());
        for value in ["none", "generalized", "specialized", "all"] {
            let level = parse_json_decode_level(value);
            assert_eq!(json_decode_level_for_output(level).as_qpdf_str(), value);
        }
        assert_eq!(parse_json_version("1"), 1);
        assert_eq!(parse_json_version("2"), 2);
        assert_eq!(parse_json_version("latest"), 2);
        assert_eq!(parse_json_version(""), 2);
        assert!(parse_job_version("1.7.3", ".version").is_ok());
        assert!(parse_job_version("invalid", ".version").is_err());
        assert_eq!(parse_positive_usize(b"2", ".count").unwrap(), 2);
        assert!(parse_positive_usize(b"0", ".count").is_err());
        assert!(parse_positive_usize(b"not-number", ".count").is_err());

        for value in ["all", "annotate", "form", "assembly", "none"] {
            let mut permissions = crate::PermissionsConfig::default();
            job_json_modify_permission(value, &mut permissions).unwrap();
        }
        assert!(
            job_json_modify_permission("invalid", &mut crate::PermissionsConfig::default())
                .is_err()
        );
        for value in ["full", "low", "none"] {
            let mut permissions = crate::PermissionsConfig::default();
            job_json_print_permission(value, &mut permissions).unwrap();
        }
        assert!(
            job_json_print_permission("invalid", &mut crate::PermissionsConfig::default()).is_err()
        );

        let encrypt_40 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","40bit":{"annotate":"y","extract":"n","modify":"none","print":"low"}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_40, true).is_ok());
        let encrypt_128 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","128bit":{"accessibility":"y","annotate":"n","assemble":"y","cleartextMetadata":"","extract":"n","form":"y","modifyOther":"n","modify":"all","print":"full","forceV4":"","useAes":"n"}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_128, true).is_ok());
        let encrypt_256 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","256bit":{"forceR5":"","allowInsecure":""}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_256, true).is_ok());
        let encrypt_128_rc4 =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"o","128bit":{}}"#)
                .unwrap();
        assert!(parse_job_encrypt(&encrypt_128_rc4, true).is_ok());
        let encrypt_256_r6 =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"o","256bit":{}}"#)
                .unwrap();
        assert!(parse_job_encrypt(&encrypt_256_r6, true).is_ok());
        let missing_password = crate::json::Json::parse(br#"{"128bit":{}}"#).unwrap();
        assert!(parse_job_encrypt(&missing_password, true).is_err());
        let duplicate_key_length = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","40bit":{},"128bit":{}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&duplicate_key_length, true).is_err());
        let no_key_length =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"o"}"#).unwrap();
        assert!(parse_job_encrypt(&no_key_length, true).is_err());
        assert!(parse_job_encrypt(&encrypt_40, false).is_err());
    }

    #[test]
    fn job_json_private_parser_covers_full_handler_dispatch() {
        let tempdir = tempfile::tempdir().unwrap();
        let password_file = tempdir.path().join("password.txt");
        std::fs::write(&password_file, b"file-password\n").unwrap();
        let mut root = serde_json::Map::new();
        for (key, value) in [
            ("inputFile", "input.pdf"),
            ("outputFile", "output.pdf"),
            ("password", "password"),
            ("jsonInput", ""),
            ("qdf", ""),
            ("preserveUnreferenced", ""),
            ("newlineBeforeEndstream", ""),
            ("normalizeContent", "y"),
            ("streamData", "compress"),
            ("compressStreams", "y"),
            ("recompressFlate", ""),
            ("decodeLevel", "all"),
            ("decrypt", ""),
            ("deterministicId", ""),
            ("staticAesIv", ""),
            ("staticId", ""),
            ("noOriginalObjectIds", ""),
            ("allowWeakCrypto", ""),
            ("progress", ""),
            ("verbose", ""),
            ("objectStreams", "generate"),
            ("minVersion", "1.4"),
            ("forceVersion", "1.7"),
            ("linearizePass1", "pass1.pdf"),
            ("linearize", ""),
            ("updateFromJson", "update.json"),
            ("collate", "2"),
            ("jsonOutput", "latest"),
            ("jsonStreamPrefix", "stream"),
            ("jsonStreamData", "file"),
            ("testJsonSchema", ""),
            ("showEncryptionKey", ""),
            ("noWarn", ""),
            ("warningExit0", ""),
            ("check", ""),
            ("showEncryption", ""),
            ("removePageLabels", ""),
            ("preserveUnreferencedResources", ""),
            ("oiMinArea", "100"),
            ("oiMinHeight", "100"),
            ("oiMinWidth", "100"),
            ("reportMemoryUsage", ""),
            ("jobJsonFile", "nested.json"),
        ] {
            root.insert(key.to_owned(), serde_json::json!(value));
        }
        root.insert(
            "passwordFile".to_owned(),
            serde_json::json!(password_file.to_string_lossy()),
        );
        root.insert("jsonKey".to_owned(), serde_json::json!(["qpdf", "pages"]));
        root.insert(
            "jsonObject".to_owned(),
            serde_json::json!(["trailer", "1 0 R"]),
        );
        root.insert(
            "encrypt".to_owned(),
            serde_json::json!({
                "userPassword": "u",
                "ownerPassword": "o",
                "128bit": {"useAes": "y"}
            }),
        );
        root.insert(
            "pages".to_owned(),
            serde_json::json!([{"file": "page.pdf", "password": "page-password", "range": "1"}]),
        );
        root.insert(
            "overlay".to_owned(),
            serde_json::json!({"file": "overlay.pdf", "from": "1", "to": "1", "repeat": "1"}),
        );
        root.insert(
            "underlay".to_owned(),
            serde_json::json!([{"file": "underlay.pdf"}]),
        );
        root.insert(
            "addAttachment".to_owned(),
            serde_json::json!([{
                "file": "attachment.bin", "filename": "shown.bin", "key": "shown-key",
                "mimetype": "application/octet-stream", "description": "description",
                "creationdate": "D:20220131134246-05'00'", "moddate": "D:20220131134246-05'00'",
                "replace": ""
            }]),
        );
        root.insert(
            "copyAttachmentsFrom".to_owned(),
            serde_json::json!([{"file": "copy.pdf", "password": "copy-password", "prefix": "p-"}]),
        );
        root.insert(
            "removeAttachment".to_owned(),
            serde_json::json!(["old-key"]),
        );
        root.insert(
            "setPageLabels".to_owned(),
            serde_json::json!(["1:D", "2:a/2/prefix"]),
        );
        let json = serde_json::Value::Object(root).to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(&json)
            .expect("all parsed qpdf job handlers should accept their valid shapes");

        let mut latest = QPDFJob::new();
        latest
            .initialize_from_json_partial(r#"{"json":""}"#)
            .unwrap();
        let unknown = crate::json::Json::parse(br#"{"potato":""}"#).unwrap();
        let error = validate_job_json_schema(&unknown).unwrap_err();
        assert!(error.to_string().contains("qpdf: job json has errors:"));
    }

    #[test]
    fn job_json_page_label_parser_covers_styles_and_failures() {
        let entries = parse_job_page_labels(
            &[
                "1:D".to_owned(),
                "2:a".to_owned(),
                "3:A".to_owned(),
                "4:r".to_owned(),
                "5:R".to_owned(),
                "6:".to_owned(),
            ],
            6,
        )
        .unwrap();
        assert_eq!(entries.len(), 6);
        assert!(parse_job_page_labels(&["bad".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["q:D".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["2:D".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["1:D".to_owned(), "1:a".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["7:D".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["1:X".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["1:D/foo".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["1:D/0".to_owned()], 6).is_err());
        assert!(parse_job_page_labels(&["rx:D".to_owned()], 6).is_err());
        let relative = parse_job_page_labels(
            &[
                "1:D".to_owned(),
                "r2:a/2/prefix".to_owned(),
                "z:R//end".to_owned(),
            ],
            6,
        )
        .unwrap();
        assert_eq!(relative[1].0, 4);
        assert_eq!(relative[2].0, 5);
    }

    #[test]
    fn job_json_private_parser_covers_remaining_choices_and_validation_errors() {
        for stream_data in ["compress", "preserve", "uncompress"] {
            let mut job = QPDFJob::new();
            job.initialize_from_json_partial(&format!(r#"{{"streamData":"{stream_data}"}}"#))
                .unwrap();
        }
        for stream_data in ["none", "inline", "file"] {
            let mut job = QPDFJob::new();
            job.initialize_from_json_partial(&format!(r#"{{"jsonStreamData":"{stream_data}"}}"#))
                .unwrap();
        }
        for resources in ["auto", "yes", "no"] {
            let mut job = QPDFJob::new();
            job.initialize_from_json_partial(&format!(
                r#"{{"removeUnreferencedResources":"{resources}"}}"#
            ))
            .unwrap();
        }

        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"addAttachment":[{"file":"/"}]}"#)
            .expect_err("a root path has no attachment basename");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"overlay":[{}]}"#)
            .expect_err("overlay file is required");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"overlay":{"file":"overlay.pdf"}}"#)
            .unwrap();
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"pages":[{}]}"#)
            .expect_err("page file is required");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"pages":{"file":"page.pdf","range":"1-2"}}"#)
            .unwrap();

        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"jsonKey":[1]}"#)
            .expect_err("jsonKey entries must be strings");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"jsonKey":["unknown"]}"#)
            .expect_err("jsonKey choices must be known");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"jsonObject":[1]}"#)
            .expect_err("jsonObject entries must be strings");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"jsonObject":["unknown"]}"#)
            .expect_err("jsonObject selectors must be valid");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"removeAttachment":[1]}"#)
            .expect_err("attachment names must be strings");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"setPageLabels":[1]}"#)
            .expect_err("page labels must be strings");

        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(r#"{"inputFile":"input.pdf","empty":""}"#)
            .expect_err("empty and inputFile are mutually exclusive");
    }

    #[test]
    fn job_input_setters_preserve_qpdf_configuration_boundaries() {
        let mut job = QPDFJob::new();
        job.set_input_file("input.pdf").unwrap();
        assert_eq!(job.input_name(), "input.pdf");
        assert!(job.set_input_file("second.pdf").is_err());

        job.set_output_file("output.pdf").unwrap();
        assert!(job.set_output_file("second-output.pdf").is_err());
        job.set_password(b"password".to_vec());

        let mut empty_job = QPDFJob::new();
        empty_job
            .initialize_from_json_partial(r#"{"empty":""}"#)
            .unwrap();
        assert!(empty_job.set_input_file("input.pdf").is_err());

        let mut replace_job = QPDFJob::new();
        replace_job
            .initialize_from_json_partial(r#"{"replaceInput":""}"#)
            .unwrap();
        assert!(replace_job.set_output_file("output.pdf").is_err());
    }
}
