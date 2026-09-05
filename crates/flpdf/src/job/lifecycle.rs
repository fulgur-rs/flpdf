//! qpdf correspondence: `QPDFJob` shared state and completion boundary.
//!
//! This module owns the state that qpdf keeps on `QPDFJob` itself rather than
//! on an individual CLI route: the message prefix, logger, progress callback,
//! warning aggregation, and the single warning-completion summary. JSON and
//! ordinary page-inspection dispatch are layered on top of this state; write,
//! page-transform, and remaining inspection consumers are later job slices.

use super::attachments::{AttachmentAddOptions, AttachmentCopyOptions, AttachmentCopySource};
use super::image_optimization::{optimize_images, ImageOptimizationOptions};
use super::json::{JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData};
use super::overlay::{apply_overlay_specs, OverlayKind, OverlaySpec};
use super::page_range::PageRange;
use super::page_specs::{PageSpecInput, PageSpecJobOutput};
use super::page_split::SplitPageOptions;
use super::resource_pruning::RemoveUnreferencedResources;
use super::rotate::{apply_rotate_to_pages, flatten_rotation_on_pages};
use super::rotate_spec::RotateSpec;
use crate::encryption::{EncryptMethod, EncryptParams, PasswordMode};
use crate::json_inspect::{DecodeLevel as JsonDecodeLevel, JsonKey, JsonObjectSelector};
use crate::linearization::{show_linearization_pdf_with_warnings, ShowLinearizationError};
use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};
use crate::{
    AcroFormDocumentHelper, Error, ObjectRef, ObjectStreamMode, PageDocumentHelper,
    PageObjectHelper, Pdf, PdfOpenOptions, PdfWriter, QPDFLogger, ReadSeek, Result, Severity,
    UsageError, WriterConfiguration,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

type ProgressHandler = Box<dyn FnMut(u8) -> Result<()> + 'static>;
type SharedProgressHandler = Rc<RefCell<ProgressHandler>>;

fn path_description_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    {
        path.to_string_lossy().into_owned().into_bytes()
    }
}

/// qpdf's `flattenAnnotations` job setting.
///
/// The three modes map to the `required` and `forbidden` annotation flag masks
/// used by `QPDFPageDocumentHelper::flattenAnnotations`
/// (`libqpdf/QPDFJob_config.cc:190-200`). Keeping the choice and its masks in
/// the job layer gives both job JSON and the CLI one canonical qpdf mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlattenAnnotationsMode {
    /// Flatten all annotations except Invisible and Hidden annotations.
    All,
    /// Flatten annotations that render on screen, excluding NoView ones.
    Screen,
    /// Flatten annotations that are marked for printing.
    Print,
}

impl FlattenAnnotationsMode {
    /// Return qpdf's `(required, forbidden)` annotation flag masks.
    pub const fn qpdf_flags(self) -> (i64, i64) {
        match self {
            Self::All => (0, 0x3),
            Self::Screen => (0, 0x23),
            Self::Print => (0x4, 0x3),
        }
    }
}

/// qpdf's `QPDFJob::Members::DEFAULT_KEEP_FILES_OPEN_THRESHOLD`
/// (`include/qpdf/QPDFJob.hh:579`).
const DEFAULT_KEEP_FILES_OPEN_THRESHOLD: usize = 200;

/// Convert a qpdf job-JSON string value to the platform path representation
/// used at the filesystem boundary.
///
/// qpdf keeps filenames in `std::string` and passes their bytes to POSIX
/// `fopen` (`libqpdf/QUtil.cc:489-517`). On Windows, qpdf treats the same
/// value as UTF-8 and converts it to UTF-16, replacing malformed sequences
/// with U+FFFD (`libqpdf/QUtil.cc:467-485,1622-1625`). Do not route Unix
/// values through a Rust `String`: a literal non-UTF-8 filename byte is a
/// valid filesystem name there and must survive unchanged.
fn path_from_qpdf_json_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    }

    #[cfg(windows)]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }

    #[cfg(not(any(unix, windows)))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn path_component_to_qpdf_bytes(component: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        component.as_bytes().to_vec()
    }

    #[cfg(windows)]
    {
        component.to_string_lossy().into_owned().into_bytes()
    }

    #[cfg(not(any(unix, windows)))]
    {
        component.to_string_lossy().into_owned().into_bytes()
    }
}

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
/// settings needed for job initialization; full command-line transform
/// dispatch remains in the operation-specific job slices.
#[derive(Debug, Clone, Default)]
struct JobConfiguration {
    input_file: Option<PathBuf>,
    empty_input: bool,
    output_file: Option<PathBuf>,
    password: Vec<u8>,
    copy_encryption: Option<PathBuf>,
    encryption_file_password: Vec<u8>,
    password_mode: PasswordMode,
    ignore_xref_streams: bool,
    password_is_hex_key: bool,
    suppress_password_recovery: bool,
    suppress_recovery: bool,
    verbose: bool,
    json_input: bool,
    update_from_json: Option<PathBuf>,
    replace_input: bool,
    check: bool,
    show_npages: bool,
    show_pages: bool,
    show_page_images: bool,
    check_linearization: bool,
    require_output: bool,
    progress: bool,
    /// qpdf stores this as a signed `int`, so a negative non-zero value must
    /// survive configuration and fail only when the split loop converts it
    /// to `size_t`.
    split_pages: Option<i32>,
    /// qpdf's explicit `--keep-files-open=y|n` setting. `None` selects the
    /// automatic distinct-page-source threshold in `handle_page_specs`.
    keep_files_open: Option<bool>,
    /// qpdf's automatic source-count threshold (`200` by default).
    keep_files_open_threshold: Option<usize>,
    /// qpdf stores rotations in a map keyed by the original page-range
    /// string (`QPDFJob.cc:369-415`); assigning the same range replaces the
    /// earlier rotation and iteration is lexical by range.
    rotations: BTreeMap<String, RotateSpec>,
    remove_restrictions: bool,
    coalesce_contents: bool,
    /// qpdf's image transformation toggles and thresholds. The actual image
    /// traversal remains in `job::image_optimization`; this job state only
    /// carries the generated JSON handler values to the canonical phase.
    optimize_images: bool,
    externalize_inline_images: bool,
    image_options: ImageOptimizationOptions,
    flatten_annotations: Option<FlattenAnnotationsMode>,
    flatten_rotation: bool,
    generate_appearances: bool,
    writer: WriterConfiguration,
    linearize: bool,
    linearize_pass1: Option<PathBuf>,
    allow_weak_crypto: bool,
    page_specs: Vec<JobPageConfig>,
    collate: Option<Vec<usize>>,
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
    json_stream_data_set: bool,
    json_stream_prefix: Option<Vec<u8>>,
    test_json_schema: bool,
    show_encryption_key: bool,
    show_encryption: bool,
    is_encrypted: bool,
    requires_password: bool,
    report_memory_usage: bool,
    show_xref: bool,
    show_linearization: bool,
    show_object: Option<JobObjectSelector>,
    show_raw_stream_data: bool,
    show_filtered_stream_data: bool,
    list_attachments: bool,
    show_attachment: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobObjectSelector {
    Trailer,
    Object(ObjectRef),
    Null,
    NoObject,
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
        "showAttachment",
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

fn read_job_json_file(path: &Path) -> Result<crate::json::Json> {
    let bytes = std::fs::read(path)
        .map_err(|error| Error::file_io("read job-json file", path.to_owned(), error))?;
    let value =
        crate::json::Json::parse(&bytes).map_err(|error| Error::System(error.to_string()))?;
    if !value.is_dictionary() {
        return Err(Error::Usage(UsageError::new(
            "top-level object is supposed to be a dictionary",
        )));
    }
    validate_job_json_schema(&value)?;
    Ok(value)
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
            "JSON handler: value at {path} is not of expected type"
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
    if !required && value.is_empty() {
        return Ok(Some(String::new()));
    }
    if let Some(choice) = choices.iter().find(|choice| value == choice.as_bytes()) {
        // qpdf compares the raw std::string value with these ASCII choice
        // literals. Return the literal after the byte comparison rather than
        // lossy-decoding an arbitrary JSON string before matching it.
        return Ok(Some((*choice).to_owned()));
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

fn job_json_rotate_range(value: &[u8]) -> String {
    let value = String::from_utf8_lossy(value);
    let Some((_, range)) = value.split_once(':') else {
        return "1-z".to_owned();
    };
    if range.is_empty() {
        "1-z".to_owned()
    } else {
        range.to_owned()
    }
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
    let allow_insecure = job_json_bare(&settings, b"allowInsecure")?;
    if key_length == "256bit"
        && owner_password.is_empty()
        && !user_password.is_empty()
        && !allow_insecure
    {
        return Err(Error::Usage(UsageError::new(
            "A PDF with a non-empty user password and an empty owner password encrypted with a 256-bit key is insecure as it can be opened without a password. If you really want to do this, you must also give the --allow-insecure option before the -- that follows --encrypt.",
        )));
    }
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

fn parse_job_version(value: &[u8], path: &str) -> Result<(String, i64)> {
    let value = std::str::from_utf8(value).map_err(|_| {
        Error::Usage(UsageError::new(format!(
            "{path}: version must be valid UTF-8"
        )))
    })?;
    crate::parse_pdf_version_spec(value)
        .ok_or_else(|| Error::Usage(UsageError::new(format!("{path}: invalid version {value}"))))
}

fn qpdf_is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' | b'\x0b')
}

fn parse_qpdf_collate_uint(component: &[u8]) -> Result<usize> {
    // QUtil::string_to_ull receives a NUL-terminated c_str(), so embedded NUL
    // bytes terminate both its sign check and strtoull's digit scan.
    let component = &component[..component
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(component.len())];
    let mut index = 0;
    while component
        .get(index)
        .is_some_and(|&byte| qpdf_is_space(byte))
    {
        index += 1;
    }
    if component.get(index) == Some(&b'-') {
        return Err(Error::System(format!(
            "underflow converting {} to 64-bit unsigned integer",
            String::from_utf8_lossy(component)
        )));
    }
    if component.get(index) == Some(&b'+') {
        index += 1;
    }

    let digit_start = index;
    let mut value = 0_u64;
    while let Some(&byte @ b'0'..=b'9') = component.get(index) {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| {
                Error::System(format!(
                    "overflow converting {} to 64-bit unsigned integer",
                    String::from_utf8_lossy(component)
                ))
            })?;
        index += 1;
    }
    if index == digit_start {
        return Ok(0);
    }
    if value > u64::from(u32::MAX) {
        return Err(Error::System(format!(
            "integer out of range converting {value} from a 8-byte unsigned type to a 4-byte unsigned type"
        )));
    }
    Ok(value as usize)
}

fn parse_qpdf_collate_parameter(parameter: &[u8]) -> Result<Vec<usize>> {
    if parameter.is_empty() {
        return Ok(vec![1]);
    }

    let mut values = Vec::new();
    let mut position = 0;
    loop {
        let end = parameter[position..]
            .iter()
            .position(|&byte| byte == b',')
            .map(|offset| position + offset);
        // qpdf passes the comma's absolute index as the *count* argument to
        // std::string::substr rather than subtracting position. Preserve that
        // source behavior for malformed middle components such as `2,,3`.
        let count = end.unwrap_or(usize::MAX);
        let component_end = position.saturating_add(count).min(parameter.len());
        let component = &parameter[position..component_end];
        if component.is_empty() {
            return Err(Error::Usage(UsageError::new("--collate: trailing comma")));
        }
        values.push(parse_qpdf_collate_uint(component)?);
        let Some(end) = end else {
            break;
        };
        position = end + 1;
    }
    Ok(values)
}

fn parse_job_split_pages(value: &[u8]) -> Result<i32> {
    // qpdf's Config::splitPages treats an empty parameter as one page
    // (`libqpdf/QPDFJob_config.cc:597-609`); preserve that generated-handler
    // default instead of treating an empty JSON string as an absent option.
    if value.is_empty() {
        return Ok(1);
    }
    let text = String::from_utf8_lossy(value);
    // qpdf converts a non-empty parameter with `QUtil::string_to_int`
    // (`libqpdf/QPDFJob_config.cc:604-609`), whose `strtoll` stage performs
    // no conversion and returns 0 for a string with no leading digit run --
    // and 0 is falsy in qpdf's own `if (m->split_pages)` checks, so a
    // malformed value behaves exactly like an explicit "0": both fall
    // through to an ordinary, unsplit write rather than being rejected.
    // Confirmed live: `splitPages: "not-a-number"` succeeds and writes one
    // ordinary output file.
    match qpdf_string_to_int_checked(&text) {
        QpdfIntParse::NoDigits => Ok(0),
        // A negative value is truthy in qpdf's `if (m->split_pages)` check
        // and only fails later, inside the actual split loop, when qpdf
        // narrows it to an unsigned chunk size (`QIntC::to_size`,
        // `libqpdf/QPDFJob.cc:2970`). Preserve the signed value here so the
        // split path can reproduce that late conversion error.
        QpdfIntParse::Value(count) => Ok(count),
        QpdfIntParse::Overflow(_) => Err(Error::Usage(UsageError::new(format!(
            ".splitPages: invalid page count {text}"
        )))),
    }
}

fn parse_job_compression_level(value: &[u8]) -> Result<i32> {
    let text = String::from_utf8_lossy(value);
    match qpdf_string_to_int_checked(&text) {
        QpdfIntParse::NoDigits => Ok(0),
        QpdfIntParse::Value(level) => Ok(level),
        QpdfIntParse::Overflow(message) => Err(Error::System(message)),
    }
}

fn parse_job_object_selector(value: &[u8]) -> Result<JobObjectSelector> {
    let value = String::from_utf8_lossy(value);
    if value == "trailer" {
        return Ok(JobObjectSelector::Trailer);
    }

    let (number, generation) = value.split_once(',').unwrap_or((&value, "0"));
    let number = parse_job_selector_integer(number)?;
    let generation = if generation.is_empty() {
        0
    } else {
        parse_job_selector_integer(generation)?
    };
    if number <= 0 {
        return Ok(JobObjectSelector::NoObject);
    }
    if !(0..=i32::from(u16::MAX)).contains(&generation) {
        return Ok(JobObjectSelector::Null);
    }
    Ok(JobObjectSelector::Object(ObjectRef::new(
        u32::try_from(number).expect("positive i32 fits u32"),
        u16::try_from(generation).expect("validated u16 generation"),
    )))
}

fn parse_job_selector_integer(value: &str) -> Result<i32> {
    let original = value;
    let value = value.trim_start_matches(|character| {
        matches!(
            character,
            ' ' | '\n' | '\r' | '\t' | '\u{000c}' | '\u{000b}'
        )
    });
    let sign_len = usize::from(matches!(value.as_bytes().first(), Some(b'+') | Some(b'-')));
    let digits_end = sign_len
        + value[sign_len..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
    if digits_end == sign_len {
        return Ok(0);
    }
    let prefix = &value[..digits_end];
    let parsed = prefix.parse::<i128>().map_err(|_| {
        Error::Usage(UsageError::new(format!(
            "overflow/underflow converting {original} to 64-bit integer"
        )))
    })?;
    if !(i128::from(i64::MIN)..=i128::from(i64::MAX)).contains(&parsed) {
        return Err(Error::Usage(UsageError::new(format!(
            "overflow/underflow converting {original} to 64-bit integer"
        ))));
    }
    let parsed = parsed as i64;
    i32::try_from(parsed).map_err(|_| {
        Error::Usage(UsageError::new(format!(
            "integer out of range converting {parsed} from a 8-byte signed type to a 4-byte signed type"
        )))
    })
}

fn parse_job_attachment(value: &crate::json::Json, path: &str) -> Result<AttachmentAddOptions> {
    let members = job_json_members(value);
    let file = job_json_required_string(&members, b"file", &format!("{path}.file"))?;
    let path = path_from_qpdf_json_bytes(&file);
    let basename = path
        .file_name()
        .map(path_component_to_qpdf_bytes)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::Usage(UsageError::new(
                "file for --add-attachment may not be empty",
            ))
        })?;
    let filename = job_json_string(&members, b"filename")?.unwrap_or_else(|| basename.clone());
    let key = job_json_string(&members, b"key")?.unwrap_or(basename);
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
            path: path_from_qpdf_json_bytes(&file),
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
    input_name_bytes: Vec<u8>,
    message_prefix: String,
    warnings: bool,
    suppress_warnings: bool,
    warnings_exit_zero: bool,
    progress_handler: Option<SharedProgressHandler>,
    configuration: JobConfiguration,
}

/// Fluent configuration proxy for the qpdf `QPDFJob::Config` surface.
///
/// qpdf returns a shared Config object whose setters mutate the owning job and
/// return the same proxy (`include/qpdf/QPDFJob.hh:317-375`). Rust expresses
/// that lifetime as a mutable borrow, so the proxy cannot outlive the
/// `QPDFJob` it configures and every setter remains on the canonical job state.
pub struct QPDFJobConfig<'a> {
    job: &'a mut QPDFJob,
}

impl Default for QPDFJob {
    fn default() -> Self {
        Self::new()
    }
}

impl QPDFJob {
    /// Return a fluent proxy for the qpdf job configuration subset used by
    /// direct API consumers.
    #[must_use]
    pub fn config(&mut self) -> QPDFJobConfig<'_> {
        QPDFJobConfig { job: self }
    }

    /// Parse one qpdf `--collate` parameter into its ordered group sizes.
    ///
    /// This is the shared job configuration entry point for the CLI and JSON
    /// paths. It mirrors `QPDFJob::Config::collate(std::string const&)`
    /// (`libqpdf/QPDFJob_config.cc:95-125`) and its unsigned conversion
    /// through `QUtil::string_to_ull` (`libqpdf/QUtil.cc:396-425`).
    pub fn parse_collate(value: &str) -> Result<Vec<usize>> {
        parse_qpdf_collate_parameter(value.as_bytes())
    }

    /// Parse qpdf's unsigned `--keep-files-open-threshold` parameter.
    ///
    /// qpdf delegates this value to `QUtil::string_to_uint`, whose
    /// `strtoull` conversion accepts an optional leading `+`, leading
    /// whitespace, and a valid digit prefix while rejecting underflow and
    /// values outside `unsigned int` (`libqpdf/QPDFJob_config.cc:350-353`,
    /// `libqpdf/QUtil.cc:396-425`). Reuse the same byte parser as the
    /// qpdf-shaped collate configuration rather than Rust's stricter
    /// `usize::parse`.
    pub fn parse_keep_files_open_threshold(value: &str) -> Result<usize> {
        parse_qpdf_collate_uint(value.as_bytes())
    }

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
            input_name_bytes: Vec::new(),
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

    /// Replace the job's logger with a private qpdf-style logger configured
    /// for the supplied output and error pipelines.
    ///
    /// This is the Rust pipeline equivalent of qpdf's deprecated
    /// `QPDFJob::setOutputStreams` (`libqpdf/QPDFJob.cc:327-333`), which
    /// creates a private logger before assigning the two streams.
    pub fn set_output_streams(
        &mut self,
        output: Option<PipelineHandle>,
        error: Option<PipelineHandle>,
    ) {
        let logger = QPDFLogger::create();
        logger.set_output_streams(output, error);
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
        let input_name = input_name.into();
        self.input_name_bytes = input_name.as_bytes().to_vec();
        self.input_name = input_name;
    }

    /// Return the input name retained by this job.
    #[must_use]
    pub fn input_name(&self) -> &str {
        &self.input_name
    }

    /// Return the qpdf input name without a UTF-8 projection.
    #[must_use]
    pub fn input_name_bytes(&self) -> &[u8] {
        &self.input_name_bytes
    }

    /// Set the qpdf input name from the byte-preserving argv/input boundary.
    ///
    /// The existing [`Self::input_name`] accessor remains a lossy display
    /// projection for text-only callers; logger/report paths use this raw form
    /// so Unix filenames are reproduced exactly.
    pub fn set_input_name_bytes(&mut self, input_name: impl AsRef<[u8]>) {
        self.input_name_bytes = input_name.as_ref().to_vec();
        self.input_name = String::from_utf8_lossy(&self.input_name_bytes).into_owned();
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
        self.set_input_name_bytes(path_description_bytes(&input_file));
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

    /// Enable qpdf's verbose job diagnostics.
    ///
    /// Corresponds to `QPDFJob::Config::verbose` (`libqpdf/QPDFJob_config.cc:
    /// 637-645`). The setting is consumed by the canonical page-spec and
    /// writer routes, while the logger and message prefix remain owned by this
    /// job.
    pub fn set_verbose(&mut self, value: bool) {
        self.configuration.verbose = value;
    }

    /// Set qpdf's explicit secondary-source file lifetime policy.
    ///
    /// `false` selects the close-and-reopen source path used by qpdf when a
    /// page job has too many distinct input files; `true` keeps those sources
    /// open for the job. The setting affects only `handle_page_specs`, just as
    /// qpdf's `QPDFJob::Config::keepFilesOpen` does.
    pub fn set_keep_files_open(&mut self, value: bool) {
        self.configuration.keep_files_open = Some(value);
    }

    /// Override qpdf's automatic `--keep-files-open` source-count threshold.
    pub fn set_keep_files_open_threshold(&mut self, value: usize) {
        self.configuration.keep_files_open_threshold = Some(value);
    }

    /// Return qpdf's effective keep-open decision for one page-spec list.
    ///
    /// qpdf counts distinct page-spec filenames, not source-document object
    /// occurrences (`QPDFJob.cc:2374-2383`). The Rust page boundary has
    /// already assigned one source index to each literal filename, so the
    /// distinct source-index count is the same observable set operation.
    #[must_use]
    pub fn keep_files_open_for_page_specs(&self, specs: &[PageSpecInput]) -> bool {
        self.configuration.keep_files_open.unwrap_or_else(|| {
            let distinct_sources = specs
                .iter()
                .map(|spec| spec.source_index)
                .collect::<BTreeSet<_>>()
                .len();
            distinct_sources
                <= self
                    .configuration
                    .keep_files_open_threshold
                    .unwrap_or(DEFAULT_KEEP_FILES_OPEN_THRESHOLD)
        })
    }

    /// Emit qpdf's automatic keep-open selection line for a page-spec job.
    ///
    /// qpdf reports this before opening foreign page sources and only when the
    /// caller did not explicitly configure `--keep-files-open`
    /// (`libqpdf/QPDFJob.cc:2374-2386`).
    pub fn report_page_spec_selection(&self, specs: &[PageSpecInput]) -> Result<()> {
        if !self.configuration.verbose || self.configuration.keep_files_open.is_some() {
            return Ok(());
        }
        let mut message = self.message_prefix.as_bytes().to_vec();
        message.extend_from_slice(b": selecting --keep-open-files=");
        message.extend_from_slice(if self.keep_files_open_for_page_specs(specs) {
            b"y\n"
        } else {
            b"n\n"
        });
        self.logger.info(message)
    }

    /// Emit qpdf's foreign-source processing line while a page source is
    /// opened by the surrounding page-spec caller.
    ///
    /// `QPDFJob::handlePageSpecs` owns this diagnostic in qpdf, but the Rust
    /// caller supplies already-opened `Pdf` values to the canonical job
    /// method. Keeping this small facade on `QPDFJob` preserves the same
    /// logger/prefix and raw filename boundary without a CLI-owned template.
    pub fn report_page_source_processing(&self, source_name: impl AsRef<[u8]>) -> Result<()> {
        if !self.configuration.verbose {
            return Ok(());
        }
        let mut message = self.message_prefix.as_bytes().to_vec();
        message.extend_from_slice(b": processing ");
        message.extend_from_slice(source_name.as_ref());
        message.push(b'\n');
        self.logger.info(message)
    }

    /// Whether this job's qpdf verbose setting is enabled.
    pub(crate) fn verbose(&self) -> bool {
        self.configuration.verbose
    }

    /// Include the derived encryption key in check/show-encryption output.
    ///
    /// This is the job-owned equivalent of qpdf's `--show-encryption-key`
    /// switch; JSON output already carries the same setting through its
    /// dedicated options.
    pub fn set_show_encryption_key(&mut self, show: bool) {
        self.configuration.show_encryption_key = show;
    }

    /// Select qpdf's optional image details for the `showPages` inspection.
    ///
    /// This is the CLI-side setter for `QPDFJob::Config::withImages`; the
    /// job-JSON parser stores the same setting in `JobConfiguration`.
    pub fn set_with_images(&mut self, show: bool) {
        self.configuration.show_page_images = show;
    }

    pub(crate) fn show_page_images(&self) -> bool {
        self.configuration.show_page_images
    }

    pub(crate) fn show_encryption_key(&self) -> bool {
        self.configuration.show_encryption_key
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

    /// Initialize the qpdf-compatible argument set supported by this job.
    ///
    /// This mirrors `QPDFJob::initializeFromArgv` for one input, one output,
    /// deterministic/static IDs, object-stream mode, password, decrypt,
    /// check, progress reporting, and the `--keep-files-open` options. Other
    /// command-line options are handled by the CLI.
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
                    _ if argument.starts_with("--keep-files-open=") => {
                        let value = &argument["--keep-files-open=".len()..];
                        configuration.keep_files_open = match value {
                            "y" => Some(true),
                            "n" => Some(false),
                            _ => {
                                return Err(UsageError::new(format!(
                                    "invalid value for --keep-files-open: {value}"
                                ))
                                .into())
                            }
                        };
                    }
                    _ if argument.starts_with("--keep-files-open-threshold=") => {
                        let value = &argument["--keep-files-open-threshold=".len()..];
                        configuration.keep_files_open_threshold =
                            Some(parse_qpdf_collate_uint(value.as_bytes())?);
                    }
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
        let input_name = self
            .configuration
            .input_file
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        self.set_input_name(input_name);
        self.warnings = false;
        Ok(())
    }

    /// Initialize the qpdf-compatible job-JSON fields supported by this
    /// lifecycle.
    ///
    /// This includes the qpdf 11.9.0 generated handler surface: input/output
    /// setup, reader policy, writer settings, encryption, page transformations,
    /// attachments, page selection, inspections, JSON output, memory reporting,
    /// and nested `jobJsonFile` includes.
    ///
    /// # Errors
    ///
    /// Returns an error if `json` is not a dictionary, if a nested job file
    /// cannot be read or parsed, if a value violates qpdf's generated handler
    /// contract, or if the non-partial form fails final input/output checks.
    pub fn initialize_from_json(&mut self, json: &str) -> Result<()> {
        self.initialize_from_json_bytes(json.as_bytes())
    }

    /// Initialize a job from raw qpdf job-JSON bytes.
    ///
    /// qpdf reads job JSON into a byte-preserving `std::string` before calling
    /// `QPDFJob::initializeFromJson` (`qpdf/test_driver.cc:2864-2876` and
    /// `libqpdf/QUtil.cc:1139-1170`). Keep this entry point byte-oriented so a
    /// syntactically valid JSON string containing a literal high-bit byte can
    /// reach the existing byte-oriented JSON parser and password fields.
    ///
    /// # Errors
    ///
    /// Has the same errors as [`Self::initialize_from_json`].
    pub fn initialize_from_json_bytes(&mut self, json: &[u8]) -> Result<()> {
        self.initialize_from_json_with_partial(json, false)
    }

    /// Initialize a job from a partial qpdf job-JSON document. Configuration
    /// checks that require command-line input/output values are deferred until
    /// [`QPDFJob::run`], matching qpdf's
    /// `QPDFJob::Config::jobJsonFile` call to `initializeFromJson(..., true)`
    /// (`libqpdf/QPDFJob_config.cc:774-784`).
    pub fn initialize_from_json_partial(&mut self, json: &str) -> Result<()> {
        self.initialize_from_json_partial_bytes(json.as_bytes())
    }

    /// Initialize a job from raw qpdf job-JSON bytes while deferring the
    /// command-boundary input/output checks until [`QPDFJob::run`].
    ///
    /// This is the byte-preserving counterpart of
    /// [`Self::initialize_from_json_partial`], used by qpdf's
    /// `--job-json-file` path after its binary file read.
    ///
    /// # Errors
    ///
    /// Returns an error if the byte input is not a qpdf job-JSON dictionary,
    /// if a nested job file cannot be read or parsed, or if a value violates
    /// qpdf's generated handler contract.
    pub fn initialize_from_json_partial_bytes(&mut self, json: &[u8]) -> Result<()> {
        self.initialize_from_json_with_partial(json, true)
    }

    fn configured_open_options(&self, password: Vec<u8>) -> PdfOpenOptions {
        PdfOpenOptions {
            // qpdf's `suppressRecovery` sets `attempt_recovery` false on every
            // QPDF created by the job (`QPDFJob.cc:651-659`).
            repair: !self.configuration.suppress_recovery,
            ignore_xref_streams: self.configuration.ignore_xref_streams,
            password,
            password_mode: self.configuration.password_mode,
            suppress_password_recovery: self.configuration.suppress_password_recovery,
            password_is_hex_key: self.configuration.password_is_hex_key,
            verbose: self.configuration.verbose,
            message_prefix: self.message_prefix.as_bytes().to_vec(),
            ..PdfOpenOptions::default()
        }
    }

    fn initialize_from_json_with_partial(&mut self, json: &[u8], partial: bool) -> Result<()> {
        // The qpdf C API sets this prefix before parsing JSON
        // (`libqpdf/qpdfjob-c.cc:79-87`), so initialization and run-time
        // configuration errors share the same observable source name.
        self.set_message_prefix("qpdfjob json");
        let value =
            crate::json::Json::parse(json).map_err(|error| Error::System(error.to_string()))?;
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
        let mut configuration = JobConfiguration {
            require_output: true,
            json_decode_level: crate::writer::DecodeLevel::Generalized,
            ..JobConfiguration::default()
        };
        self.dispatch_job_json_document(&mut configuration, &value, &mut BTreeSet::new())?;

        self.configuration = configuration;
        let input_name = self
            .configuration
            .input_file
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        self.set_input_name(input_name);
        self.warnings = false;
        if !partial {
            self.check_configuration()?;
        }
        Ok(())
    }

    fn dispatch_job_json_document(
        &mut self,
        configuration: &mut JobConfiguration,
        value: &crate::json::Json,
        active: &mut BTreeSet<PathBuf>,
    ) -> Result<()> {
        // qpdf validates every document before invoking its generated
        // handlers, including documents reached through jobJsonFile
        // (`QPDFJob_json.cc:611-625`, `QPDFJob_config.cc:774-784`).
        validate_job_json_schema(value)?;
        for (key, item) in job_json_members(value) {
            if key == b"jobJsonFile" {
                let mut members = std::collections::BTreeMap::new();
                members.insert(key.clone(), item);
                let path_bytes = job_json_string(&members, b"jobJsonFile")?
                    .expect("jobJsonFile member is present in the one-member dictionary");
                let path = path_from_qpdf_json_bytes(&path_bytes);
                let identity = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                // qpdf recursively re-enters `initializeFromJson` without a
                // cycle guard. Keep the Rust job boundary finite for a
                // hostile include graph; this input shape has no successful
                // qpdf output to preserve.
                // qpdf-deviation-start: reject recursive jobJsonFile includes instead of recursing until stack exhaustion
                if !active.insert(identity.clone()) {
                    return Err(Error::Usage(UsageError::new(format!(
                        "recursive jobJsonFile reference: {}",
                        path.display()
                    ))));
                }
                // qpdf-deviation-end
                let nested_result = (|| {
                    let nested = read_job_json_file(&path)?;
                    self.dispatch_job_json_document(configuration, &nested, active)
                })();
                active.remove(&identity);
                nested_result.map_err(|error| {
                    Error::System(format!(
                        "error with job-json file {}: {error}\nRun {} --job-json-help for information on the file format.",
                        path.display(),
                        self.message_prefix
                    ))
                })?;
            } else {
                let mut members = std::collections::BTreeMap::new();
                members.insert(key, item);
                self.apply_job_json_members(configuration, members)?;
            }
        }
        Ok(())
    }

    fn apply_job_json_members(
        &mut self,
        configuration: &mut JobConfiguration,
        members: std::collections::BTreeMap<Vec<u8>, crate::json::Json>,
    ) -> Result<()> {
        if let Some(input) = job_json_string(&members, b"inputFile")? {
            if configuration.input_file.is_some() || configuration.empty_input {
                return Err(Error::Usage(UsageError::new(
                    "input file has already been given",
                )));
            }
            if input.is_empty() {
                configuration.empty_input = true;
            } else {
                configuration.input_file = Some(path_from_qpdf_json_bytes(&input));
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
            if configuration.output_file.is_some() || configuration.replace_input {
                return Err(Error::Usage(UsageError::new(
                    "output file has already been given",
                )));
            }
            configuration.output_file = Some(path_from_qpdf_json_bytes(&output));
        }
        if let Some(copy_encryption) = job_json_string(&members, b"copyEncryption")? {
            configuration.copy_encryption = Some(path_from_qpdf_json_bytes(&copy_encryption));
            configuration.writer.clear_encryption_parameters();
        }
        if members.contains_key(b"encryptionFilePassword".as_slice()) {
            configuration.encryption_file_password =
                job_json_string(&members, b"encryptionFilePassword")?.unwrap_or_default();
        }
        if job_json_bare(&members, b"replaceInput")? {
            if configuration.output_file.is_some() || configuration.replace_input {
                return Err(Error::Usage(UsageError::new(
                    "replace-input can't be used since output file has already been given",
                )));
            }
            configuration.replace_input = true;
        }
        if members.contains_key(b"password".as_slice()) {
            configuration.password = job_json_string(&members, b"password")?.unwrap_or_default();
        }
        if let Some(password_file) = job_json_string(&members, b"passwordFile")? {
            let path = path_from_qpdf_json_bytes(&password_file);
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
        if job_json_bare(&members, b"ignoreXrefStreams")? {
            configuration.ignore_xref_streams = true;
        }
        if job_json_bare(&members, b"passwordIsHexKey")? {
            configuration.password_is_hex_key = true;
        }
        if job_json_bare(&members, b"suppressPasswordRecovery")? {
            configuration.suppress_password_recovery = true;
        }
        if job_json_bare(&members, b"suppressRecovery")? {
            configuration.suppress_recovery = true;
        }
        if let Some(value) = job_json_choice(
            &members,
            b"passwordMode",
            &["bytes", "hex-bytes", "unicode", "auto"],
            true,
        )? {
            configuration.password_mode = match value.as_str() {
                "bytes" => PasswordMode::Bytes,
                "hex-bytes" => PasswordMode::HexBytes,
                "unicode" => PasswordMode::Unicode,
                "auto" => PasswordMode::Auto,
                _ => unreachable!("passwordMode was validated above"), // cov:ignore: passwordMode comes only from the validated qpdf job schema choices
            };
        }
        if job_json_bare(&members, b"jsonInput")? {
            configuration.json_input = true;
        }

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
        if let Some(value) = job_json_string(&members, b"compressionLevel")? {
            configuration
                .writer
                .set_compression_level(parse_job_compression_level(&value)?);
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
            configuration.writer.clear_encryption_parameters();
            configuration.copy_encryption = None;
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
        if job_json_bare(&members, b"allowWeakCrypto")? {
            configuration.allow_weak_crypto = true;
        }
        if job_json_bare(&members, b"progress")? {
            configuration.progress = true;
        }
        if job_json_bare(&members, b"verbose")? {
            configuration.verbose = true;
        }
        if let Some(value) = job_json_yn(&members, b"keepFilesOpen")? {
            configuration.keep_files_open = Some(value);
        }
        if let Some(value) = job_json_string(&members, b"keepFilesOpenThreshold")? {
            configuration.keep_files_open_threshold = Some(parse_qpdf_collate_uint(&value)?);
        }
        if let Some(value) = job_json_string(&members, b"splitPages")? {
            configuration.split_pages = Some(parse_job_split_pages(&value)?);
        }
        if let Some(value) = job_json_string(&members, b"rotate")? {
            let value = String::from_utf8_lossy(&value);
            let rotation = RotateSpec::parse(&value)
                .map_err(|error| Error::Usage(UsageError::new(format!(".rotate: {error}"))))?;
            configuration
                .rotations
                .insert(job_json_rotate_range(value.as_bytes()), rotation);
        }
        if job_json_bare(&members, b"removeRestrictions")? {
            configuration.remove_restrictions = true;
        }
        if job_json_bare(&members, b"coalesceContents")? {
            configuration.coalesce_contents = true;
        }
        if job_json_bare(&members, b"externalizeInlineImages")? {
            configuration.externalize_inline_images = true;
        }
        if job_json_bare(&members, b"keepInlineImages")? {
            configuration.image_options.keep_inline_images = true;
        }
        if job_json_bare(&members, b"optimizeImages")? {
            configuration.optimize_images = true;
        }
        if let Some(value) = job_json_string(&members, b"iiMinBytes")? {
            configuration.image_options.inline_min_bytes =
                parse_qpdf_collate_uint(&value)? as usize;
        }
        if let Some(value) = job_json_string(&members, b"oiMinArea")? {
            configuration.image_options.min_area = parse_qpdf_collate_uint(&value)? as u32;
        }
        if let Some(value) = job_json_string(&members, b"oiMinHeight")? {
            configuration.image_options.min_height = parse_qpdf_collate_uint(&value)? as u32;
        }
        if let Some(value) = job_json_string(&members, b"oiMinWidth")? {
            configuration.image_options.min_width = parse_qpdf_collate_uint(&value)? as u32;
        }
        // The generated qpdf handler accepts only these three strings
        // (`libqpdf/qpdf/auto_job_json_init.hh:377-379`); unlike the bare
        // transformation toggles, this setting carries a mode.
        if let Some(value) = job_json_choice(
            &members,
            b"flattenAnnotations",
            &["all", "print", "screen"],
            true,
        )? {
            configuration.flatten_annotations = Some(match value.as_str() {
                "all" => FlattenAnnotationsMode::All,
                "print" => FlattenAnnotationsMode::Print,
                "screen" => FlattenAnnotationsMode::Screen,
                _ => unreachable!("flattenAnnotations was validated above"), // cov:ignore: flattenAnnotations comes only from the validated qpdf job schema choices
            });
        }
        if job_json_bare(&members, b"flattenRotation")? {
            configuration.flatten_rotation = true;
        }
        if job_json_bare(&members, b"generateAppearances")? {
            configuration.generate_appearances = true;
        }
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
            let (version, extension) = parse_job_version(&value, ".minVersion")?;
            configuration
                .writer
                .set_minimum_pdf_version(version, extension);
        }
        if let Some(value) = job_json_string(&members, b"forceVersion")? {
            let (version, extension) = parse_job_version(&value, ".forceVersion")?;
            configuration.writer.force_pdf_version(version, extension);
        }
        if let Some(value) = job_json_string(&members, b"linearizePass1")? {
            configuration.linearize_pass1 = Some(path_from_qpdf_json_bytes(&value));
        }
        if job_json_bare(&members, b"linearize")? {
            configuration.linearize = true;
        }
        if let Some(value) = job_json_string(&members, b"updateFromJson")? {
            configuration.update_from_json = Some(path_from_qpdf_json_bytes(&value));
        }
        if let Some(value) = job_json_string(&members, b"collate")? {
            let value = String::from_utf8_lossy(&value);
            configuration
                .collate
                .get_or_insert_with(Vec::new)
                .extend(Self::parse_collate(&value)?);
        }

        if let Some(value) = job_json_choice(&members, b"json", &["1", "2", "latest"], false)? {
            configuration.json_version = Some(parse_json_version(&value));
            configuration.require_output = false;
        }
        if let Some(value) = job_json_choice(&members, b"jsonOutput", &["2", "latest"], false)? {
            configuration.json_output = true;
            configuration.json_version = Some(parse_json_version(&value));
            if !configuration.json_stream_data_set {
                configuration.json_stream_data = JsonStreamData::Inline;
            }
            if !configuration.json_decode_level_set {
                configuration.json_decode_level = crate::writer::DecodeLevel::None;
            }
            configuration.require_output = false;
            configuration.json_keys.push(JsonKey::Qpdf);
        }
        if let Some(value) = job_json_string(&members, b"jsonStreamPrefix")? {
            configuration.json_stream_prefix = Some(value);
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
            configuration.json_stream_data_set = true;
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
        if job_json_bare(&members, b"testJsonSchema")? {
            configuration.test_json_schema = true;
        }
        if job_json_bare(&members, b"showEncryptionKey")? {
            configuration.show_encryption_key = true;
        }
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
        if job_json_bare(&members, b"showNpages")? {
            configuration.show_npages = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"showPages")? {
            configuration.show_pages = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"showEncryption")? {
            configuration.show_encryption = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"isEncrypted")? {
            configuration.is_encrypted = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"requiresPassword")? {
            configuration.requires_password = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"checkLinearization")? {
            configuration.check_linearization = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"showXref")? {
            configuration.show_xref = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"showLinearization")? {
            configuration.show_linearization = true;
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"filteredStreamData")? {
            configuration.show_filtered_stream_data = true;
        }
        if job_json_bare(&members, b"rawStreamData")? {
            configuration.show_raw_stream_data = true;
        }
        if let Some(value) = job_json_string(&members, b"showObject")? {
            configuration.show_object = Some(parse_job_object_selector(&value)?);
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"listAttachments")? {
            configuration.list_attachments = true;
            configuration.require_output = false;
        }
        if let Some(value) = job_json_string(&members, b"showAttachment")? {
            configuration.show_attachment = Some(value);
            configuration.require_output = false;
        }
        if job_json_bare(&members, b"withImages")? {
            configuration.show_page_images = true;
        }
        if job_json_bare(&members, b"reportMemoryUsage")? {
            configuration.report_memory_usage = true;
        }

        if let Some(value) = members.get(b"encrypt".as_slice()) {
            // qpdf's `EncConfig::endEncrypt` clears copy-encryption and
            // decrypt state (`QPDFJob_config.cc:1158-1167`). The generated
            // handler visits `copyEncryption` before `encrypt`, so preserve
            // that precedence in the configuration snapshot.
            configuration.copy_encryption = None;
            configuration
                .writer
                .set_encryption_parameters(parse_job_encrypt(
                    value,
                    configuration.allow_weak_crypto,
                )?); // cov:ignore: llvm-cov attributes this successful encryption parse continuation to its opening expressions
        }

        if let Some(value) = members.get(b"pages".as_slice()) {
            if !configuration.page_specs.is_empty() {
                return Err(Error::Usage(UsageError::new(
                    "--pages may only be specified one time",
                )));
            }
            let items = job_json_items(value);
            if items.is_empty() {
                return Err(Error::Usage(UsageError::new(
                    "--pages: no page specifications given",
                )));
            }
            for (index, item) in items.into_iter().enumerate() {
                let item_members = job_json_members(&item);
                let file = job_json_string(&item_members, b"file")?.ok_or_else(|| {
                    Error::Usage(UsageError::new("file is required in page specification"))
                })?;
                let range = job_json_range(
                    item_members.get(b"range".as_slice()),
                    &format!(".pages[{index}].range"),
                )?; // cov:ignore: llvm-cov attributes this successful page range conversion to the opening call lines
                configuration.page_specs.push(JobPageConfig {
                    path: path_from_qpdf_json_bytes(&file),
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
                        path: path_from_qpdf_json_bytes(&file),
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
        options: PdfOpenOptions,
    ) -> Result<JobDocument>
    where
        R: Read + Seek + 'static,
    {
        let input_name = input_name.into();
        self.open_document_with_description(source, input_name.as_bytes(), options)
    }

    /// Open a job-owned document with a byte-preserving input description.
    pub fn open_document_with_description<R>(
        &mut self,
        source: R,
        input_name: impl AsRef<[u8]>,
        mut options: PdfOpenOptions,
    ) -> Result<JobDocument>
    where
        R: Read + Seek + 'static,
    {
        let input_name = input_name.as_ref().to_vec();
        self.set_input_name_bytes(&input_name);
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        options.verbose |= self.configuration.verbose;
        options.message_prefix = self.message_prefix.as_bytes().to_vec();
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
        // qpdf's `Config::emptyInput` uses the empty string as the page-spec
        // source-map key while `QPDF::emptyPDF` names the diagnostic source
        // "empty PDF" (`libqpdf/QPDFJob_config.cc:27-38`;
        // `libqpdf/QPDF.cc:290-293`). Keep those two qpdf values distinct so
        // page-source ordering can use the map key without changing output
        // text.
        self.configuration.empty_input = true;
        // qpdf's `setQPDFOptions` (`QPDFJob.cc:651-665`) runs unconditionally
        // right after `QPDF` construction, before dispatching to empty,
        // JSON-input, or file-based creation (`QPDFJob.cc:1701-1710`), so
        // `noWarn` suppresses warnings for an empty document exactly like the
        // other two creation kinds. `description` mirrors qpdf's
        // `QPDF::emptyPDF` calling `processMemoryFile("empty PDF", ...)`
        // (`libqpdf/QPDF.cc:290-293`), which becomes the description qpdf
        // shows in warnings involving this document (e.g. an
        // `--update-from-json` validation failure against the empty
        // primary: `WARNING: empty PDF ( from <path>): ...`, live-probed
        // against qpdf 11.9.0).
        let mut options = self.configured_open_options(Vec::new());
        options.logger = Some(self.logger.clone());
        options.suppress_warnings = self.suppress_warnings;
        options.description = b"empty PDF".to_vec();
        let mut pdf = crate::engine::open_empty_with_options_erased(options)?;
        self.input_name.clear();
        self.input_name_bytes.clear();
        pdf.root_handle()?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Return the qpdf page-spec source-map key for one opened source.
    ///
    /// Ordinary file sources use their raw input description as both the map
    /// key and diagnostic filename. The empty primary is the qpdf exception:
    /// its map key is empty while its `QPDF` diagnostic filename is `empty
    /// PDF`.
    pub(crate) fn page_spec_source_sort_key(
        &self,
        source_index: usize,
        source_description: &[u8],
    ) -> Vec<u8> {
        if source_index == 0 && self.configuration.empty_input {
            Vec::new()
        } else {
            source_description.to_vec()
        }
    }

    /// Create a complete JSON-input document through the same job document
    /// boundary as file and empty input.
    pub fn create_from_json_document<S>(
        &mut self,
        source: S,
        input_name: impl AsRef<[u8]>,
    ) -> Result<JobDocument>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.as_ref().to_vec();
        self.set_input_name_bytes(&input_name);
        // See `create_empty_document`: qpdf applies `noWarn` to every
        // creation kind uniformly, including JSON-input.
        let mut options = self.configured_open_options(Vec::new());
        options.logger = Some(self.logger.clone());
        options.suppress_warnings = self.suppress_warnings;
        let pdf = crate::json::create_from_json_erased(source, input_name, options)?;
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
            return match self.create_from_json_document(file, path_description_bytes(&input)) {
                Ok(pdf) => Ok(Some(pdf)),
                Err(error) => {
                    self.report_job_error(&error)?;
                    Ok(None)
                }
            };
        }
        match self.open_document_with_description(
            BufReader::new(file),
            path_description_bytes(&input),
            self.configured_open_options(self.configuration.password.clone()),
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
        if let Some(path) = self.configuration.copy_encryption.clone() {
            match self.copy_encryption_source(&path) {
                Ok(source) => writer_configuration.copy_encryption_parameters(source),
                Err(error) => {
                    self.report_job_error(&error)?;
                    return Ok(JobExitCode::Error);
                }
            }
        }
        let auto_password_warnings = match writer_configuration
            .normalize_encryption_passwords(self.configuration.password_mode)
        {
            Ok(count) => count,
            Err(error) => {
                self.report_job_error(&error)?;
                return Ok(JobExitCode::Error);
            }
        };
        for _ in 0..auto_password_warnings {
            self.logger.error(format!(
                "{}: WARNING: supplied password looks like a Unicode password with characters not allowed in passwords for 40-bit and 128-bit encryption; most readers will not be able to open this file with the supplied password. (Use --password-mode=bytes to suppress this warning and use the password anyway.)\n",
                self.message_prefix
            ))?;
        }
        writer_configuration.set_linearization(self.configuration.linearize);
        if let Some(path) = self.configuration.linearize_pass1.as_deref() {
            writer_configuration.set_linearization_pass1_filename(path.to_path_buf());
        }
        let progress_requested = self.configuration.progress;
        let splitting = self.configuration.split_pages.is_some_and(|size| size != 0);
        let write_result: Result<()> =
            if let Some(split_pages) = self.configuration.split_pages.filter(|size| *size != 0) {
                // Keep the signed qpdf value until the split implementation has
                // reached the same page-boundary conversion as
                // `QIntC::to_size(m->split_pages)` (`libqpdf/QPDFJob.cc:2970`).
                let mut split_options = SplitPageOptions::new(1, output.clone())
                    .with_qpdf_chunk_size(split_pages)
                    .with_writer_configuration(writer_configuration.clone())
                    .with_verbose(self.configuration.verbose)
                    .with_remove_unreferenced_resources(
                        self.configuration.remove_unreferenced_resources,
                    );
                if let Some(input) = self.configuration.input_file.clone() {
                    split_options = split_options.with_input_path(input);
                }
                // qpdf reports each chunk from inside the split loop itself
                // (`libqpdf/QPDFJob.cc:3019-3021`), so this call's own verbose
                // option -- not the shared report below -- is what a split write
                // relies on; the shared report is for the non-split branch only,
                // and stays correct even if a later chunk fails after earlier
                // chunks already reported success.
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
                if self.configuration.verbose && output != Path::new("-") && !splitting {
                    let message =
                        format!("{}: wrote file {}\n", self.message_prefix, output.display());
                    self.logger.info(message)?;
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
        if self.configuration.is_encrypted || self.configuration.requires_password {
            return self.run_encryption_status();
        }
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
        if configuration.report_memory_usage && status != JobExitCode::Error {
            self.report_memory_usage()?;
        }
        if configuration.replace_input {
            if status == JobExitCode::Error {
                self.remove_replace_input_temp();
            } else {
                self.finish_replace_input()?;
            }
        }
        Ok(status)
    }

    fn report_memory_usage(&self) -> Result<()> {
        self.logger.warn(format!(
            "qpdf-max-memory-usage {}\n",
            crate::memory_usage::max_memory_usage()
        ))
    }

    fn run_encryption_status(&mut self) -> Result<JobExitCode> {
        self.check_configuration()?;
        // qpdf's `createQPDF` still creates an empty document for `--empty`
        // before the encryption-status early return (`QPDFJob.cc:429-456,
        // 1699-1708`). An empty document is necessarily unencrypted, so both
        // `isEncrypted` and `requiresPassword` return EXIT_IS_NOT_ENCRYPTED
        // (2) without attempting to open an input file.
        if self.configuration.empty_input {
            return Ok(JobExitCode::Error);
        }
        let Some(input) = self.configuration.input_file.clone() else {
            // cov:ignore-start: with `empty_input` handled above,
            // `check_configuration` rejects an encryption-status query that
            // has no configured input before this defensive invariant guard
            return Err(UsageError::new("an input file name is required").into());
            // cov:ignore-end
        };
        let file = match File::open(&input) {
            Ok(file) => file,
            Err(error) => {
                let error = Error::file_io("open", input.clone(), error);
                self.report_job_error(&error)?;
                return Ok(JobExitCode::Error);
            }
        };
        let options = self.configured_open_options(self.configuration.password.clone());
        let pdf = match self.open_for_encryption_inspection_with_description(
            BufReader::new(file),
            path_description_bytes(&input),
            options,
        ) {
            Ok(pdf) => pdf,
            Err(error) => {
                self.report_job_error(&error)?;
                return Ok(JobExitCode::Error);
            }
        };
        let encrypted = pdf.is_encrypted();
        if self.configuration.is_encrypted {
            return Ok(if encrypted {
                JobExitCode::Success
            } else {
                JobExitCode::Error
            });
        }

        // qpdf's `requiresPassword` uses exit 3 when authentication succeeds,
        // exit 0 when an encrypted document still needs another password, and
        // exit 2 for a plaintext document (`QPDFJob::getExitCode`,
        // `QPDFJob.cc:535-557`). `encryption_file_key` also covers the raw
        // `passwordIsHexKey` path, where user/owner match flags stay false.
        if !encrypted {
            return Ok(JobExitCode::Error);
        }
        Ok(if pdf.encryption_file_key().is_some() {
            JobExitCode::Warning
        } else {
            JobExitCode::Success
        })
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
                path_description_bytes(update_path),
            )?; // cov:ignore: llvm-cov attributes this successful update continuation to its opening call lines
        }

        if configuration.page_specs.is_empty() {
            self.apply_configured_rotations(&mut primary, configuration)?;
            self.run_document_stages(&mut primary, configuration)
        } else {
            let mut page_sources = vec![primary];
            // qpdf keys its opened-source cache by filename alone
            // (`page_spec_qpdfs.count(page_spec.filename) == 0`,
            // `QPDFJob.cc:2389-2427`), reusing the existing QPDF for a
            // repeated literal path rather than reopening it. `source_paths`
            // mirrors that cache for the secondary sources opened here (the
            // primary's own aliases are already handled by the check above).
            let mut source_paths: Vec<PathBuf> = Vec::new();
            let mut source_passwords: Vec<Vec<u8>> = Vec::new();
            let mut specs = Vec::with_capacity(configuration.page_specs.len());
            for page in &configuration.page_specs {
                let source_index = if page.path == Path::new(".")
                    || self.configuration.input_file.as_deref() == Some(page.path.as_path())
                {
                    0
                } else if let Some(index) = source_paths.iter().position(|path| *path == page.path)
                {
                    index + 1
                } else {
                    source_paths.push(page.path.clone());
                    source_passwords.push(page.password.clone());
                    source_paths.len()
                };
                specs.push(PageSpecInput::new(source_index, page.range.clone()));
            }
            let keep_files_open = self.keep_files_open_for_page_specs(&specs);
            self.report_page_spec_selection(&specs)?;
            for (path, password) in source_paths.iter().zip(source_passwords.iter()) {
                self.report_page_source_processing(path_description_bytes(path))?;
                let source = self.open_job_source(path, password)?;
                if !keep_files_open {
                    // qpdf calls ClosedFileInputSource::stayOpen(false)
                    // immediately after processInputSource, before opening
                    // the next distinct page source (`QPDFJob.cc:2414-2432`).
                    source.set_input_source_stay_open(false);
                }
                page_sources.push(source);
            }
            let page_output = self.handle_page_specs(
                &mut page_sources,
                &specs,
                configuration.collate.as_deref(),
                configuration.remove_unreferenced_resources,
                configuration.writer.preserves_unreferenced_objects(),
            )?; // cov:ignore: llvm-cov attributes this successful page merge continuation to its opening call lines
            match page_output {
                PageSpecJobOutput::InPlace {
                    pdf,
                    result,
                    prune_mode,
                } => {
                    QPDFJob::complete_in_place_page_selection(pdf, &result, prune_mode)?;
                    self.apply_configured_rotations(pdf, configuration)?;
                    self.run_document_stages(pdf, configuration)
                }
                PageSpecJobOutput::Merged(mut merged) => {
                    self.apply_configured_rotations(&mut merged, configuration)?;
                    let status = self.run_document_stages(&mut merged, configuration);
                    // `merged` may retain provider-backed objects from
                    // page_sources; both are deliberately alive until every
                    // output byte is written.
                    drop(merged);
                    status
                }
            }
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
        if page_refs.is_empty() {
            // qpdf's handleRotations resolves each range against the real page
            // count and then filters `0 <= pageno < npages` before touching
            // `pages`, so an empty document rotates nothing without erroring
            // (confirmed live: `--collate=0 --rotate=90` exits 0). A resolved
            // range's own out-of-bounds check requires page_count >= 1, so
            // this document-empty case is handled up front instead.
            return Ok(());
        }
        let page_count = u32::try_from(page_refs.len())
            .map_err(|_| Error::Unsupported("page count exceeds qpdf's range".to_owned()))?;
        for rotation in configuration.rotations.values() {
            let selected = rotation.range.resolve(page_count)?;
            let selected_refs = selected
                .into_iter()
                .map(|page| {
                    // cov:ignore-start: PageRange::resolve guarantees each
                    // selected page is a 1-based member of page_refs, so these
                    // defensive conversion/index failures are unreachable.
                    let index = usize::try_from(page - 1).map_err(|_| {
                        Error::Unsupported("rotation page index underflow".to_owned())
                    })?;
                    page_refs.get(index).copied().ok_or_else(|| {
                        Error::Unsupported("rotation page index out of range".to_owned())
                    })
                    // cov:ignore-end
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

        // qpdf's `handleTransformations` externalizes inline images before
        // optimizing reachable Image XObjects and before appearance
        // generation (`libqpdf/QPDFJob.cc:2151-2174`). The existing image
        // phase owns both the inline-image and deferred DCT provider routes;
        // make an explicit externalization request plus optimization one
        // pass so the inline content is not traversed twice. An explicit
        // request wins over `keepInlineImages`, matching qpdf's condition.
        if configuration.optimize_images {
            let mut image_options = configuration.image_options;
            if configuration.externalize_inline_images {
                image_options.keep_inline_images = false;
            }
            optimize_images(
                pdf,
                &self.logger,
                &self.message_prefix,
                configuration.verbose,
                image_options,
            )?; // cov:ignore: llvm-cov attributes this successful multiline image phase call to its opening expressions
        } else if configuration.externalize_inline_images {
            let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
            for page_ref in page_refs {
                PageObjectHelper::new(page_ref, pdf).externalize_inline_images(
                    configuration.image_options.inline_min_bytes,
                    false,
                )?; // cov:ignore: llvm-cov attributes this successful multiline image externalization call to its opening expressions
            }
        }

        // qpdf's `handleTransformations` generates form appearances after
        // removing restrictions and before content coalescing or rotation
        // flattening (`QPDFJob.cc:2177-2180`). The AcroForm helper owns the
        // `/NeedAppearances` gate, widget traversal, and marker clearing.
        if configuration.generate_appearances {
            let mut acroform = AcroFormDocumentHelper::new(pdf)?;
            acroform.generate_appearances_if_needed()?;
        }

        // qpdf's `handleTransformations` flattens annotations after appearance
        // generation and before content coalescing or rotation flattening
        // (`QPDFJob.cc:2177-2194`). Keep the mode-to-mask mapping in the job
        // boundary so JSON and CLI callers reach the same page helper route.
        if let Some(mode) = configuration.flatten_annotations {
            let (required_flags, forbidden_flags) = mode.qpdf_flags();
            PageDocumentHelper::new(pdf).flatten_annotations(required_flags, forbidden_flags)?;
        }

        // qpdf's `handleTransformations` coalesces every page after the
        // earlier document transformations and before later page-label/output
        // completion (`QPDFJob.cc:2185-2188`). Keep the existing lazy,
        // provider-backed PageObjectHelper route; do not decode page contents
        // into a new eager buffer here.
        if configuration.coalesce_contents {
            let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
            for page_ref in page_refs {
                PageObjectHelper::new(page_ref, pdf).coalesce_content_streams()?;
            }
        }

        // qpdf's `handleTransformations` flattens rotation after coalescing
        // content streams and before page-label/output completion
        // (`QPDFJob.cc:2190-2194`). The existing job rotation module owns the
        // page-level matrix, box, and annotation semantics.
        if configuration.flatten_rotation {
            let page_refs = PageDocumentHelper::new(pdf).get_all_pages()?;
            flatten_rotation_on_pages(pdf, &page_refs)?;
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
            let source = self.open_job_source(&copy.path, &copy.password)?;
            attachment_sources.push((
                source,
                AttachmentCopyOptions {
                    path: copy.path.clone(),
                    prefix: copy.prefix.clone(),
                    verbose: configuration.verbose,
                },
            ));
        }
        // qpdf copies from every configured donor in one pass and reports the
        // conflicting keys once after the last donor (`QPDFJob.cc:2089-2135`).
        let mut copy_sources = attachment_sources
            .iter_mut()
            .map(|(source, options)| AttachmentCopySource {
                source,
                options: options.clone(),
            })
            .collect::<Vec<_>>();
        self.copy_attachments_many(pdf, &mut copy_sources)?;
        drop(copy_sources);

        if configuration.check
            || configuration.show_npages
            || configuration.show_pages
            || configuration.show_encryption
            || configuration.check_linearization
            || configuration.show_xref
            || configuration.show_linearization
            || configuration.show_object.is_some()
            || configuration.list_attachments
            || configuration.show_attachment.is_some()
        {
            let status = self.run_configured_inspection(pdf, configuration)?;
            drop(attachment_sources);
            drop(overlay_specs);
            return Ok(status);
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

    fn run_configured_inspection<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<JobExitCode>
    where
        R: Read + Seek + 'static,
    {
        // qpdf's doInspection executes selected branches independently in this
        // order and emits one warning/status completion after the branches
        // (`libqpdf/QPDFJob.cc:1646-1693`). Keep report generation separate
        // from completion so combined job-JSON inspection flags do not emit
        // duplicate summaries.
        pdf.set_logger(self.logger.clone());
        if configuration.check {
            if let Err(error) = self.run_check_report(pdf) {
                return self.map_check_result(Err(error));
            }
        }
        if configuration.show_npages {
            self.show_npages_report(pdf)?;
        }
        if configuration.show_encryption {
            self.show_encryption(pdf, configuration.password_is_hex_key)?;
        }
        if configuration.check_linearization {
            self.check_linearization_report(pdf)?;
        }
        if configuration.show_linearization {
            self.show_linearization_report(pdf)?;
        }
        if configuration.show_xref {
            self.show_xref_report(pdf)?;
        }
        if let Some(selector) = configuration.show_object {
            match selector {
                JobObjectSelector::Trailer => {
                    let object = pdf.trailer();
                    // cov:ignore-start: malformed object-report errors are covered by the public inspection route; only this propagated edge is excluded
                    self.show_object_report(
                        pdf,
                        &object,
                        configuration.show_raw_stream_data,
                        configuration.show_filtered_stream_data,
                    )?;
                    // cov:ignore-end
                }
                JobObjectSelector::Object(object_ref) => {
                    let object = pdf.get_object_handle(object_ref);
                    // cov:ignore-start: malformed object-report errors are covered by the public inspection route; only this propagated edge is excluded
                    self.show_object_report(
                        pdf,
                        &object,
                        configuration.show_raw_stream_data,
                        configuration.show_filtered_stream_data,
                    )?;
                    // cov:ignore-end
                }
                JobObjectSelector::Null => self.logger.info(b"null\n")?,
                JobObjectSelector::NoObject => {}
            }
        }
        if configuration.show_pages {
            self.show_pages_report_with_images(pdf, configuration.show_page_images)?;
        }
        if configuration.list_attachments {
            self.list_attachments_report(pdf, configuration.verbose)?;
        }
        if let Some(key) = configuration
            .show_attachment
            .as_deref()
            .filter(|key| !key.is_empty())
        {
            self.show_attachment_report(pdf, key)?;
        }
        self.record_document_warnings(pdf);
        self.complete(false)
    }

    /// Run qpdf's standalone `--show-linearization` inspection on an already
    /// opened document and complete the shared warning/status boundary.
    ///
    /// qpdf calls `showLinearizationData` on the same `QPDF` that
    /// `createQPDF` configured and later passes through its inspection
    /// completion (`libqpdf/QPDFJob.cc:650-665,1646-1674`). Keeping the
    /// document supplied by the caller avoids a second default-logger open and
    /// preserves the configured input description.
    pub fn show_linearization<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<JobExitCode> {
        self.show_linearization_report(pdf)?;
        self.record_document_warnings(pdf);
        self.complete(false)
    }

    fn show_linearization_report<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<()> {
        // QPDFJob installs its logger on the one document before inspection
        // (`libqpdf/QPDFJob.cc:650-665,1646-1674`). Keep this report safe for
        // callers that supply an already-opened Pdf as well as for job-owned
        // documents, and preserve an explicit document-level suppression flag.
        let suppress_warnings = pdf.suppress_warnings() || self.suppress_warnings;
        pdf.set_logger(self.logger.clone());
        pdf.set_suppress_warnings(suppress_warnings);
        let input_name = self.input_name_bytes().to_owned();
        let output = show_linearization_pdf_with_warnings(pdf, &input_name)
            .map_err(map_show_linearization_error)?;
        for warning in output.warnings {
            self.record_warnings();
            // cov:ignore-start: warning-sink propagation is an injected logger edge; the data warning and status branches are covered separately
            if !suppress_warnings {
                let mut line = b"WARNING: ".to_vec();
                line.extend_from_slice(&input_name);
                line.extend_from_slice(b": ");
                line.extend_from_slice(&warning);
                line.push(b'\n');
                self.logger.warn(line)?;
            }
            // cov:ignore-end
        }
        self.logger.info(output.dump)
    }

    fn open_job_source(&mut self, path: &Path, password: &[u8]) -> Result<JobDocument> {
        let mut options = self.configured_open_options(password.to_vec());
        options.logger = Some(self.logger.clone());
        options.description = path_description_bytes(path);
        // `open_file_with_options` installs the qpdf-shaped reopenable source;
        // keep the job's warning policy on this secondary document exactly as
        // `open_document` does for the primary.
        options.suppress_warnings |= self.suppress_warnings;
        let mut pdf = Pdf::<Box<dyn ReadSeek>>::open_file_with_options(path, options)?;
        pdf.root_handle()?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    fn copy_encryption_source(&mut self, path: &Path) -> Result<crate::CopyEncryptionSource> {
        let password = self.configuration.encryption_file_password.clone();
        let mut donor = self.open_job_source(path, &password)?;
        donor.writer_copy_encryption_source()?.ok_or_else(|| {
            Error::Usage(UsageError::new(format!(
                "copyEncryption donor {} is not encrypted",
                path.display()
            )))
        })
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
                .map_err(Error::from);
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
        .map_err(Error::from)
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
            .map(|path| path_with_suffix(path, ".~qpdf-temp#"))
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
        let mut backup = path_with_suffix(input, ".~qpdf-orig");
        if !self.warnings {
            backup = path_with_suffix(&backup, "#");
        }
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
    pub fn check_configuration(&self) -> Result<()> {
        if self.configuration.input_file.is_none()
            && !self.configuration.empty_input
            && (self.configuration.require_output
                || self.configuration.check
                || self.configuration.show_npages
                || self.configuration.show_pages
                || self.configuration.check_linearization
                || self.configuration.show_xref
                || self.configuration.show_linearization
                || self.configuration.show_object.is_some()
                || self.configuration.list_attachments
                || self.configuration.show_attachment.is_some()
                || self.configuration.show_encryption
                || self.configuration.is_encrypted
                || self.configuration.requires_password
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
            if self.configuration.split_pages.is_some_and(|size| size != 0) {
                return Err(
                    UsageError::new("--split-pages may not be used with --replace-input").into(),
                );
            }
            if self.configuration.json_version.is_some() {
                return Err(UsageError::new("--json may not be used with --replace-input").into());
            }
        }
        if self.configuration.require_output
            && self.configuration.output_file.is_none()
            && !self.configuration.replace_input
        {
            return Err(UsageError::new(
                "an output file name is required; use - for standard output",
            )
            .into());
        }
        if (self.configuration.check
            || self.configuration.show_npages
            || self.configuration.show_pages
            || self.configuration.check_linearization
            || self.configuration.show_xref
            || self.configuration.show_linearization
            || self.configuration.show_object.is_some()
            || self.configuration.list_attachments
            || self.configuration.show_attachment.is_some()
            || self.configuration.show_encryption
            || self.configuration.is_encrypted
            || self.configuration.requires_password)
            // qpdf's JSON output defaults to stdout before this conflict
            // check (`QPDFJob.cc:582-595`), so it is an output destination
            // even when no explicit outputFile was supplied.
            && (self.configuration.output_file.is_some()
                || self.configuration.replace_input
                || self.configuration.json_version.is_some())
        {
            return Err(UsageError::new("no output file may be given for this option").into());
        }
        if self.configuration.is_encrypted && self.configuration.requires_password {
            return Err(UsageError::new(
                "--requires-password and --is-encrypted may not be given together",
            )
            .into());
        }
        if self.configuration.output_file.as_deref() == Some(Path::new("-")) {
            if self.configuration.split_pages.is_some_and(|size| size != 0) {
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
            // qpdf only runs this check when `!m->split_pages`
            // (`libqpdf/QPDFJob.cc:627`): a splitting write never truncates
            // the original input in place, so aliasing input and output is
            // not destructive when splitting.
            if !self.configuration.replace_input
                && !self.configuration.split_pages.is_some_and(|size| size != 0)
                && crate::qutil::same_file(input, output)
            {
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

    /// Report one qpdf job error through the job's error logger.
    ///
    /// This is the Rust consumer boundary corresponding to the exception
    /// catch in `qpdfjob-c.cc:32-40`: the prefix, separator, message, and
    /// newline remain four writes so custom pipelines observe the same
    /// boundaries as qpdf's stream insertion sequence. The ordinary
    /// [`QPDFJob::run`] contract still returns usage errors to its caller;
    /// callers that model qpdf's C wrapper can report the error here and map
    /// it to the wrapper's error status.
    pub fn report_job_error(&self, error: &Error) -> Result<()> {
        // qpdf's C wrapper streams the prefix, separator, message, and final
        // newline separately (`qpdfjob-c.cc:32-39`). Keeping those writes
        // separate preserves custom-pipeline boundaries as well as bytes.
        let pipeline = self.logger.get_error()?;
        pipeline
            .write(self.message_prefix.as_bytes())
            .map_err(Error::from)?;
        pipeline.write(b": ").map_err(Error::from)?;
        pipeline
            .write(&Self::job_error_message(error))
            .map_err(Error::from)?;
        pipeline.write(b"\n").map_err(Error::from)
    }

    fn job_error_message(error: &Error) -> Vec<u8> {
        if let Some(message) = error.raw_message() {
            return message.to_vec();
        }
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
                format!("{operation} {}: {source}", path.display()).into_bytes()
            }
            _ => error.to_string().into_bytes(),
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
        input_name: impl AsRef<[u8]>,
    ) -> Result<Pdf<Cursor<Vec<u8>>>>
    where
        S: Read + Seek + 'static,
    {
        let input_name = input_name.as_ref().to_vec();
        self.set_input_name_bytes(&input_name);
        let pdf = Pdf::create_from_json_with_options(
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
        options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.into();
        self.open_with_description(source, input_name.as_bytes(), options)
    }

    /// Open a document with a qpdf input description that may contain raw
    /// Unix argv/path bytes.
    pub fn open_with_description<R>(
        &mut self,
        source: R,
        input_name: impl AsRef<[u8]>,
        mut options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.as_ref().to_vec();
        self.set_input_name_bytes(&input_name);
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        options.verbose |= self.configuration.verbose;
        options.message_prefix = self.message_prefix.as_bytes().to_vec();
        // qpdf's `setQPDFOptions` applies `noWarn` to every ordinary QPDF
        // immediately after construction and before `processFile`
        // (`QPDFJob.cc:650-666,1695-1711`). Preserve an explicit caller
        // suppression request while adding the job-wide policy.
        options.suppress_warnings |= self.suppress_warnings;
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
        options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.into();
        self.open_for_encryption_inspection_with_description(source, input_name.as_bytes(), options)
    }

    /// Open for encryption inspection with a byte-preserving input
    /// description.
    pub fn open_for_encryption_inspection_with_description<R>(
        &mut self,
        source: R,
        input_name: impl AsRef<[u8]>,
        mut options: PdfOpenOptions,
    ) -> Result<Pdf<R>>
    where
        R: Read + Seek,
    {
        let input_name = input_name.as_ref().to_vec();
        self.set_input_name_bytes(&input_name);
        options.logger = Some(self.logger.clone());
        options.description = input_name;
        // qpdf's createQPDF reaches doProcess for every command, including
        // --show-encryption, so the job's verbose policy and message prefix
        // apply to this open exactly like the ordinary path
        // (`QPDFJob.cc:1717-1791`).
        options.verbose |= self.configuration.verbose;
        options.message_prefix = self.message_prefix.as_bytes().to_vec();
        // The encryption-inspection creation path is still a qpdf input
        // QPDF, so `noWarn` must be applied before authentication/parsing just
        // like the ordinary `doProcessOnce` path.
        options.suppress_warnings |= self.suppress_warnings;
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
        input_name: impl AsRef<[u8]>,
    ) -> Result<()>
    where
        R: Read + Seek,
        S: Read + Seek + 'static,
    {
        let input_name = input_name.as_ref().to_vec();
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
        super::json::write_json_with_version_with_logger(
            pdf,
            version,
            test_json_schema,
            json_output,
            show_encryption_key,
            options,
            output,
            &self.logger,
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

impl QPDFJobConfig<'_> {
    /// Set the primary input filename, rejecting duplicate input selection.
    pub fn input_file(&mut self, input_file: impl Into<PathBuf>) -> Result<&mut Self> {
        self.job.set_input_file(input_file)?;
        Ok(self)
    }

    /// Set the output filename, rejecting duplicate output selection.
    pub fn output_file(&mut self, output_file: impl Into<PathBuf>) -> Result<&mut Self> {
        self.job.set_output_file(output_file)?;
        Ok(self)
    }

    /// Request qpdf QDF output.
    pub fn qdf(&mut self) -> &mut Self {
        self.job.configuration.writer.set_qdf_mode(true);
        self
    }

    /// Request qpdf deterministic trailer identifiers.
    pub fn deterministic_id(&mut self) -> &mut Self {
        self.job.configuration.writer.set_deterministic_id(true);
        self
    }

    /// Select qpdf's object-stream policy.
    pub fn object_streams(&mut self, mode: &str) -> Result<&mut Self> {
        self.job
            .configuration
            .writer
            .set_object_stream_mode(parse_object_stream_mode(mode)?);
        Ok(self)
    }

    /// Request qpdf writer progress reporting.
    pub fn progress(&mut self) -> &mut Self {
        self.job.set_progress(true);
        self
    }

    /// Enable qpdf verbose diagnostics.
    pub fn verbose(&mut self) -> &mut Self {
        self.job.set_verbose(true);
        self
    }

    /// Select the qpdf object inspection target and make output optional.
    pub fn show_object(&mut self, selector: &str) -> Result<&mut Self> {
        self.job.configuration.show_object = Some(parse_job_object_selector(selector.as_bytes())?);
        self.job.configuration.require_output = false;
        Ok(self)
    }

    /// Run the owning job's qpdf configuration consistency checks.
    pub fn check_configuration(&mut self) -> Result<()> {
        self.job.check_configuration()
    }
}

fn map_show_linearization_error(error: ShowLinearizationError) -> Error {
    match error {
        ShowLinearizationError::Io(error) => match error.downcast::<Error>() {
            Ok(error) => *error,
            Err(error) => Error::System(format!("I/O error: {error}")),
        },
        ShowLinearizationError::Malformed { message } => {
            Error::System(format!("malformed linearization data: {message}"))
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

    #[test]
    fn config_verbose_enables_the_job_verbose_setting() {
        let mut job = QPDFJob::new();
        job.config().verbose();

        assert!(job.verbose());
    }

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

    struct RecordingInfoSink {
        bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl crate::pipeline::Pipeline for RecordingInfoSink {
        // cov:ignore-start: the logger never queries an info sink's identifier
        fn identifier(&self) -> &str {
            "recording info sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> crate::pipeline::PipelineResult<()> {
            self.bytes.lock().unwrap().extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: the logger does not finish an info sink during an open
        fn finish(&mut self) -> crate::pipeline::PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    /// qpdf's `createQPDF` reaches `doProcess` for `--show-encryption` too,
    /// so the job's verbose policy and message prefix govern the password
    /// retry diagnostic on the encryption-inspection open exactly as on the
    /// ordinary open (`QPDFJob.cc:1717-1791`).
    #[test]
    fn open_for_encryption_inspection_applies_the_job_verbose_policy_and_prefix() {
        let mut source = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let mut writer = crate::PdfWriter::new(&mut source);
        writer.set_encryption_parameters(crate::EncryptParams::v4_aes128(
            b"caf\xe9".to_vec(),
            b"owner".to_vec(),
        ));
        writer.set_output_memory().expect("memory output");
        writer.write().expect("encrypt fixture");
        let encrypted = writer.get_buffer().expect("encrypted bytes");

        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = crate::QPDFLogger::create();
        logger.set_info(Some(crate::pipeline::PipelineHandle::new(
            RecordingInfoSink {
                bytes: std::sync::Arc::clone(&bytes),
            },
        )));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.configuration.verbose = true;
        job.set_message_prefix("job");

        let pdf = job
            .open_for_encryption_inspection(
                Cursor::new(encrypted),
                "input.pdf",
                PdfOpenOptions {
                    password: "caf\u{e9}".as_bytes().to_vec(),
                    ..PdfOpenOptions::default()
                },
            )
            .expect("qpdf-compatible password recovery authenticates");
        drop(pdf);

        let output = bytes.lock().unwrap();
        assert!(
            output.starts_with(b"job: supplied password didn't work; trying other"),
            "the inspection open must emit the job-prefixed retry line: {:?}",
            String::from_utf8_lossy(&output) // cov:ignore: assertion failure message
        );
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
    fn show_linearization_mapping_preserves_core_errors() {
        let error =
            ShowLinearizationError::Io(Box::new(Error::System("sink write failure 1".to_owned())));

        let mapped = map_show_linearization_error(error);

        assert!(matches!(
            mapped,
            Error::System(message) if message == "sink write failure 1"
        ));

        let error = ShowLinearizationError::Io(Box::new(std::io::Error::other("disk gone")));
        assert!(matches!(
            map_show_linearization_error(error),
            Error::System(message) if message == "I/O error: disk gone"
        ));
        let error = ShowLinearizationError::Malformed {
            message: "bad hint table".to_owned(),
        };
        assert!(matches!(
            map_show_linearization_error(error),
            Error::System(message) if message == "malformed linearization data: bad hint table"
        ));
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
    fn job_json_byte_entry_point_preserves_literal_high_bit_password_bytes() {
        let mut json =
            br#"{"inputFile":"input.pdf","outputFile":"output.pdf","password":""}"#.to_vec();
        json.insert(json.len() - 2, 0x80);

        let mut job = QPDFJob::new();
        job.initialize_from_json_bytes(&json).unwrap();

        assert_eq!(job.configuration.password, vec![0x80]);
    }

    #[test]
    fn job_json_version_rejects_non_utf8_bytes_instead_of_replacing_them() {
        let json = b"{\"inputFile\":\"input.pdf\",\"outputFile\":\"output.pdf\",\"forceVersion\":\"\xff\"}";

        let error = QPDFJob::new()
            .initialize_from_json_bytes(json)
            .expect_err("non-UTF-8 job JSON version must not be lossy");
        assert!(error.to_string().contains(".forceVersion"));
        assert!(error.to_string().contains("UTF-8"));
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
        assert!(parse_job_version(b"1.7.3", ".version").is_ok());
        assert_eq!(
            parse_job_version(b"invalid", ".version").unwrap(),
            ("invalid".to_string(), 0)
        );
        assert_eq!(QPDFJob::parse_collate("2").unwrap(), vec![2]);
        assert_eq!(QPDFJob::parse_collate("0").unwrap(), vec![0]);
        assert_eq!(QPDFJob::parse_collate("not-number").unwrap(), vec![0]);

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
        let insecure_256 =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"","256bit":{}}"#)
                .unwrap();
        assert!(parse_job_encrypt(&insecure_256, true).is_err());
        let allowed_insecure_256 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"","256bit":{"allowInsecure":""}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&allowed_insecure_256, true).is_ok());
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
    fn job_json_split_pages_rejects_an_i32_overflow_at_the_qpdf_boundary() {
        let error = parse_job_split_pages(b"2147483648").expect_err("i32 overflow must fail");
        assert_eq!(
            error.to_string(),
            ".splitPages: invalid page count 2147483648"
        );
    }

    #[test]
    fn job_json_compression_level_uses_qpdf_integer_prefix_conversion() {
        assert_eq!(parse_job_compression_level(b"  +9tail").unwrap(), 9);
        assert_eq!(parse_job_compression_level(b"not-a-number").unwrap(), 0);
        assert!(parse_job_compression_level(b"99999999999999999999").is_err());
    }

    #[test]
    fn job_json_show_object_selector_preserves_qpdf_forms() {
        assert_eq!(
            parse_job_object_selector(b"trailer").unwrap(),
            JobObjectSelector::Trailer
        );
        assert_eq!(
            parse_job_object_selector(b"1").unwrap(),
            JobObjectSelector::Object(ObjectRef::new(1, 0))
        );
        assert_eq!(
            parse_job_object_selector(b"1,").unwrap(),
            JobObjectSelector::Object(ObjectRef::new(1, 0))
        );
        assert_eq!(
            parse_job_object_selector(b"-1").unwrap(),
            JobObjectSelector::NoObject
        );
        assert_eq!(
            parse_job_object_selector(b"").unwrap(),
            JobObjectSelector::NoObject
        );
        assert_eq!(
            parse_job_object_selector(b"1,65536").unwrap(),
            JobObjectSelector::Null
        );
        assert!(parse_job_object_selector(b"999999999999999999999").is_err());
        assert!(parse_job_object_selector(b"99999999999999999999999999999999999999999").is_err());
        assert!(parse_job_object_selector(b"2147483648").is_err());
    }

    #[test]
    fn job_collate_parser_matches_qpdf_parameter_semantics() {
        assert_eq!(QPDFJob::parse_collate("").unwrap(), vec![1]);
        assert_eq!(QPDFJob::parse_collate("2,3").unwrap(), vec![2, 3]);
        assert_eq!(QPDFJob::parse_collate("2,,3").unwrap(), vec![2, 0, 3]);
        assert_eq!(QPDFJob::parse_collate("0").unwrap(), vec![0]);
        assert_eq!(QPDFJob::parse_collate("2abc").unwrap(), vec![2]);
        assert_eq!(QPDFJob::parse_collate("abc").unwrap(), vec![0]);
        assert_eq!(QPDFJob::parse_collate(" +2").unwrap(), vec![2]);

        let leading_comma = QPDFJob::parse_collate(",2").unwrap_err();
        assert!(leading_comma.to_string().contains("trailing comma"));
        let trailing_comma = QPDFJob::parse_collate("2,").unwrap_err();
        assert!(trailing_comma.to_string().contains("trailing comma"));
        let underflow = QPDFJob::parse_collate("-1").unwrap_err();
        assert!(underflow.to_string().contains("underflow converting -1"));
        let overflow = QPDFJob::parse_collate("18446744073709551616").unwrap_err();
        assert!(overflow
            .to_string()
            .contains("overflow converting 18446744073709551616"));
        let narrowing = QPDFJob::parse_collate("4294967296").unwrap_err();
        assert!(narrowing
            .to_string()
            .contains("integer out of range converting 4294967296"));
    }

    #[test]
    fn job_json_private_parser_covers_full_handler_dispatch() {
        let tempdir = tempfile::tempdir().unwrap();
        let password_file = tempdir.path().join("password.txt");
        std::fs::write(&password_file, b"file-password\n").unwrap();
        let nested_job_file = tempdir.path().join("nested.json");
        std::fs::write(&nested_job_file, b"{}").unwrap();
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
            ("copyEncryption", "donor.pdf"),
            ("encryptionFilePassword", "donor-password"),
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
            ("flattenAnnotations", "all"),
            ("checkLinearization", ""),
            ("jsonOutput", "latest"),
            ("externalizeInlineImages", ""),
            ("iiMinBytes", "100"),
            ("keepInlineImages", ""),
            ("optimizeImages", ""),
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
            ("ignoreXrefStreams", ""),
            ("passwordIsHexKey", ""),
            ("passwordMode", "auto"),
            ("suppressPasswordRecovery", ""),
            ("suppressRecovery", ""),
            ("compressionLevel", "1"),
            ("reportMemoryUsage", ""),
            ("isEncrypted", ""),
            ("requiresPassword", ""),
            ("filteredStreamData", ""),
            ("rawStreamData", ""),
            ("showXref", ""),
            ("showLinearization", ""),
            ("showObject", "trailer"),
            ("listAttachments", ""),
            ("showAttachment", "attachment.txt"),
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
            "jobJsonFile".to_owned(),
            serde_json::json!(nested_job_file.to_string_lossy()),
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
        job.initialize_from_json_partial(r#"{"passwordMode":"invalid"}"#)
            .expect_err("passwordMode choices must be known");

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
        job.initialize_from_json_partial(r#"{"pages":[]}"#)
            .expect_err("an empty pages array must finish with qpdf's no-specification error");

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
    fn job_json_file_rejects_recursive_includes() {
        let tempdir = tempfile::tempdir().unwrap();
        let first = tempdir.path().join("first.json");
        let second = tempdir.path().join("second.json");
        std::fs::write(
            &first,
            serde_json::json!({"jobJsonFile": second.display().to_string()}).to_string(),
        )
        .unwrap();
        std::fs::write(
            &second,
            serde_json::json!({"jobJsonFile": first.display().to_string()}).to_string(),
        )
        .unwrap();
        let json = serde_json::json!({"jobJsonFile": first.display().to_string()}).to_string();
        let mut job = QPDFJob::new();
        let error = job
            .initialize_from_json_partial(&json)
            .expect_err("recursive job JSON includes must be bounded");
        assert!(error
            .to_string()
            .contains("recursive jobJsonFile reference"));

        let mut non_dictionary_job = QPDFJob::new();
        assert!(non_dictionary_job
            .initialize_from_json_partial("[]")
            .is_err());
        let scalar_file = tempdir.path().join("scalar.json");
        std::fs::write(&scalar_file, b"[]").unwrap();
        let mut scalar_job = QPDFJob::new();
        assert!(scalar_job
            .initialize_from_json_partial(
                &serde_json::json!({
                    "jobJsonFile": scalar_file.display().to_string()
                })
                .to_string()
            )
            .is_err());
    }

    #[test]
    fn job_json_nested_dispatch_keeps_qpdf_shared_state_and_key_order() {
        let tempdir = tempfile::tempdir().unwrap();
        let nested = tempdir.path().join("nested.json");
        std::fs::write(
            &nested,
            serde_json::json!({
                "collate": "2,3",
                "jsonStreamData": "file",
                "rotate": "90:1"
            })
            .to_string(),
        )
        .unwrap();
        let json = serde_json::json!({
            "collate": "4",
            "jobJsonFile": nested.display().to_string(),
            "jsonOutput": "2",
            "rotate": "180:1"
        })
        .to_string();

        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(&json).unwrap();

        assert_eq!(job.configuration.collate, Some(vec![4, 2, 3]));
        assert_eq!(job.configuration.json_stream_data, JsonStreamData::File);
        assert!(job.configuration.json_stream_data_set);
        assert_eq!(job.configuration.rotations.len(), 1);
        assert_eq!(job.configuration.rotations["1"].op.degrees, 180);
    }

    #[test]
    fn job_json_nested_dispatch_appends_attachment_operations() {
        let tempdir = tempfile::tempdir().unwrap();
        let nested = tempdir.path().join("nested.json");
        std::fs::write(
            &nested,
            serde_json::json!({
                "copyAttachmentsFrom": [{"file": "inner.pdf", "prefix": "inner-"}],
                "removeAttachment": ["inner-key"]
            })
            .to_string(),
        )
        .unwrap();
        let json = serde_json::json!({
            "copyAttachmentsFrom": [{"file": "outer.pdf", "prefix": "outer-"}],
            "jobJsonFile": nested.display().to_string(),
            "removeAttachment": ["outer-key"]
        })
        .to_string();

        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(&json).unwrap();

        assert_eq!(
            job.configuration
                .attachments_to_copy
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("outer.pdf"), Path::new("inner.pdf")]
        );
        assert_eq!(
            job.configuration.attachments_to_remove,
            [b"inner-key".to_vec(), b"outer-key".to_vec()]
        );
    }

    #[test]
    fn job_json_nested_dispatch_rejects_duplicate_output_files() {
        let tempdir = tempfile::tempdir().unwrap();
        let nested = tempdir.path().join("nested.json");
        std::fs::write(
            &nested,
            serde_json::json!({"outputFile": "inner.pdf"}).to_string(),
        )
        .unwrap();
        let json = serde_json::json!({
            "jobJsonFile": nested.display().to_string(),
            "outputFile": "outer.pdf"
        })
        .to_string();

        let mut job = QPDFJob::new();
        let error = job
            .initialize_from_json_partial(&json)
            .expect_err("qpdf accepts only one output file");
        assert!(matches!(
            error,
            Error::Usage(usage) if usage.to_string() == "output file has already been given"
        ));
    }

    #[test]
    fn job_json_encryption_status_and_copy_errors_use_job_boundaries() {
        let fixture_root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let encrypted = fixture_root.join("encrypted/v4-aes-128-r4.pdf");
        let plaintext = fixture_root.join("minimal.pdf");
        let cases = [
            (
                serde_json::json!({
                    "inputFile": encrypted.display().to_string(),
                    "isEncrypted": ""
                })
                .to_string(),
                JobExitCode::Success,
            ),
            (
                serde_json::json!({
                    "inputFile": plaintext.display().to_string(),
                    "requiresPassword": ""
                })
                .to_string(),
                JobExitCode::Error,
            ),
        ];
        for (json, expected) in cases {
            let mut job = QPDFJob::new();
            let logger = QPDFLogger::create();
            logger.set_info(Some(logger.discard()));
            logger.set_warn(Some(logger.discard()));
            logger.set_error(Some(logger.discard()));
            job.set_logger(logger);
            job.initialize_from_json_partial(&json).unwrap();
            assert_eq!(job.run().unwrap(), expected);
        }

        let tempdir = tempfile::tempdir().unwrap();
        let missing = tempdir.path().join("missing.pdf");
        let mut missing_job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        missing_job.set_logger(logger);
        missing_job
            .initialize_from_json_partial(
                &serde_json::json!({
                    "inputFile": missing.display().to_string(),
                    "isEncrypted": ""
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(missing_job.run().unwrap(), JobExitCode::Error);

        let malformed = tempdir.path().join("malformed.pdf");
        std::fs::write(&malformed, b"not a PDF").unwrap();
        let mut malformed_job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        malformed_job.set_logger(logger);
        malformed_job
            .initialize_from_json_partial(
                &serde_json::json!({
                    "inputFile": malformed.display().to_string(),
                    "requiresPassword": ""
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(malformed_job.run().unwrap(), JobExitCode::Error);

        let mut copy_job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        copy_job.set_logger(logger);
        copy_job
            .initialize_from_json_partial(
                &serde_json::json!({
                    "inputFile": plaintext.display().to_string(),
                    "outputFile": tempdir.path().join("output.pdf").display().to_string(),
                    "copyEncryption": plaintext.display().to_string()
                })
                .to_string(),
            )
            .unwrap();
        assert_eq!(copy_job.run().unwrap(), JobExitCode::Error);
    }

    #[test]
    fn job_json_status_rejects_combined_encryption_queries() {
        let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/minimal.pdf");
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(
            &serde_json::json!({
                "inputFile": input.display().to_string(),
                "isEncrypted": "",
                "requiresPassword": ""
            })
            .to_string(),
        )
        .unwrap();
        let error = job
            .run()
            .expect_err("status queries are mutually exclusive");
        assert!(error
            .to_string()
            .contains("--requires-password and --is-encrypted may not be given together"));
    }

    #[test]
    fn job_json_show_linearization_reports_soft_warnings() {
        let mut bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
        )
        .unwrap();
        let offset = bytes
            .windows(3)
            .position(|window| window == b"/N ")
            .expect("linearized fixture has /N");
        bytes[offset + 3] = b'Z';
        let tempdir = tempfile::tempdir().unwrap();
        let input = tempdir.path().join("linearized.pdf");
        std::fs::write(&input, bytes).unwrap();

        let mut job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        job.set_logger(logger);
        job.initialize_from_json_partial(
            &serde_json::json!({
                "inputFile": input.display().to_string(),
                "showLinearization": ""
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    }

    #[test]
    fn job_json_inspection_dispatch_covers_object_and_report_variants() {
        let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/minimal.pdf");
        for selector in ["trailer", "1", "-1", "1,65536"] {
            let mut job = QPDFJob::new();
            let logger = QPDFLogger::create();
            logger.set_info(Some(logger.discard()));
            logger.set_warn(Some(logger.discard()));
            logger.set_error(Some(logger.discard()));
            job.set_logger(logger);
            job.initialize_from_json_partial(
                &serde_json::json!({
                    "inputFile": input.display().to_string(),
                    "showObject": selector
                })
                .to_string(),
            )
            .unwrap();
            assert_eq!(job.run().unwrap(), JobExitCode::Success);
        }

        let mut job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        job.set_logger(logger);
        job.initialize_from_json_partial(
            &serde_json::json!({
                "inputFile": input.display().to_string(),
                "showNpages": "",
                "showPages": "",
                "showLinearization": "",
                "showXref": "",
                "listAttachments": ""
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(job.run().unwrap(), JobExitCode::Success);
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

    #[cfg(unix)]
    #[test]
    fn attachment_default_names_preserve_non_utf8_basename_bytes() {
        let mut json = b"{\"file\":\"attachment-".to_vec();
        json.push(0x80);
        json.extend_from_slice(b".bin\"}");
        let value = crate::json::Json::parse(&json).unwrap();

        let options = parse_job_attachment(&value, ".addAttachment[0]").unwrap();

        assert_eq!(options.key, b"attachment-\x80.bin");
        assert_eq!(options.filename, b"attachment-\x80.bin");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn job_json_path_fields_preserve_non_utf8_bytes_in_configuration() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        use std::path::PathBuf;

        fn non_utf8_path(directory: &std::path::Path, name: &[u8]) -> PathBuf {
            let mut bytes = directory.as_os_str().as_bytes().to_vec();
            bytes.push(b'/');
            bytes.extend_from_slice(name);
            PathBuf::from(OsString::from_vec(bytes))
        }

        fn append_member(json: &mut Vec<u8>, first: &mut bool, key: &[u8], value: &[u8]) {
            if !*first {
                json.push(b',');
            }
            *first = false;
            json.push(b'"');
            json.extend_from_slice(key);
            json.extend_from_slice(b"\":\"");
            json.extend_from_slice(value);
            json.push(b'"');
        }

        fn append_file_object(
            json: &mut Vec<u8>,
            first: &mut bool,
            key: &[u8],
            path: &[u8],
            suffix: &[u8],
        ) {
            if !*first {
                json.push(b',');
            }
            *first = false;
            json.push(b'"');
            json.extend_from_slice(key);
            json.extend_from_slice(b"\":{\"file\":\"");
            json.extend_from_slice(path);
            json.extend_from_slice(b"\"");
            json.extend_from_slice(suffix);
        }

        let directory = tempfile::tempdir().unwrap();
        let input = non_utf8_path(directory.path(), b"input-\x80.pdf");
        let output = non_utf8_path(directory.path(), b"output-\x80.pdf");
        let password_file = non_utf8_path(directory.path(), b"password-\x80.txt");
        let linearize_pass1 = non_utf8_path(directory.path(), b"pass1-\x80.tmp");
        let update = non_utf8_path(directory.path(), b"update-\x80.json");
        let nested = non_utf8_path(directory.path(), b"nested-\x80.json");
        std::fs::write(&password_file, b"password\nignored\n").unwrap();
        std::fs::write(&nested, b"{}").unwrap();

        let input_bytes = input.as_os_str().as_bytes();
        let output_bytes = output.as_os_str().as_bytes();
        let password_file_bytes = password_file.as_os_str().as_bytes();
        let linearize_pass1_bytes = linearize_pass1.as_os_str().as_bytes();
        let update_bytes = update.as_os_str().as_bytes();
        let nested_bytes = nested.as_os_str().as_bytes();

        let mut json = b"{".to_vec();
        let mut first = true;
        append_member(&mut json, &mut first, b"inputFile", input_bytes);
        append_member(&mut json, &mut first, b"outputFile", output_bytes);
        append_member(&mut json, &mut first, b"copyEncryption", input_bytes);
        append_member(&mut json, &mut first, b"passwordFile", password_file_bytes);
        append_member(
            &mut json,
            &mut first,
            b"linearizePass1",
            linearize_pass1_bytes,
        );
        append_member(&mut json, &mut first, b"updateFromJson", update_bytes);
        append_file_object(
            &mut json,
            &mut first,
            b"pages",
            input_bytes,
            b",\"range\":\"1\"}",
        );
        append_file_object(&mut json, &mut first, b"overlay", input_bytes, b"}");
        append_file_object(&mut json, &mut first, b"addAttachment", input_bytes, b"}");
        append_file_object(
            &mut json,
            &mut first,
            b"copyAttachmentsFrom",
            input_bytes,
            b"}",
        );
        append_member(&mut json, &mut first, b"jobJsonFile", nested_bytes);
        json.push(b'}');

        let mut job = QPDFJob::new();
        job.initialize_from_json_partial_bytes(&json).unwrap();

        assert_eq!(job.configuration.input_file.as_ref(), Some(&input));
        assert_eq!(job.configuration.output_file.as_ref(), Some(&output));
        assert_eq!(job.configuration.copy_encryption.as_ref(), Some(&input));
        assert_eq!(job.configuration.password, b"password");
        assert_eq!(
            job.configuration.linearize_pass1.as_ref(),
            Some(&linearize_pass1)
        );
        assert_eq!(job.configuration.update_from_json.as_ref(), Some(&update));
        assert_eq!(job.configuration.page_specs[0].path, input);
        assert_eq!(job.configuration.overlays[0].path, input);
        assert_eq!(job.configuration.attachments_to_add[0].path, input);
        assert_eq!(job.configuration.attachments_to_copy[0].path, input);
    }
}
