//! Own the shared job state for diagnostics, progress, warnings, and completion.
//!
//! qpdf correspondence: `QPDFJob` shared state and completion boundary.
//!
//!

use super::attachments::AttachmentAddOptions;
use super::attachments::AttachmentCopyOptions;
use super::image_optimization::{optimize_images, ImageOptimizationOptions};
use super::json::{JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData};
use super::overlay::{
    handle_under_overlay, overlay_verbose_report, OverlayKind, OverlaySpec, OverlayVerbosePage,
};
use super::page_range::PageRange;
use super::page_specs::{PageSpecInput, PageSpecJobOutput};
use super::page_split::SplitPageOptions;
use super::resource_pruning::RemoveUnreferencedResources;
use super::rotate::flatten_rotation_on_pages;
use super::rotate_spec::{parse_rotation_parameter, RotationSpec};
use crate::encryption::{
    EncryptMethod, EncryptParams, PasswordMode, PermissionsConfig, R2PermissionsConfig,
};
use crate::json_inspect::{DecodeLevel as JsonDecodeLevel, JsonKey};
use crate::linearization::{show_linearization_pdf_with_warnings, ShowLinearizationError};
use crate::pipeline::{Pipeline, PipelineHandle, PipelineResult};
use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};
use crate::{
    AcroFormDocumentHelper, Error, ObjectHandle, ObjectRef, ObjectStreamMode, PageDocumentHelper,
    PageObjectHelper, Pdf, PdfOpenOptions, PdfVersion, PdfWriter, QPDFLogger, ReadSeek, Result,
    UsageError, WriterConfiguration,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[path = "argv.rs"]
mod argv;

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

/// Batch size for job output written through the shared save pipeline.
const JOB_OUTPUT_BUFFER_CAPACITY: usize = 4096;

/// Adapt the job's save pipeline to [`Write`], batching small fragments.
///
/// Both of qpdf's JSON destinations are buffered before they reach the
/// operating system: a named output file is a `FILE*` driven through
/// `Pl_StdioFile` (`libqpdf/QPDFJob.cc:3103-3104`), and standard output is the
/// C++ stream whose underlying `stdout` the CLI puts in line-buffered mode
/// (`qpdf/qpdf.cc:30`, `libqpdf/QUtil.cc:780-784`). flpdf's save pipeline is a
/// mutex-guarded handle, so the serializer's many small fragments would each
/// take a lock and a write; the file destination already batches through
/// `StdioBuffer`. Batching changes no output bytes.
struct JobOutputWriter {
    pipeline: PipelineHandle,
    buffer: Vec<u8>,
}

impl JobOutputWriter {
    fn new(pipeline: PipelineHandle) -> Self {
        Self {
            pipeline,
            buffer: Vec::with_capacity(JOB_OUTPUT_BUFFER_CAPACITY),
        }
    }

    /// Hand every batched byte to the pipeline.
    fn flush_buffer(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let result = self
            .pipeline
            .write(&self.buffer)
            .map_err(std::io::Error::other);
        self.buffer.clear();
        result
    }
}

impl Write for JobOutputWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.buffer.len() + data.len() > JOB_OUTPUT_BUFFER_CAPACITY {
            self.flush_buffer()?;
        }
        if data.len() >= JOB_OUTPUT_BUFFER_CAPACITY {
            self.pipeline.write(data).map_err(std::io::Error::other)?;
        } else {
            self.buffer.extend_from_slice(data);
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_buffer()
    }
}

/// Persistent qpdf `Config` encryption state.
///
/// qpdf's `encrypt()` and `decrypt()` callbacks change the active operation,
/// but `decrypt()` does not reset the password, permission, AES, revision, or
/// metadata fields that a later `encrypt()` callback reuses
/// (`QPDFJob_config.cc:147-157,1088-1101`). Keep those fields independently
/// from the writer's currently active parameters so argv occurrence order
/// remains observable across `--decrypt` and repeated `--encrypt` groups.
#[derive(Debug, Clone, Default)]
struct EncryptionDefaults {
    user_password: Vec<u8>,
    owner_password: Vec<u8>,
    use_aes: bool,
    force_v4: bool,
    force_r5: bool,
    allow_insecure: bool,
    cleartext_metadata: bool,
    permissions: PermissionsConfig,
    r2_permissions: R2PermissionsConfig,
    accessibility_disabled: bool,
}

/// Portable writer/input state populated by the qpdf job argv/JSON boundary.
///
/// This owns the settings needed for qpdf job initialization and the later
/// create/write/inspection stages. Native flpdf subcommand parsing remains in
/// the CLI, but qpdf's flat option grammar terminates here.
#[derive(Debug, Clone, Default)]
struct JobConfiguration {
    input_file: Option<PathBuf>,
    empty_input: bool,
    output_file: Option<PathBuf>,
    password: Vec<u8>,
    copy_encryption: Option<PathBuf>,
    /// Whether `copy_encryption` still governs the output encryption.
    ///
    /// qpdf's `--decrypt`/`--encrypt` clear the `copy_encryption` flag but
    /// keep `encryption_file` for the page-spec password fallback
    /// (`libqpdf/QPDFJob_config.cc:146-148,155-157,1164-1166`).
    copy_encryption_applies_to_writer: bool,
    encryption_file_password: Vec<u8>,
    password_mode: PasswordMode,
    ignore_xref_streams: bool,
    password_is_hex_key: bool,
    suppress_password_recovery: bool,
    suppress_recovery: bool,
    /// qpdf's `max_input_version`: the greatest version among the documents
    /// this job opened. It lives on the job, not on the writer, because
    /// `doProcessOnce` accumulates it during the create stage
    /// (`QPDFJob.cc:1695-1716`) while `setWriterOptions` hands it to the
    /// writer at write time (`QPDFJob.cc:2913`).
    max_input_version: Option<(String, i64)>,
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
    rotations: BTreeMap<Vec<u8>, RotationSpec>,
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
    /// qpdf's `QPDFJob::Members::normalize` flag used by `doShowObj` and
    /// propagated to the writer by `setWriterOptions`.
    normalize_content: Option<bool>,
    writer: WriterConfiguration,
    linearize: bool,
    linearize_pass1: Option<PathBuf>,
    allow_weak_crypto: bool,
    encryption_defaults: EncryptionDefaults,
    page_specs: Vec<JobPageConfig>,
    page_specs_origin: PageSpecsOrigin,
    collate: Option<Vec<usize>>,
    overlays: Vec<JobOverlayConfig>,
    underlays: Vec<JobOverlayConfig>,
    attachments_to_add: Vec<AttachmentAddOptions>,
    attachments_to_copy: Vec<JobCopyAttachmentsConfig>,
    attachments_to_remove: Vec<Vec<u8>>,
    remove_unreferenced_resources: RemoveUnreferencedResources,
    set_page_labels: Option<Vec<PageLabelSpec>>,
    remove_page_labels: bool,
    json_version: Option<i32>,
    json_output: bool,
    json_decode_level: crate::writer::DecodeLevel,
    json_decode_level_set: bool,
    json_keys: Vec<JsonKey>,
    json_objects: Vec<String>,
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

/// Build qpdf's initial job state.
///
/// qpdf keeps the job-level `decode_level` default at generalized, but leaves
/// `decode_level_set` false until `Config::decodeLevel` is called
/// (`include/qpdf/QPDFJob.hh:635-637`, `libqpdf/QPDFJob_config.cc:717-729`).
/// The writer therefore retains its own default decode level (none) until the
/// explicit setter is replayed by `setWriterOptions`
/// (`libqpdf/QPDFJob.cc:2865-2875`). `JobConfiguration::default` already keeps
/// those two states separate: `json_decode_level` defaults to generalized,
/// while `WriterConfiguration` defaults to the standalone writer state.
fn qpdf_default_job_configuration() -> JobConfiguration {
    JobConfiguration::default()
}

/// qpdf opens one `Config::pages()` group and then permits multiple
/// `pageSpec()` calls inside that group. JSON and the fluent Config surface
/// must therefore share one origin marker rather than using the page-spec
/// vector's non-empty state as a proxy (`QPDFJob_config.cc:945-969`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PageSpecsOrigin {
    #[default]
    None,
    Config,
    Json,
}

/// One qpdf `--set-page-labels` specification after the argv/Config parser has
/// validated its grammar. Relative first-page values retain qpdf's signed
/// representation (`rN` becomes `-N`, `z` becomes `-1`) until the document
/// page count is known in `handleTransformations`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PageLabelSpec {
    first_page: i64,
    style: crate::page_label_document_helper::LabelStyle,
    start: i64,
    prefix: Vec<u8>,
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
    /// `None` is qpdf's null `PageSpec::password`; `Some(Vec::new())` is an
    /// explicitly supplied empty password. The distinction controls the
    /// `--encryption-file-password` fallback in `prepare_document`.
    password: Option<Vec<u8>>,
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
    // A nested `jobJsonFile` JSON key dispatches through the same
    // `Config::jobJsonFile` callback as the CLI's `--job-json-file`
    // (`libqpdf/qpdf/auto_job_json_init.hh:472-474`), which reads the file
    // via `QUtil::read_file_into_string` -> `QUtil::safe_fopen`
    // (`QPDFJob_config.cc:776`, `libqpdf/QUtil.cc:490-519,1167-1172`) and
    // reports a missing/unreadable file with portable `strerror` wording, not
    // Rust's `io::Error` text.
    let bytes = std::fs::read(path).map_err(|error| {
        Error::System(format!(
            "open {}: {}",
            path.display(),
            crate::qutil::strerror_text(&error)
        ))
    })?;
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
    job_json_range_with_empty_default(value, path, true)
}

fn job_json_overlay_range(value: Option<&crate::json::Json>, path: &str) -> Result<PageRange> {
    job_json_range_with_empty_default(value, path, false)
}

fn job_json_range_with_empty_default(
    value: Option<&crate::json::Json>,
    path: &str,
    empty_is_all: bool,
) -> Result<PageRange> {
    let Some(value) = value else {
        return Ok(PageRange::all());
    };
    let bytes = value
        .get_string()
        .ok_or_else(|| Error::Usage(UsageError::new(format!("{path}: value must be a string"))))?;
    if bytes.is_empty() {
        return Ok(if empty_is_all {
            PageRange::all()
        } else {
            PageRange::empty()
        });
    }
    let value = String::from_utf8_lossy(&bytes);
    PageRange::parse_numrange(&value)
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

fn parse_job_encrypt(
    value: &crate::json::Json,
    _allow_weak_crypto: bool,
    inherited: &EncryptionDefaults,
) -> Result<(EncryptParams, EncryptionDefaults)> {
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
    let allow_insecure = inherited.allow_insecure || job_json_bare(&settings, b"allowInsecure")?;
    let mut permissions = inherited.permissions;
    let mut r2_permissions = inherited.r2_permissions;
    let mut accessibility_disabled = inherited.accessibility_disabled;
    if let Some(value) = job_json_yn(&settings, b"accessibility")? {
        permissions.accessibility = value;
        accessibility_disabled = !value;
    }
    if let Some(value) = job_json_yn(&settings, b"annotate")? {
        if key_length == "40bit" {
            r2_permissions.annotate = value;
        } else {
            permissions.annotate = value;
        }
    }
    if let Some(value) = job_json_yn(&settings, b"assemble")? {
        permissions.assemble = value;
    }
    if let Some(value) = job_json_yn(&settings, b"extract")? {
        if key_length == "40bit" {
            r2_permissions.extract = value;
        } else {
            permissions.extract = value;
        }
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
        if key_length == "40bit" {
            r2_permissions.modify = value == "y";
        } else {
            job_json_modify_permission(&value, &mut permissions)?;
        }
    }
    if let Some(value) = job_json_yn(&settings, b"modifyOther")? {
        permissions.modify_contents = value;
    }
    if let Some(value) = job_json_choice(&settings, b"print", &["full", "low", "none"], true)? {
        if key_length == "40bit" {
            r2_permissions.print = value == "y";
        } else {
            job_json_print_permission(&value, &mut permissions)?;
        }
    }

    let defaults_user_password = user_password.clone();
    let defaults_owner_password = owner_password.clone();
    let use_aes = match key_length {
        // Config::encrypt(256, ...) unconditionally sets use_aes before the
        // JSON encryption handler applies its key-length-specific options
        // (`QPDFJob_config.cc:1088-1096`).
        "256bit" => true,
        "128bit" => job_json_choice(&settings, b"useAes", &["y", "n"], true)?
            .map_or(inherited.use_aes, |value| value == "y"),
        // Config::encrypt(40, ...) leaves use_aes untouched, so a later
        // 128-bit group can still reuse AES selected by an earlier group.
        "40bit" => inherited.use_aes,
        _ => unreachable!("key length was validated above"), // cov:ignore: key length comes only from the validated qpdf job schema choices
    };
    let force_v4 =
        inherited.force_v4 || (key_length == "128bit" && job_json_bare(&settings, b"forceV4")?);
    let force_r5 =
        inherited.force_r5 || (key_length == "256bit" && job_json_bare(&settings, b"forceR5")?);
    let cleartext_metadata =
        inherited.cleartext_metadata || job_json_bare(&settings, b"cleartextMetadata")?;

    let mut params = match key_length {
        "40bit" => EncryptParams::rc4(EncryptMethod::V1Rc440, user_password, owner_password),
        "128bit" => {
            if use_aes {
                EncryptParams::v4_aes128(user_password, owner_password)
            } else if force_v4 || cleartext_metadata {
                EncryptParams::rc4(EncryptMethod::V4Rc4128, user_password, owner_password)
            } else {
                EncryptParams::rc4(EncryptMethod::V2Rc4128, user_password, owner_password)
            }
        }
        "256bit" => {
            if force_r5 {
                EncryptParams::v5_r5(user_password, owner_password)
            } else {
                EncryptParams::v5_r6(user_password, owner_password)
            }
        }
        _ => unreachable!("key length was validated above"), // cov:ignore: key length comes only from the validated qpdf job schema choices
    };
    params.permissions = permissions;
    params.r2_permissions = r2_permissions;
    if matches!(
        params.method,
        EncryptMethod::V4Aes128
            | EncryptMethod::V4Rc4128
            | EncryptMethod::V5R5Aes256
            | EncryptMethod::V5R6Aes256
    ) {
        params.permissions.accessibility = true;
    }
    params.encrypt_metadata = !cleartext_metadata;
    // qpdf defers weak-RC4 refusal until `setEncryptionOptions` at writer
    // setup (`QPDFJob.cc:2738-2762`), so a later JSON/argv occurrence can
    // still set allowWeakCrypto or decrypt before the final write.
    let defaults = EncryptionDefaults {
        user_password: defaults_user_password,
        owner_password: defaults_owner_password,
        use_aes,
        force_v4,
        force_r5,
        allow_insecure,
        cleartext_metadata,
        permissions,
        r2_permissions,
        accessibility_disabled,
    };
    Ok((params, defaults))
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
        // qpdf reports the conversion failure itself rather than a
        // splitPages-specific message: `--split-pages=<huge>` prints
        // "overflow/underflow converting <n> to 64-bit integer" and an i32
        // overflow prints the narrowing text, through the same
        // `QUtil::string_to_int` boundary every other numeric option uses.
        // `parse_job_compression_level` below already propagates it this way.
        QpdfIntParse::Overflow(message) => Err(Error::System(message)),
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
        let from = job_json_overlay_range(
            members.get(b"from".as_slice()),
            &format!(
                ".{}[{index}].from",
                match kind {
                    OverlayKind::Overlay => "overlay",
                    OverlayKind::Underlay => "underlay",
                }
            ),
        )?; // cov:ignore: llvm-cov attributes this successful range conversion to the opening call lines
        let to = job_json_overlay_range(
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
            .map(|value| job_json_overlay_range(Some(value), "underlay/overlay repeat"))
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

const PAGE_LABEL_SPEC_ERROR: &str = "page label spec must be n:[D|a|A|r|R][/start[/prefix]]";

fn page_label_spec_error() -> Error {
    Error::Usage(UsageError::new(PAGE_LABEL_SPEC_ERROR))
}

fn parse_decimal_bytes(value: &[u8]) -> Option<i64> {
    if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    value.iter().try_fold(0i64, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(digit - b'0')))
    })
}

/// Parse qpdf's `Config::setPageLabels` grammar before a document is opened.
/// Page-count/order checks remain in [`parse_job_page_labels`], matching
/// `QPDFJob_config.cc:1101-1151` and `QPDFJob.cc:2199-2224`.
fn parse_page_label_spec(spec: &[u8]) -> Result<PageLabelSpec> {
    let Some(colon) = spec.iter().position(|byte| *byte == b':') else {
        return Err(page_label_spec_error());
    };
    let first_page = &spec[..colon];
    let label_spec = &spec[colon + 1..];
    let first_page = if first_page == b"z" {
        -1
    } else if let Some(value) = first_page.strip_prefix(b"r") {
        let value = parse_decimal_bytes(value).ok_or_else(page_label_spec_error)?;
        value.checked_neg().ok_or_else(page_label_spec_error)?
    } else {
        parse_decimal_bytes(first_page).ok_or_else(page_label_spec_error)?
    };

    let mut parts = label_spec.splitn(3, |byte| *byte == b'/');
    let style = match parts.next().unwrap_or_default() {
        b"" => crate::page_label_document_helper::LabelStyle::None,
        b"D" => crate::page_label_document_helper::LabelStyle::Decimal,
        b"a" => crate::page_label_document_helper::LabelStyle::AlphaLower,
        b"A" => crate::page_label_document_helper::LabelStyle::AlphaUpper,
        b"r" => crate::page_label_document_helper::LabelStyle::RomanLower,
        b"R" => crate::page_label_document_helper::LabelStyle::RomanUpper,
        _ => return Err(page_label_spec_error()),
    };
    let start = match parts.next() {
        None | Some(b"") => 1,
        // qpdf's `start` group is `(\d+)?` inside the spec regex
        // (`libqpdf/QPDFJob_config.cc:1101-1108`), so a non-numeric start makes
        // the whole spec fail to match and produces the spec error. Only a
        // parsed value below 1 reaches `usage("starting page number must be >=
        // 1")` (`:1141-1144`).
        Some(value) => parse_decimal_bytes(value).ok_or_else(page_label_spec_error)?,
    };
    if start < 1 {
        return Err(Error::Usage(UsageError::new(
            "starting page number must be >= 1",
        )));
    }
    let prefix = parts.next().map_or_else(Vec::new, ToOwned::to_owned);
    Ok(PageLabelSpec {
        first_page,
        style,
        start,
        prefix,
    })
}

fn parse_job_page_labels(
    specs: &[PageLabelSpec],
    page_count: usize,
) -> Result<Vec<(i64, PageLabelSpec)>> {
    let page_count = i64::try_from(page_count)
        .map_err(|_| Error::Unsupported("page count exceeds qpdf's range".to_owned()))?;
    let mut entries = Vec::with_capacity(specs.len());
    let mut last_page = 0i64;
    for spec in specs {
        let first_page = if spec.first_page < 0 {
            page_count
                .checked_add(1)
                .and_then(|value| value.checked_add(spec.first_page))
                .ok_or_else(|| Error::Unsupported("page label page overflow".to_owned()))?
        } else {
            spec.first_page
        };
        if entries.is_empty() {
            if first_page != 1 {
                // qpdf raises these three with `throw std::runtime_error(...)`
                // (`libqpdf/QPDFJob.cc:2206,2211,2215`), not `QPDFUsage`. Its CLI
                // catches the two separately (`qpdf/qpdf.cc:37-41`): a usage error
                // goes through `usageExit` and prints the banner, while a plain
                // exception prints only `qpdf: <what()>`. Keeping these as usage
                // errors adds a 9-line banner qpdf never emits here.
                return Err(Error::SystemBytes(
                    b"the first page label specification must start with page 1".to_vec(),
                ));
            }
        } else if first_page <= last_page {
            return Err(Error::SystemBytes(
                b"page label specifications must be in order by first page".to_vec(),
            ));
        }
        if first_page < 1 || first_page > page_count {
            return Err(Error::SystemBytes(
                format!(
                    "page label spec: page {first_page} is more than the total number of pages ({page_count})"
                )
                .into_bytes(),
            ));
        }
        entries.push((first_page - 1, spec.clone()));
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

/// Encryption bits retained by qpdf between `createQPDF` and `getExitCode`.
///
/// qpdf records these independently from the ordinary warning state because
/// `--is-encrypted` and `--requires-password` return their status without
/// running the normal output/inspection operation.
#[derive(Debug, Clone, Copy, Default)]
struct EncryptionStatus {
    encrypted: bool,
    password_incorrect: bool,
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
    message_prefix_bytes: Vec<u8>,
    warnings: bool,
    suppress_warnings: bool,
    warnings_exit_zero: bool,
    progress_handler: Option<SharedProgressHandler>,
    configuration: JobConfiguration,
    /// Source documents retained by the create-stage page merge. The copied
    /// document may contain provider-backed streams that are intentionally
    /// read only when the later write stage consumes them.
    page_source_documents: Vec<JobDocument>,
    /// Overlay and underlay donors retained through the write boundary for
    /// the same deferred foreign-stream contract.
    overlay_sources: Vec<OverlaySpec<Box<dyn ReadSeek>>>,
    /// Encryption captured from an encrypted primary before a multi-source
    /// page merge replaces it with a fresh target. qpdf's writer preserves
    /// that primary encryption after `createQPDF` has built the merged
    /// document, so keep the authenticated snapshot on the job until
    /// `write_qpdf` (`QPDFJob.cc:2891-2920`).
    primary_copy_encryption: Option<crate::CopyEncryptionSource>,
    /// qpdf's status bits consumed by the side-effect-free exit-code query.
    encryption_status: EncryptionStatus,
    /// Whether the last create stage returned qpdf's successful null document
    /// result. `create_qpdf` also uses `None` for errors, so the `run` wrapper
    /// needs this separate bit to distinguish the `show_encryption`-
    /// after-bad-password path from an ordinary failed creation.
    create_qpdf_succeeded_without_document: bool,
    /// Whether this job created its primary document through
    /// [`QPDFJob::create_empty_document`]. qpdf's `Config::emptyInput` keys the
    /// page-spec source map with the empty string while `QPDF::emptyPDF`
    /// leaves the job configuration alone (`libqpdf/QPDFJob_config.cc:27-38`,
    /// `libqpdf/QPDF.cc:290-293`); track the factory outcome separately so a
    /// reused job can still receive an input file afterwards.
    empty_primary_created: bool,
    /// Whether the current pre-run JSON initialization sequence has already
    /// populated the configuration. qpdf applies repeated `jobJsonFile`
    /// occurrences to the same Config object.
    partial_json_initialized: bool,
    /// Preserve flpdf's existing reusable-job boundary after `run`: a later
    /// partial JSON sequence starts fresh, while CLI settings before the first
    /// sequence remain visible to qpdf's shared Config.
    has_run: bool,
    /// qpdf's help/version callbacks terminate the argv entry point before a
    /// document lifecycle starts. Keep that result on the job so a caller that
    /// uniformly invokes `run` observes the same successful early exit.
    argv_early_exit: bool,
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

/// How an inspection-stage failure reaches the `write_qpdf` boundary.
///
/// qpdf's inspection stage reports nothing itself; every failure escapes as an
/// exception and the CLI's single catch prints `qpdf: <what()>` once
/// (`qpdf/qpdf.cc:39-41`). flpdf's check consumer already writes that line for
/// detected structural errors (`job/check.rs::report_errors_detected`,
/// mirroring the `std::runtime_error("errors detected")` qpdf throws at
/// `libqpdf/QPDFJob.cc:793`), so the boundary has to know which failures are
/// still owed a diagnostic.
#[derive(Debug)]
enum InspectionFailure {
    /// The diagnostic has already been written; the boundary must stay silent.
    Reported(Error),
    /// The boundary still owes the caller its one diagnostic line.
    Unreported(Error),
}

impl From<Error> for InspectionFailure {
    fn from(error: Error) -> Self {
        Self::Unreported(error)
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
            message_prefix_bytes: b"qpdf".to_vec(),
            warnings: false,
            suppress_warnings: false,
            warnings_exit_zero: false,
            progress_handler: None,
            configuration: qpdf_default_job_configuration(),
            page_source_documents: Vec::new(),
            overlay_sources: Vec::new(),
            primary_copy_encryption: None,
            encryption_status: EncryptionStatus::default(),
            create_qpdf_succeeded_without_document: false,
            empty_primary_created: false,
            partial_json_initialized: false,
            has_run: false,
            argv_early_exit: false,
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
        let message_prefix = message_prefix.into();
        self.message_prefix_bytes = message_prefix.as_bytes().to_vec();
        self.message_prefix = message_prefix;
    }

    /// Set the qpdf diagnostic prefix from its original raw argv bytes.
    ///
    /// qpdf stores the first argv element in a std::string and emits those bytes without
    /// UTF-8 replacement (QUtil.cc:788-803, QPDFJob_argv.cc:427-428).
    /// Keep the lossy string projection for the existing text getter while
    /// preserving the source bytes for logger and pipeline output.
    pub(crate) fn set_message_prefix_bytes(&mut self, message_prefix: Vec<u8>) {
        self.message_prefix = String::from_utf8_lossy(&message_prefix).into_owned();
        self.message_prefix_bytes = message_prefix;
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

    /// Return the greatest PDF version observed on a source opened by this job.
    ///
    /// qpdf accumulates `max_input_version` while `doProcessOnce` opens each
    /// input (`libqpdf/QPDFJob.cc:1695-1716`) and applies it to the writer in
    /// `setWriterOptions` (`libqpdf/QPDFJob.cc:2847-2918`). A caller that owns
    /// the final writer outside [`Self::write_qpdf`] can use this snapshot to
    /// preserve that same floor without reopening a source document.
    #[must_use]
    pub fn input_version_floor(&self) -> Option<PdfVersion> {
        let (version, extension_level) = self.configuration.max_input_version.as_ref()?;
        let version = crate::parse_pdf_version(version)?;
        Some(PdfVersion::new(
            version.major(),
            version.minor(),
            *extension_level,
        ))
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

    /// Replace the writer settings carried by this job's qpdf configuration.
    ///
    /// The CLI and job-JSON paths use the same `WriterConfiguration` shape;
    /// keeping the assignment on `QPDFJob` lets the later `write_qpdf` stage
    /// remain the sole writer owner (`QPDFJob.cc:2846-2937`).
    pub fn set_writer_configuration(&mut self, configuration: WriterConfiguration) {
        self.configuration.writer = configuration;
    }

    /// Apply qpdf's write-time password and weak-crypto checks to one writer
    /// configuration.
    ///
    /// Ordinary output calls this immediately before `QPDFWriter::write`.
    /// Split output calls it from the first chunk's writer setup instead:
    /// qpdf's `doSplitPages` performs the resource/page-copy work first and
    /// invokes `setWriterOptions` only after that work
    /// (`QPDFJob.cc:2939-3027`). Keeping this helper separate lets both
    /// branches preserve that observable diagnostic order.
    pub(crate) fn prepare_writer_configuration(
        &self,
        writer_configuration: &mut WriterConfiguration,
    ) -> Result<()> {
        let auto_password_notices = writer_configuration
            .normalize_encryption_passwords(self.configuration.password_mode)?;
        for notice in auto_password_notices {
            match notice {
                crate::encryption::PasswordWriteNotice::Info if self.configuration.verbose => {
                    self.logger.info(self.prefixed_message(
                        b"automatically converting Unicode password to single-byte encoding as required for 40-bit or 128-bit encryption\n",
                    ))?;
                }
                crate::encryption::PasswordWriteNotice::Warning => {
                    self.logger.error(self.prefixed_message(
                        b"WARNING: supplied password looks like a Unicode password with characters not allowed in passwords for 40-bit and 128-bit encryption; most readers will not be able to open this file with the supplied password. (Use --password-mode=bytes to suppress this warning and use the password anyway.)\n",
                    ))?;
                }
                crate::encryption::PasswordWriteNotice::None
                | crate::encryption::PasswordWriteNotice::Info => {}
            }
        }
        if self
            .configuration
            .encryption_defaults
            .accessibility_disabled
            && writer_configuration
                .encryption_parameters()
                .is_some_and(|params| {
                    // cov:ignore-start: modern accessibility warning regression executes this predicate; LLVM maps the covered match continuation elsewhere
                    matches!(
                        params.method,
                        EncryptMethod::V4Aes128
                            | EncryptMethod::V4Rc4128
                            | EncryptMethod::V5R5Aes256
                            | EncryptMethod::V5R6Aes256
                    )
                    // cov:ignore-end
                })
        {
            self.logger.error(self.prefixed_message(
                b"-accessibility=n is ignored for modern encryption formats\n",
            ))?; // cov:ignore: accessibility warning regression executes the logger write; LLVM maps the covered continuation to the format call
        }
        if !self.configuration.allow_weak_crypto
            && writer_configuration
                .encryption_parameters()
                .is_some_and(EncryptParams::is_weak_rc4)
        {
            let message = self.prefixed_message(
                b"refusing to write a file with RC4, a weak cryptographic algorithm\nPlease use 256-bit keys for better security.\nPass --allow-weak-crypto to enable writing insecure files.\nSee also https://qpdf.readthedocs.io/en/stable/weak-crypto.html\n",
            );
            self.logger.error(message)?;
            return Err(Error::System(
                "refusing to write a file with weak crypto".to_string(),
            ));
        }
        Ok(())
    }

    /// Set qpdf's job-level content-normalization flag.
    ///
    /// `QPDFJob::Config::normalizeContent` stores this separately from the
    /// writer object. `doShowObj` consumes it while selecting its output pipe,
    /// and `setWriterOptions` later copies the same flag into a writer
    /// (`libqpdf/QPDFJob_config.cc:414-418`, `libqpdf/QPDFJob.cc:2862`).
    pub fn set_content_normalization(&mut self, value: bool) {
        self.configuration.normalize_content = Some(value);
    }

    /// Return the job-level content-normalization flag used by inspection.
    pub(crate) fn content_normalization_enabled(&self) -> bool {
        self.configuration.normalize_content.unwrap_or(false)
    }

    /// Set qpdf's recovery policy for documents opened by this job.
    pub fn set_suppress_recovery(&mut self, value: bool) {
        self.configuration.suppress_recovery = value;
    }

    /// Set qpdf's document-wide `ignoreXRefStreams` policy for job-owned
    /// primary and secondary sources.
    pub fn set_ignore_xref_streams(&mut self, value: bool) {
        self.configuration.ignore_xref_streams = value;
    }

    /// Set qpdf's `--password-mode` interpretation for job-owned opens.
    pub fn set_password_mode(&mut self, value: PasswordMode) {
        self.configuration.password_mode = value;
    }

    /// Set qpdf's `--password-is-hex-key` policy for job-owned opens.
    pub fn set_password_is_hex_key(&mut self, value: bool) {
        self.configuration.password_is_hex_key = value;
    }

    /// Set qpdf's `--suppress-password-recovery` policy for job-owned opens.
    pub fn set_suppress_password_recovery(&mut self, value: bool) {
        self.configuration.suppress_password_recovery = value;
    }

    /// Set qpdf's `--allow-weak-crypto` policy for the write-time RC4
    /// refusal check (`QPDFJob::setEncryptionOptions`, `QPDFJob.cc:2752-2761`).
    pub fn set_allow_weak_crypto(&mut self, value: bool) {
        self.configuration.allow_weak_crypto = value;
    }

    /// Configure qpdf's linearization writer mode and optional pass-one file.
    pub fn set_linearization(&mut self, value: bool, pass1: Option<PathBuf>) {
        self.configuration.linearize = value;
        self.configuration.linearize_pass1 = pass1;
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

    /// Return the configured qpdf object-stream mode for page selection.
    #[must_use]
    pub(crate) fn object_stream_mode_for_page_specs(&self) -> crate::ObjectStreamMode {
        self.configuration.writer.object_stream_mode()
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
        let mut message = self.message_prefix_bytes().to_vec();
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
        let mut message = self.message_prefix_bytes.clone();
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

    /// Return the raw qpdf diagnostic prefix bytes.
    #[must_use]
    pub(crate) fn message_prefix_bytes(&self) -> &[u8] {
        &self.message_prefix_bytes
    }

    /// Prefix a qpdf diagnostic body without converting the prefix through
    /// UTF-8.
    pub(crate) fn prefixed_message(&self, body: &[u8]) -> Vec<u8> {
        let mut message = self.message_prefix_bytes.clone();
        message.extend_from_slice(b": ");
        message.extend_from_slice(body);
        message
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
                let prefix = self.message_prefix_bytes.clone();
                // `writeOutfile` has already swapped the output name to the
                // `.~qpdf-temp#` replacement target, or cleared it for
                // standard output, before `setWriterOptions` builds this
                // reporter (`libqpdf/QPDFJob.cc:3031-3041` then `:2926-2935`),
                // so the label is the effective destination.
                let output_name = self.configuration.output_file.as_ref().map_or_else(
                    || "standard output".to_owned(),
                    |path| path.display().to_string(),
                );
                let callback: ProgressHandler = Box::new(move |percent| {
                    let mut message = prefix.clone();
                    message.extend_from_slice(b": ");
                    message.extend_from_slice(output_name.as_bytes());
                    message.extend_from_slice(b": write progress: ");
                    message.extend_from_slice(percent.to_string().as_bytes());
                    message.extend_from_slice(b"%\n");
                    logger.info(message)
                });
                Rc::new(RefCell::new(callback))
            }
            None => return,
        };
        writer
            .register_progress_reporter(Box::new(move |percent| (reporter.borrow_mut())(percent)));
    }

    /// Initialize the complete qpdf 11.9.0 argument set supported by this job.
    ///
    /// This is the library-owned counterpart of
    /// `QPDFJob::initializeFromArgv`: all generated main-table options and the
    /// pages, encryption, underlay/overlay, attachment, copy-attachment, and
    /// page-label option tables mutate this job's one `JobConfiguration`.
    /// Native flpdf subcommands remain a separate CLI surface.
    ///
    /// The argv grammar matches `QPDFArgParser::parseArgs`
    /// (`QPDFArgParser.cc:429-560`): a one-level `@file` expansion runs
    /// before any option is inspected, and a top-level `--` resets to the
    /// main option table rather than ending option parsing (an option
    /// spelled after it is still recognized).
    ///
    /// # Errors
    ///
    /// Propagates [`Error::FileIo`] when an `@file` argument (or `@-` reading
    /// stdin) opens successfully but cannot be read to completion, and the
    /// same [`Error::Usage`] arms this function already raises for its other
    /// qpdf-compatible options.
    pub fn initialize_from_argv(&mut self, argv: &[String]) -> Result<()> {
        let raw = argv
            .iter()
            .map(|argument| argument.as_bytes().to_vec())
            .collect::<Vec<_>>();
        self.initialize_from_raw_argv(&raw)
    }

    /// Initialize the job from qpdf's raw argv byte boundary.
    ///
    /// qpdf's `QPDFArgParser` receives `char const*` values and passes the
    /// original bytes directly to `QPDFJob::Config` callbacks. Keeping this
    /// entry point generic over `AsRef<[u8]>` lets Unix callers supply literal
    /// non-UTF-8 paths and passwords while the existing [`Self::initialize_from_argv`]
    /// remains a checked UTF-8 convenience wrapper.
    ///
    /// The parser owns the complete qpdf 11.9.0 option-table grammar. Native
    /// flpdf subcommands are intentionally outside this library boundary.
    pub fn initialize_from_raw_argv<A: AsRef<[u8]>>(&mut self, argv: &[A]) -> Result<()> {
        let raw = argv
            .iter()
            .map(|argument| argument.as_ref().to_vec())
            .collect::<Vec<_>>();
        argv::initialize(self, raw)
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
            message_prefix: self.message_prefix_bytes.clone(),
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
        // qpdf's initializeFromJson configures the existing QPDFJob rather
        // than replacing its state (`QPDFJob_json.cc:611-625`). Partial
        // job-json-file occurrences therefore continue from the same mutable
        // configuration, including settings applied by an earlier argv
        // occurrence.
        let mut configuration = if partial && !self.has_run {
            // qpdf's partial initializer always dispatches into the Config
            // already owned by this QPDFJob. This is what lets an argv
            // occurrence before --job-json-file remain visible when the JSON
            // handler runs (`QPDFJob_json.cc:611-625`).
            let mut configuration = self.configuration.clone();
            if !(configuration.check
                || configuration.show_npages
                || configuration.show_pages
                || configuration.check_linearization
                || configuration.show_xref
                || configuration.show_linearization
                || configuration.show_object.is_some()
                || configuration.list_attachments
                || configuration.show_attachment.is_some()
                || configuration.show_encryption
                || configuration.is_encrypted
                || configuration.requires_password)
            {
                // A newly constructed QPDFJob stores the library's optional
                // output default, but qpdf's partial job-JSON CLI boundary
                // requires an output unless an inspection selector disabled
                // it (`QPDFJob.hh:705`, `QPDFJob.cc:591-595`).
                configuration.require_output = true;
            }
            configuration
        } else if partial && self.partial_json_initialized {
            self.configuration.clone()
        } else {
            let mut configuration = qpdf_default_job_configuration();
            configuration.require_output = true;
            configuration.json_decode_level = crate::writer::DecodeLevel::Generalized;
            configuration.page_specs = self.configuration.page_specs.clone();
            configuration.page_specs_origin = self.configuration.page_specs_origin;
            configuration
        };
        self.dispatch_job_json_document(&mut configuration, &value, &mut BTreeSet::new())?;

        self.configuration = configuration;
        self.partial_json_initialized = partial;
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
                    let mut message = b"error with job-json file ".to_vec();
                    message.extend_from_slice(&path_description_bytes(&path));
                    message.extend_from_slice(b": ");
                    message.extend_from_slice(error.to_string().as_bytes());
                    message.extend_from_slice(b"\nRun ");
                    message.extend_from_slice(self.message_prefix_bytes());
                    message
                        .extend_from_slice(b" --job-json-help for information on the file format.");
                    Error::SystemBytes(message)
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
            configuration.copy_encryption_applies_to_writer = true;
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
            configuration.normalize_content = Some(value == "y");
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
            // qpdf clears only the `copy_encryption` flag; `encryption_file`
            // and its password stay for the page-spec fallback
            // (`QPDFJob_config.cc:155-157`, `QPDFJob.cc:2405-2410`).
            configuration.copy_encryption_applies_to_writer = false;
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
            // qpdf's JSON handler passes `parameter.c_str()` into Config, so
            // JSON input truncates at its first NUL before the private
            // parseRotationParameter receives the value. Direct Config and
            // CLI callers retain the full std::string/argv bytes instead.
            let value = &value[..value
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(value.len())];
            let rotation = parse_rotation_parameter(value)?;
            configuration
                .rotations
                .insert(rotation.range, rotation.spec);
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
                ".jsonKey: unexpected value; expected one of acroform, attachments, encrypt, objectinfo, objects, outlines, pagelabels, pages, qpdf",
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
                configuration
                    .json_objects
                    .push(String::from_utf8_lossy(&item).into_owned());
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
            configuration.copy_encryption_applies_to_writer = false;
            let (params, encryption_defaults) = parse_job_encrypt(
                value,
                configuration.allow_weak_crypto,
                &configuration.encryption_defaults,
            )?; // cov:ignore: llvm-cov attributes this successful encryption parse continuation to its opening expressions
            configuration.encryption_defaults = encryption_defaults;
            configuration.writer.set_encryption_parameters(params);
        }

        if let Some(value) = members.get(b"pages".as_slice()) {
            if configuration.page_specs_origin != PageSpecsOrigin::None {
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
            configuration.page_specs_origin = PageSpecsOrigin::Json;
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
                    password: job_json_string(&item_members, b"password")?,
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
                labels.push(parse_page_label_spec(&label)?);
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
        options.message_prefix = self.message_prefix_bytes.clone();
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
        self.update_writer_version_floor(&mut pdf)?;
        self.record_document_warnings(&pdf);
        Ok(pdf)
    }

    /// Create qpdf's canonical empty document through the same job document
    /// boundary as file and JSON input.
    pub fn create_empty_document(&mut self) -> Result<JobDocument> {
        // qpdf's `Config::emptyInput` uses the empty string as the page-spec
        // source-map key while `QPDF::emptyPDF` names the diagnostic source
        // "empty PDF" (`libqpdf/QPDFJob_config.cc:27-38`;
        // `libqpdf/QPDF.cc:290-293`). Remember the factory outcome for the
        // page-source ordering key without touching the input configuration:
        // `QPDF::emptyPDF` does not configure the job, so a reused job may
        // still be given an input file afterwards.
        self.empty_primary_created = true;
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
        if source_index == 0 && (self.configuration.empty_input || self.empty_primary_created) {
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

    /// Finish qpdf's creation boundary for one primary document.
    ///
    /// qpdf performs every document mutation before `createQPDF` returns:
    /// update-JSON, page selection, rotations, underlay/overlay, and the
    /// configured transformations. The later write stage only chooses how to
    /// consume this already-prepared document
    /// (`libqpdf/QPDFJob.cc:428-481`).
    fn finish_created_document(&mut self, mut pdf: JobDocument) -> Result<JobDocument> {
        self.page_source_documents.clear();
        self.overlay_sources.clear();
        self.primary_copy_encryption = None;
        self.encryption_status = EncryptionStatus {
            encrypted: pdf.is_encrypted(),
            password_incorrect: false,
        };

        let configuration = self.configuration.clone();
        // qpdf leaves createQPDF before any stage when the job only reports
        // encryption status: `if (m->check_is_encrypted ||
        // m->check_requires_password) { return nullptr; }`
        // (`libqpdf/QPDFJob.cc:455-456`) sits ahead of `updateFromJSON`
        // (`:462`) and `handleRotations` (`:470`). Running them here would let
        // a status-only job fail on a missing update file and would mutate a
        // document that exists solely to be inspected.
        if configuration.is_encrypted || configuration.requires_password {
            return Ok(pdf);
        }
        if let Some(update_path) = configuration.update_from_json.as_deref() {
            let update_file = File::open(update_path).map_err(|error| {
                Error::file_io("open update JSON", update_path.to_path_buf(), error)
            })?;
            self.update_from_json(
                &mut pdf,
                BufReader::new(update_file),
                path_description_bytes(update_path),
            )?; // cov:ignore: llvm-cov attributes the successful update continuation to the parser call's opening expressions
        }
        self.prepare_document(pdf, &configuration)
    }

    /// Apply qpdf's create-stage page operation and document transformations.
    ///
    /// Multi-source page selection returns a fresh target, while the
    /// one-source case keeps qpdf's in-place page identity. Secondary page
    /// documents and overlay donors are retained on `self` because flpdf's
    /// canonical foreign copier may defer provider-backed stream reads until
    /// the later `write_qpdf` call.
    fn prepare_document(
        &mut self,
        primary: JobDocument,
        configuration: &JobConfiguration,
    ) -> Result<JobDocument> {
        if configuration.page_specs.is_empty() {
            let mut primary = primary;
            self.apply_configured_rotations(&mut primary, configuration)?;
            self.prepare_document_transformations(&mut primary, configuration)?;
            return Ok(primary);
        }

        let mut page_sources = vec![primary];
        let mut source_paths: Vec<PathBuf> = Vec::new();
        let mut source_passwords: Vec<Option<Vec<u8>>> = Vec::new();
        let mut specs = Vec::with_capacity(configuration.page_specs.len());
        for page in &configuration.page_specs {
            let source_index = if page.path == Path::new(".")
                || self.configuration.input_file.as_deref() == Some(page.path.as_path())
            {
                0
            } else if let Some(index) = source_paths.iter().position(|path| *path == page.path) {
                index + 1
            } else {
                source_paths.push(page.path.clone());
                let password = page.password.clone().or_else(|| {
                    // qpdf substitutes the encryption-file password only for
                    // a null page-spec password and an exact filename match
                    // (`QPDFJob.cc:2396-2410`). An explicit empty page
                    // password remains `Some(Vec::new())` and must bypass the
                    // fallback.
                    // qpdf compares the raw filename strings and never a
                    // normalized path — `page_spec.filename` and
                    // `m->encryption_file` are both `std::string`, and
                    // `QPDFJob.cc:2397` says "Do not canonicalize the file
                    // name." `Path` equality folds away `.` components and
                    // repeated separators, so compare the `OsStr` bytes.
                    (configuration
                        .copy_encryption
                        .as_deref()
                        .map(Path::as_os_str)
                        == Some(page.path.as_os_str()))
                    .then(|| configuration.encryption_file_password.clone())
                });
                source_passwords.push(password);
                source_paths.len()
            };
            specs.push(PageSpecInput::new(source_index, page.range.clone()));
        }
        let keep_files_open = self.keep_files_open_for_page_specs(&specs);
        self.report_page_spec_selection(&specs)?;
        for (path, password) in source_paths.iter().zip(source_passwords.iter()) {
            self.report_page_source_processing(path_description_bytes(path))?;
            let mut source = self.open_job_source(path, password.as_deref().unwrap_or_default())?;
            // qpdf's doProcess marks every page source as an input document,
            // so its version contributes to max_input_version before the
            // writer stage. The copy-encryption donor is opened separately
            // with used_for_input=false and is intentionally not updated here
            // (`QPDFJob.cc:2396-2428,2891-2899`).
            self.update_writer_version_floor(&mut source)?;
            self.record_document_warnings(&source);
            if !keep_files_open {
                // qpdf calls ClosedFileInputSource::stayOpen(false)
                // immediately after processInputSource, before opening the
                // next distinct page source (`QPDFJob.cc:2414-2432`).
                source.set_input_source_stay_open(false);
            }
            page_sources.push(source);
        }

        // qpdf's page-operation target is fresh when more than one distinct
        // source participates. In that case the target no longer carries the
        // primary's `/Encrypt` state, but `writeQPDF` still preserves the
        // authenticated primary encryption unless a writer option disables
        // it. Snapshot the source while the primary document is still live;
        // the later writer stage owns the precedence decision.
        let primary_copy_encryption = if page_sources.len() > 1 {
            page_sources[0].writer_copy_encryption_source()?
        } else {
            None
        };

        if page_sources.len() == 1 && specs.iter().all(|spec| spec.source_index == 0) {
            {
                let page_output = self.handle_page_specs(
                    &mut page_sources,
                    &specs,
                    configuration.collate.as_deref(),
                    configuration.remove_unreferenced_resources,
                    configuration.writer.preserves_unreferenced_objects(),
                )?; // cov:ignore: this successful in-place page selection continuation is covered by the public lifecycle tests
                match page_output {
                    PageSpecJobOutput::InPlace {
                        pdf,
                        result,
                        prune_mode,
                    } => {
                        QPDFJob::complete_in_place_page_selection(pdf, &result, prune_mode)?;
                    }
                    PageSpecJobOutput::Merged(_) => {
                        // cov:ignore-start: the method's single-source
                        // predicate guarantees the in-place variant.
                        return Err(Error::Internal(
                            "single-source page selection returned a merged target".to_owned(),
                        ));
                        // cov:ignore-end
                    }
                }
            }
            let mut primary = page_sources
                .pop()
                .ok_or_else(|| Error::Internal("page selection lost its primary".to_owned()))?;
            self.apply_configured_rotations(&mut primary, configuration)?;
            self.prepare_document_transformations(&mut primary, configuration)?;
            return Ok(primary);
        }

        let target = self.create_page_selection_target()?;
        let page_output = self.handle_page_specs_with_target(
            &mut page_sources,
            &specs,
            configuration.collate.as_deref(),
            configuration.remove_unreferenced_resources,
            configuration.writer.preserves_unreferenced_objects(),
            target,
        )?; // cov:ignore: llvm-cov attributes this covered multi-source call continuation to the opening expression
        let mut primary = match page_output {
            PageSpecJobOutput::Merged(merged) => {
                // qpdf's page_heap is destroyed when createQPDF returns
                // (`QPDF.cc:465-480`). Disconnect the source object graphs at
                // that same boundary so direct values copied into the fresh
                // target retain qpdf's destroyed-owner behavior at write time.
                // Keep the erased Pdf wrappers only for the replace-input
                // close boundary; file-backed foreign streams already capture
                // their input and stream metadata in the canonical provider.
                for source in page_sources.iter().skip(1) {
                    source.resolver.disconnect_all();
                }
                self.page_source_documents = page_sources;
                self.primary_copy_encryption = primary_copy_encryption;
                *merged
            }
            PageSpecJobOutput::InPlace { .. } => {
                // cov:ignore-start: a multi-source request always selects the
                // caller-provided merged target.
                return Err(Error::Internal(
                    "multi-source page selection returned an in-place target".to_owned(),
                ));
                // cov:ignore-end
            }
        };
        self.apply_configured_rotations(&mut primary, configuration)?;
        self.prepare_document_transformations(&mut primary, configuration)?;
        Ok(primary)
    }

    /// Create the empty target used by qpdf's multi-source page merge without
    /// changing the job's configured primary input state.
    fn create_page_selection_target(&self) -> Result<JobDocument> {
        let mut options = self.configured_open_options(Vec::new());
        options.logger = Some(self.logger.clone());
        options.suppress_warnings = self.suppress_warnings;
        options.description = b"empty PDF".to_vec();
        crate::engine::open_empty_with_options_erased(options)
    }

    /// Create the configured input document, returning `None` after qpdf-style
    /// error reporting for a missing or malformed input.
    pub fn create_qpdf(&mut self) -> Result<Option<JobDocument>> {
        self.create_qpdf_succeeded_without_document = false;
        match self.check_configuration() {
            Ok(()) => {}
            Err(error @ Error::Usage(_)) => return Err(error),
            Err(error) => {
                self.report_job_error(&error)?;
                return Ok(None);
            }
        }
        if self.configuration.empty_input {
            let pdf = self.create_empty_document()?;
            return match self.finish_created_document(pdf) {
                Ok(pdf) => Ok(Some(pdf)),
                Err(error @ Error::Usage(_)) => Err(error),
                Err(error) => {
                    self.report_job_error(&error)?;
                    Ok(None)
                }
            };
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
                Ok(pdf) => match self.finish_created_document(pdf) {
                    Ok(pdf) => Ok(Some(pdf)),
                    Err(error @ Error::Usage(_)) => Err(error),
                    Err(error) => {
                        self.report_job_error(&error)?;
                        Ok(None)
                    }
                },
                Err(error) => {
                    self.report_job_error(&error)?;
                    Ok(None)
                }
            };
        }
        let input_name = path_description_bytes(&input);
        let needs_encryption_inspection_open = self.configuration.show_encryption
            || self.configuration.is_encrypted
            || self.configuration.requires_password;
        let open_result: Result<JobDocument> = if needs_encryption_inspection_open {
            // qpdf's createQPDF keeps the partially initialized QPDF when the
            // password handler throws for show-encryption and status queries
            // (`libqpdf/QPDFJob.cc:432-448`). Use the existing inspection
            // opener here so direct `create_qpdf` callers retain that parsed
            // encryption state instead of converting it into the ordinary
            // invalid-password error.
            let source: Box<dyn ReadSeek> = Box::new(BufReader::new(file));
            self.open_for_encryption_inspection_with_description(
                source,
                &input_name,
                self.configured_open_options(self.configuration.password.clone()),
            )
        } else {
            self.open_document_with_description(
                BufReader::new(file),
                &input_name,
                self.configured_open_options(self.configuration.password.clone()),
            )
        };
        match open_result {
            Ok(mut pdf)
                if needs_encryption_inspection_open
                    && pdf.is_encrypted()
                    && pdf.encryption_file_key().is_none() =>
            {
                // qpdf evaluates encryption-status queries before its
                // show-encryption fallback in the password-error catch
                // (`libqpdf/QPDFJob.cc:436-448`). Record the status and skip
                // the report when both kinds of inspection are configured.
                if self.configuration.is_encrypted || self.configuration.requires_password {
                    self.encryption_status = EncryptionStatus {
                        encrypted: true,
                        password_incorrect: true,
                    };
                } else {
                    // qpdf reports the encryption parameters from the partial
                    // document and returns nullptr before update/page
                    // transformations or write/inspection continuation.
                    self.show_encryption(&mut pdf, self.configuration.password_is_hex_key)?;
                }
                self.create_qpdf_succeeded_without_document = true;
                Ok(None)
            }
            Ok(pdf) => match self.finish_created_document(pdf) {
                Ok(pdf) => Ok(Some(pdf)),
                // A usage error belongs to qpdf's `QPDFUsage` path, which the
                // CLI renders through `usageExit` (`qpdf/qpdf.cc:12-22,37-38`).
                // Reporting it here would print the bare message instead, so
                // propagate it the way `check_configuration` already does.
                Err(error @ Error::Usage(_)) => Err(error),
                Err(error) => {
                    self.report_job_error(&error)?;
                    Ok(None)
                }
            },
            Err(error) => {
                self.report_job_error(&error)?;
                Ok(None)
            }
        }
    }

    /// Write a created document through the configured qpdf writer and
    /// complete the shared warning/status boundary.
    pub fn write_qpdf<R>(&mut self, pdf: &mut Pdf<R>) -> Result<()>
    where
        R: Read + Seek + 'static,
    {
        if !self.creates_output() {
            let configuration = self.configuration.clone();
            if let Err(failure) = self.run_configured_inspection(pdf, &configuration) {
                let error = match failure {
                    InspectionFailure::Reported(error) => error,
                    InspectionFailure::Unreported(error) => {
                        self.report_job_error(&error)?;
                        error
                    }
                };
                return Err(error);
            }
            self.drain_document_warnings(pdf);
            self.complete(self.creates_output())?;
            if self.configuration.report_memory_usage {
                self.report_memory_usage()?;
            }
            return Ok(());
        }

        // Reserving again here is a no-op once `apply_transformations` has
        // done it, matching qpdf's own second call, which its comment calls
        // "defensive and harmless" (`QPDFJob.cc:3051-3053`). It still matters
        // for callers that reach write_qpdf without the create stage.
        self.reserve_standard_output()?;
        let splitting = self.configuration.split_pages.is_some_and(|size| size != 0);
        if !splitting {
            // `writeOutfile` rewrites the output name before it opens the
            // destination (`libqpdf/QPDFJob.cc:3031-3041`): `--replace-input`
            // writes to a temporary sibling of the input, and an explicit `-`
            // becomes "no output name" so the writer takes the save pipeline.
            // `doSplitPages` is the other dispatch arm and never reaches this,
            // so it keeps the configured name for its own chunk expansion.
            if self.configuration.replace_input {
                self.configuration.output_file = Some(
                    self.replace_input_path()
                        .expect("--replace-input requires a configured input file"),
                );
            } else if self.configuration.output_file.as_deref() == Some(Path::new("-")) {
                self.configuration.output_file = None;
            }
        }
        let mut writer_configuration = self.configuration.writer.clone();
        // qpdf keeps `normalizeContent` on the job and reapplies it from
        // `setWriterOptions` after the writer configuration has been
        // assembled. Preserve that ownership even when a caller replaces the
        // portable writer configuration after setting the job option
        // (`QPDFJob.cc:2847-2863`).
        if let Some(value) = self.configuration.normalize_content {
            writer_configuration.set_content_normalization(value);
        }
        // qpdf's setWriterOptions applies the accumulated input floor to the
        // writer here, in the write stage (`QPDFJob.cc:2913`), so a source
        // opened during the create stage still raises the output version.
        if let Some((version, extension_level)) = self.configuration.max_input_version.clone() {
            writer_configuration.set_minimum_pdf_version(version, extension_level);
        }
        let writer_donor = self
            .configuration
            .copy_encryption_applies_to_writer
            .then(|| self.configuration.copy_encryption.clone())
            .flatten();
        if let Some(path) = writer_donor {
            match self.copy_encryption_source(&path) {
                Ok(Some(source)) => writer_configuration.copy_encryption_parameters(source),
                Ok(None) => {
                    // qpdf's QPDFWriter::copyEncryptionParameters clears
                    // implicit source-encryption preservation even when the
                    // donor has no `/Encrypt` key (`QPDFWriter.cc:651-658`).
                    writer_configuration.set_preserve_encryption(false);
                }
                Err(error) => {
                    self.report_job_error(&error)?;
                    return Err(error);
                }
            }
        }
        if self.configuration.copy_encryption.is_none()
            && !pdf.is_encrypted()
            && !splitting
            && writer_configuration.can_preserve_encryption()
        {
            if let Some(source) = self.primary_copy_encryption.take() {
                // The explicit donor above wins when configured. This branch
                // is qpdf's implicit primary-encryption preservation for a
                // fresh multi-source page-operation target.
                writer_configuration.copy_encryption_parameters(source);
            }
        }
        writer_configuration.set_linearization(self.configuration.linearize);
        if let Some(path) = self.configuration.linearize_pass1.as_deref() {
            writer_configuration.set_linearization_pass1_filename(path.to_path_buf());
        }
        let progress_requested = self.configuration.progress;
        let write_result: Result<()> =
            if let Some(split_pages) = self.configuration.split_pages.filter(|size| *size != 0) {
                // Keep the signed qpdf value until the split implementation has
                // reached the same page-boundary conversion as
                // `QIntC::to_size(m->split_pages)` (`libqpdf/QPDFJob.cc:2970`).
                let split_output = self
                    .configuration
                    .output_file
                    .clone()
                    .expect("--split-pages requires a configured output file");
                let mut split_options = SplitPageOptions::new(1, split_output)
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
            } else if self.configuration.json_version.is_some() {
                let configuration = self.configuration.clone();
                self.write_configured_json(pdf, &configuration)
            } else {
                (|| {
                    let mut writer = PdfWriter::new(pdf);
                    // The rewritten output name decides the destination:
                    // `saveToStandardOutput` was already called, but qpdf
                    // repeats it here because it is "defensive and harmless"
                    // (`libqpdf/QPDFJob.cc:3044-3056`).
                    if let Some(path) = self.configuration.output_file.clone() {
                        writer.set_output_file(&path)?;
                    } else {
                        self.logger.save_to_standard_output(true)?;
                        writer.set_output_pipeline(JobOutputPipeline(self.logger.get_save()?))?;
                    }
                    // qpdf opens the destination before applying
                    // setWriterOptions (`QPDFJob.cc:3049-3056`). Keep
                    // password normalization and weak-crypto validation after
                    // that open so an unusable output path wins over a
                    // write-time encryption refusal.
                    self.prepare_writer_configuration(&mut writer_configuration)?;
                    writer_configuration.apply_to(&mut writer);
                    if progress_requested {
                        self.configure_writer_progress(&mut writer);
                    }
                    writer.write()
                })()
            };
        match write_result {
            Ok(()) => {
                // qpdf reads the document's warnings without draining them
                // inside `writeOutfile` — `bool warnings = pdf.anyWarnings()`
                // decides the backup name just before the rename
                // (`libqpdf/QPDFJob.cc:3071`) — and only drains afterwards, in
                // `writeQPDF`, where `!pdf.getWarnings().empty()` sets the
                // job's own flag (`:493-494`). Draining up front would clear
                // the document's collection before the verbose message and the
                // rename have run, so a failure in either would leave a caller
                // that still holds the `Pdf` with no diagnostics.
                if pdf.any_warnings() {
                    self.record_warnings();
                }
                // qpdf reports the destination it kept in the rewritten output
                // name (`libqpdf/QPDFJob.cc:3058-3062`), which is already
                // absent for standard output; `doSplitPages` reports each
                // chunk from its own loop instead, so the split arm is
                // excluded here.
                if self.configuration.verbose && !splitting {
                    if let Some(name) = self.configuration.output_file.clone() {
                        let mut message = self.message_prefix_bytes.clone();
                        message.extend_from_slice(b": wrote file ");
                        message.extend_from_slice(&path_description_bytes(&name));
                        message.push(b'\n');
                        self.logger.info(message)?;
                    }
                }
                // qpdf clears the output name again once the verbose report
                // has been emitted, so the completion summary below sees only
                // `m->replace_input` (`libqpdf/QPDFJob.cc:3063-3065`).
                if self.configuration.replace_input {
                    self.configuration.output_file = None;
                }
                // qpdf's `writeOutfile` performs the replace-input rename
                // itself, after the verbose message and unconditionally for
                // every caller that reaches it (`libqpdf/QPDFJob.cc:3057-3086`),
                // not only when driven through `run()`. `check_configuration`
                // rejects `--replace-input` together with `--split-pages` or
                // `--json` (`libqpdf/QPDFJob.cc:572-579`, mirrored at
                // `Self::check_configuration`), so this branch and the
                // `splitting`/JSON branches above are mutually exclusive.
                if self.configuration.replace_input {
                    // qpdf closes the input source before renaming because
                    // POSIX/Windows both refuse to rename an open file
                    // reliably (`libqpdf/QPDFJob.cc:3068-3069`).
                    pdf.close_input_source();
                    // qpdf's page_heap donors die when createQPDF returns;
                    // flpdf keeps these documents through write so deferred
                    // foreign streams can still read their providers. Close
                    // every retained donor at the same replace-input boundary
                    // before the original path is renamed, including a donor
                    // that aliases the primary input.
                    for source in &self.page_source_documents {
                        source.close_input_source();
                    }
                    for spec in &self.overlay_sources {
                        spec.source.close_input_source();
                    }
                    // A rename failure escapes qpdf's `writeOutfile` as an
                    // exception and its CLI catch still prints one
                    // `qpdf: <what()>` line (`qpdf/qpdf.cc:39-41`). Report it
                    // here for the same reason the write-failure arm below
                    // does: `run()` turns the error into an exit status and
                    // would otherwise discard the explanation entirely.
                    // cov:ignore-start: reaching this arm needs the rename
                    // itself to fail after a successful write, which requires
                    // a filesystem-level failure (permissions revoked between
                    // write and rename, or a still-open handle on Windows)
                    // that cannot be induced in-process on Linux CI
                    if let Err(error) = self.finish_replace_input() {
                        self.report_job_error(&error)?;
                        return Err(error);
                    }
                    // cov:ignore-end
                }
                // The drain qpdf performs after `writeOutfile` returns
                // (`libqpdf/QPDFJob.cc:493-494`).
                self.drain_document_warnings(pdf);
                // The same predicate `writeQPDF` dispatched on, re-queried
                // after `writeOutfile`'s rewrites (`libqpdf/QPDFJob.cc:497`).
                // Standard output is an output destination for dispatch but
                // not a resulting file for the warning suffix, because the
                // name has been cleared in between.
                self.complete(self.creates_output())?;
                if self.configuration.report_memory_usage {
                    self.report_memory_usage()?;
                }
                Ok(())
            }
            // A usage error belongs to qpdf's `QPDFUsage` path: `writeJSON`
            // calls `usage()` for a stdout destination that cannot name its
            // stream side files (`libqpdf/QPDFJob.cc:3105-3110`), and that
            // exception passes straight through `writeOutfile`/`writeQPDF` to
            // the CLI's `usageExit` (`qpdf/qpdf.cc:37-38`). Reporting it here
            // would print the bare message instead of qpdf's usage block, the
            // same reason `create_qpdf` propagates it unreported.
            Err(error @ Error::Usage(_)) => Err(error),
            Err(error) => {
                self.report_job_error(&error)?;
                Err(error)
            }
        }
    }

    /// Apply the configured qpdf document transformations to an already-open
    /// primary document.
    ///
    /// `QPDFJob::createQPDF` normally owns this stage before `writeQPDF`
    /// (`libqpdf/QPDFJob.cc:428-481`). The public job boundary lets the CLI
    /// migrate one existing opened-document consumer at a time without
    /// reimplementing `handleTransformations` beside the job lifecycle.
    pub fn apply_transformations<R>(&mut self, pdf: &mut Pdf<R>) -> Result<()>
    where
        R: Read + Seek + 'static,
    {
        // qpdf reserves standard output inside `checkConfiguration`, which
        // `createQPDF` runs before any transformation
        // (`libqpdf/QPDFJob.cc:428-431,614-626`). Doing it here keeps verbose
        // and diagnostic output raised by the transformations on standard
        // error instead of consuming the stream the PDF itself needs.
        self.reserve_standard_output()?;
        let configuration = self.configuration.clone();
        self.apply_configured_rotations(pdf, &configuration)?;
        self.prepare_document_transformations(pdf, &configuration)
    }

    /// Reserve the save pipeline when this job writes to standard output.
    ///
    /// `only_if_not_set` makes repeated calls idempotent, so the create and
    /// write stages can both reserve without the second one failing.
    fn reserve_standard_output(&mut self) -> Result<()> {
        if self.configuration.output_file.as_deref() != Some(Path::new("-")) {
            return Ok(());
        }
        if let Err(error) = self.logger.save_to_standard_output(true) {
            self.report_job_error(&error)?;
            return Err(error);
        }
        Ok(())
    }

    /// Clear the completed-run flag when a new initialization restarts the
    /// job's lifecycle. qpdf has no such flag: `initializeFromArgv` simply
    /// rebuilds the configuration, so this reset keeps the Rust side's
    /// post-run JSON-layering branch from seeing a stale value.
    pub(super) fn reset_has_run_for_initialization(&mut self) {
        self.has_run = false;
    }

    /// Run the configured create/write or check lifecycle.
    pub fn run(&mut self) -> Result<JobExitCode> {
        if self.argv_early_exit {
            return Ok(JobExitCode::Success);
        }
        self.partial_json_initialized = false;
        self.has_run = true;
        if self.configuration.is_encrypted || self.configuration.requires_password {
            return self.run_encryption_status();
        }
        let Some(mut pdf) = self.create_qpdf()? else {
            return Ok(if self.create_qpdf_succeeded_without_document {
                self.get_exit_code()
            } else {
                JobExitCode::Error
            });
        };

        let status = match self.write_qpdf(&mut pdf) {
            Ok(()) => self.get_exit_code(),
            // qpdf's `QPDFUsage` is not caught by `run()`; it reaches the
            // caller, which renders it through `usageExit`
            // (`qpdf/qpdf.cc:37-38`). Every other write failure has already
            // been reported by `writeQPDF`, so it only contributes its status.
            Err(error @ Error::Usage(_)) => return Err(error),
            Err(_error) => JobExitCode::Error,
        };
        // The replace-input rename is `write_qpdf`'s own responsibility
        // (mirroring qpdf's `writeOutfile`, `libqpdf/QPDFJob.cc:3057-3086`).
        Ok(status)
    }

    /// Run the configured qpdf inspection column on one already-open document.
    ///
    /// This is the report-only form of `write_qpdf`'s no-output branch. It
    /// keeps `run_configured_inspection` as the single owner of qpdf's
    /// independent `doInspection` ordering and emits the warning completion
    /// exactly once after every selected report (`QPDFJob.cc:483-511,
    /// 1646-1693`).
    pub fn inspect_configured<R>(
        &mut self,
        pdf: &mut Pdf<R>,
    ) -> std::result::Result<JobExitCode, super::check::CheckError>
    where
        R: Read + Seek + 'static,
    {
        let configuration = self.configuration.clone();
        match self.run_configured_inspection(pdf, &configuration) {
            Ok(()) => {
                self.drain_document_warnings(pdf);
                self.complete(false)?;
                if self.configuration.report_memory_usage {
                    self.report_memory_usage()?;
                }
                Ok(self.get_exit_code())
            }
            Err(InspectionFailure::Reported(_error)) => {
                Err(super::check::CheckError::ErrorsDetected)
            }
            Err(InspectionFailure::Unreported(error)) => {
                Err(super::check::CheckError::Operation(error))
            }
        }
    }

    fn report_memory_usage(&self) -> Result<()> {
        self.logger.warn(format!(
            "qpdf-max-memory-usage {}\n",
            crate::memory_usage::max_memory_usage()
        ))
    }

    fn run_encryption_status(&mut self) -> Result<JobExitCode> {
        self.check_configuration()?;
        // Clear before the open so a failure below cannot leave the previous
        // document's bits behind. qpdf sets `m->encryption_status` while
        // processing each input and lets an open failure escape `run()` as an
        // exception, so `getExitCode` is never consulted against a stale
        // status (`QPDFJob.cc:1699-1708`, `qpdf/qpdf.cc:39-43`). flpdf turns
        // that failure into `JobExitCode::Error` instead of unwinding, and
        // `get_exit_code` is a public query, so the reset has to be explicit.
        self.encryption_status = EncryptionStatus::default();
        // qpdf's `createQPDF` still creates an empty document for `--empty`
        // before the encryption-status early return (`QPDFJob.cc:429-456,
        // 1699-1708`). An empty document is necessarily unencrypted, so both
        // `isEncrypted` and `requiresPassword` return EXIT_IS_NOT_ENCRYPTED
        // (2) without attempting to open an input file.
        if self.configuration.empty_input {
            return Ok(self.get_exit_code());
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
        self.encryption_status = EncryptionStatus {
            encrypted,
            password_incorrect: encrypted && pdf.encryption_file_key().is_none(),
        };
        if self.configuration.is_encrypted {
            return Ok(self.get_exit_code());
        }

        // qpdf's `requiresPassword` uses exit 3 when authentication succeeds,
        // exit 0 when an encrypted document still needs another password, and
        // exit 2 for a plaintext document (`QPDFJob::getExitCode`,
        // `QPDFJob.cc:535-557`). `encryption_file_key` also covers the raw
        // `passwordIsHexKey` path, where user/owner match flags stay false.
        if !encrypted {
            return Ok(self.get_exit_code());
        }
        Ok(self.get_exit_code())
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
        let page_count = crate::qutil::qpdf_size_to_int(page_refs.len())?;
        for (range, rotation) in &configuration.rotations {
            let selected = crate::qutil::parse_numrange(range, page_count)?;
            for page in selected {
                let index = page.wrapping_sub(1);
                if index >= 0 && index < page_count {
                    let mut page = PageObjectHelper::new(page_refs[index as usize], pdf);
                    page.rotate_page(rotation.angle, rotation.relative)?;
                }
            }
        }
        Ok(())
    }

    fn prepare_document_transformations<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<()>
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
            let mut source = self.open_job_source(&overlay.path, &overlay.password)?;
            self.update_writer_version_floor(&mut source)?;
            overlay_specs.push(OverlaySpec {
                source,
                kind: overlay.kind,
                from: overlay.from.clone(),
                to: overlay.to.clone(),
                repeat: overlay.repeat.clone(),
            });
        }
        if configuration.verbose && !overlay_specs.is_empty() {
            let report = overlay_verbose_report(pdf, &mut overlay_specs)?;
            self.report_overlay_progress(&report, configuration)?;
        }
        handle_under_overlay(pdf, &mut overlay_specs)?;
        self.overlay_sources = overlay_specs;

        // qpdf's `handleTransformations` applies `removeRestrictions` after
        // underlay/overlay handling and delegates the mutation to
        // `QPDFAcroFormDocumentHelper::disableDigitalSignatures`
        // (`libqpdf/QPDFJob.cc:2137-2150`). Keep the same document-level
        // /Perms, /SigFlags, and signature-field boundary; do not reuse the
        // CLI's separate rewrite policy.
        if configuration.remove_restrictions {
            let mut acroform = AcroFormDocumentHelper::new(pdf)?;
            acroform.disable_digital_signatures()?;
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
                self.message_prefix_bytes(),
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
                let mut message = b"attachment ".to_vec();
                message.extend_from_slice(key);
                message.extend_from_slice(b" not found");
                return Err(Error::SystemBytes(message));
            }
            if configuration.verbose {
                // The key is arbitrary PDF bytes, not UTF-8. Building the line
                // as bytes keeps a key like `key-\xff` intact; going through
                // `String::from_utf8_lossy` would print U+FFFD where qpdf
                // prints the original byte.
                let mut message = self.message_prefix_bytes.clone();
                message.extend_from_slice(b": removed attachment ");
                message.extend_from_slice(key);
                message.push(b'\n');
                self.logger.info(message)?; // cov:ignore: llvm-cov attributes this successful logger write to its opening expressions
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

        // qpdf copies from every configured donor in one pass and reports the
        // conflicting keys once after the last donor (`QPDFJob.cc:2089-2135`).
        let copy_options = configuration
            .attachments_to_copy
            .iter()
            .map(|copy| AttachmentCopyOptions {
                path: copy.path.clone(),
                password: copy.password.clone(),
                prefix: copy.prefix.clone(),
                verbose: configuration.verbose,
            })
            .collect::<Vec<_>>();
        self.copy_attachments_with_opener(pdf, &copy_options, |job, option| {
            // qpdf's `copyAttachments` lets the donor's `processFile`
            // exception escape directly (`QPDFJob.cc:2100`), so a password
            // failure remains typed here. The CLI adds the donor path at its
            // reporting boundary, while library callers retain the original
            // `Encrypted(BadPassword)` classification.
            job.open_job_source(&option.path, &option.password)
        })?;

        Ok(())
    }

    fn report_overlay_progress(
        &self,
        report: &[OverlayVerbosePage],
        configuration: &JobConfiguration,
    ) -> Result<()> {
        let paths: Vec<&Path> = configuration
            .underlays
            .iter()
            .chain(configuration.overlays.iter())
            .map(|overlay| overlay.path.as_path())
            .collect();
        let mut message = self.message_prefix_bytes.clone();
        message.extend_from_slice(b": processing underlay/overlay\n");
        for page in report {
            message.extend_from_slice(b"  page ");
            message.extend_from_slice(page.dest_page.to_string().as_bytes());
            message.push(b'\n');
            for source in &page.sources {
                let path = paths.get(source.spec_index).ok_or_else(|| {
                    Error::Internal("overlay verbose source index out of range".into())
                })?;
                message.extend_from_slice(b"    ");
                message.extend_from_slice(&path_description_bytes(path));
                message.push(b' ');
                message.extend_from_slice(match source.kind {
                    OverlayKind::Underlay => b"underlay" as &[u8],
                    OverlayKind::Overlay => b"overlay" as &[u8],
                });
                message.push(b' ');
                message.extend_from_slice(source.src_page.to_string().as_bytes());
                message.push(b'\n');
            }
        }
        self.logger.info(message)
    }

    fn run_configured_inspection<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> std::result::Result<(), InspectionFailure>
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
                return match error {
                    // qpdf's `doCheck` throws `std::runtime_error("errors
                    // detected")` (`libqpdf/QPDFJob.cc:793`) and the CLI's
                    // catch prints `qpdf: errors detected` exactly once
                    // (`qpdf/qpdf.cc:39-41`). `run_check_report` has already
                    // written that line (`job/check.rs::report_errors_detected`),
                    // so the boundary must not report it a second time.
                    super::check::CheckError::ErrorsDetected => Err(InspectionFailure::Reported(
                        Error::Unsupported("errors detected".to_owned()),
                    )),
                    super::check::CheckError::Operation(error) => {
                        Err(InspectionFailure::Unreported(error))
                    }
                };
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
        Ok(())
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
        self.drain_document_warnings(pdf);
        self.complete(false)?;
        Ok(self.get_exit_code())
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
        let input_name = path_description_bytes(path);
        let mut options = self.configured_open_options(password.to_vec());
        options.logger = Some(self.logger.clone());
        options.description = input_name.clone();
        // `open_file_with_options` installs the qpdf-shaped reopenable source;
        // keep the job's warning policy on this secondary document exactly as
        // `open_document` does for the primary.
        options.suppress_warnings |= self.suppress_warnings;
        let result = (|| {
            let mut pdf = Pdf::<Box<dyn ReadSeek>>::open_file_with_options(path, options)?;
            pdf.root_handle()?;
            Ok(pdf)
        })();
        if result.is_err() {
            // qpdf reports an opening failure against the source that failed,
            // even though the job's primary input name remains unchanged
            // after a successful donor open. Retain the source name only on
            // this error path so the job-level reporter can render the same
            // path-scoped diagnostic without contaminating later primary
            // errors or duplicate-attachment messages.
            self.set_input_name_bytes(&input_name);
        }
        result
    }

    fn update_writer_version_floor(&mut self, source: &mut JobDocument) -> Result<()> {
        // qpdf's doProcessOnce updates max_input_version for every source
        // document used by the job (`QPDFJob.cc:1695-1716`). The ordinary
        // overlay consumer needs the same floor before the final writer runs.
        let version = source.get_version_as_pdf_version()?;
        let (version, extension_level) = version.get_version();
        crate::writer::update_minimum_pdf_version(
            &mut self.configuration.max_input_version,
            version,
            extension_level,
        );
        Ok(())
    }

    fn copy_encryption_source(
        &mut self,
        path: &Path,
    ) -> Result<Option<crate::CopyEncryptionSource>> {
        let password = self.configuration.encryption_file_password.clone();
        let mut donor = self.open_job_source(path, &password)?;
        self.record_document_warnings(&donor);
        donor.writer_copy_encryption_source()
    }

    fn write_configured_json<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        configuration: &JobConfiguration,
    ) -> Result<()>
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
        // `writeJSON` writes to the output name when one survived
        // `writeOutfile`'s rewrites, and to the save pipeline otherwise
        // (`libqpdf/QPDFJob.cc:3093-3115`).
        if let Some(path) = configuration.output_file.as_deref() {
            // qpdf opens this destination with `QUtil::safe_fopen(..., "w")`
            // (`libqpdf/QPDFJob.cc:3103-3104`), whose failure reads
            // `open <path>: <strerror>` with no operation prefix.
            let mut file = File::create(path)
                .map_err(|error| crate::filespec_helper::qpdf_style_open_error(path, error))?;
            return self
                .write_json_without_completion(
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
        let mut output = JobOutputWriter::new(self.logger.get_save()?);
        let result = self.write_json_without_completion(
            pdf,
            version,
            configuration.test_json_schema,
            configuration.json_output,
            configuration.show_encryption_key,
            options,
            JsonJobOutput::Stdout(&mut output),
        );
        if result.is_err() {
            // A successful serialization already flushed through the JSON
            // pipeline's own `finish`. A failed one has not, and qpdf keeps
            // whatever its destination received before the failure -- its
            // deferred `--json-object` parse can fail after earlier sections
            // reached the output (`libqpdf/QPDFJob.cc:929-997`). A flush
            // failure here cannot replace the error that stopped the write.
            let _ = output.flush_buffer();
        }
        result.map_err(Error::from)
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
            let root = pdf.root_handle()?;
            root.remove_key(b"/PageLabels"); // cov:ignore: the validated Catalog mutation has no qpdf failure branch
        }
        let Some(specs) = configuration.set_page_labels.as_deref() else {
            return Ok(());
        };
        // qpdf guards the whole rebuild on a non-empty spec vector
        // (`if (!m->page_label_specs.empty())`, `libqpdf/QPDFJob.cc:2199`), so
        // an empty set leaves `/PageLabels` as it is instead of installing
        // `<< /Nums [] >>`.
        if specs.is_empty() {
            return Ok(());
        }
        // qpdf's `getRoot()` is the semantic Catalog boundary here: direct and
        // indirect trailer `/Root` values are both valid, while a missing,
        // dangling, or non-dictionary root is a document-level error.
        let root = pdf.root_handle()?;
        let page_count = crate::page_document_helper::PageDocumentHelper::new(pdf)
            .get_all_pages()?
            .len();
        let entries = parse_job_page_labels(specs, page_count)?;
        let mut nums = Vec::with_capacity(entries.len() * 2);
        for (index, spec) in entries {
            nums.push(ObjectHandle::integer(index));
            nums.push(crate::page_label_document_helper::PageLabelDocumentHelper::<R>::page_label_dict_bytes(
                spec.style,
                spec.start,
                &spec.prefix,
            ));
        }
        root.replace_key(
            b"/PageLabels",
            ObjectHandle::dictionary(vec![(b"/Nums".to_vec(), ObjectHandle::array(nums))]),
        )?; // cov:ignore: a validated direct Catalog replacement cannot fail without an impossible concurrent handle mutation
        Ok(())
    }

    fn replace_input_path(&self) -> Option<PathBuf> {
        self.configuration
            .input_file
            .as_ref()
            .map(|path| path_with_suffix(path, ".~qpdf-temp#"))
    }

    /// Complete a `--replace-input` write: rename the original input to a
    /// backup and the temporary output over the original input path.
    ///
    /// This is qpdf's `writeOutfile` replace-input tail
    /// (`libqpdf/QPDFJob.cc:3068-3086`), which keeps the backup (and logs a
    /// message) when `pdf.anyWarnings()` is true at this point, or deletes it
    /// otherwise. The job-level `self.warnings` flag is already updated from
    /// foreign-source `any_warnings` checks and the primary document's
    /// completion-time `get_warnings` drain before this helper is called. That
    /// matches qpdf's `anyWarnings()` decision in `writeOutfile`
    /// (`libqpdf/QPDFJob.cc:3068-3073`) and keeps write-time warnings on the
    /// warning-preserving backup path (`flpdf-3yn9.48.26`).
    ///
    /// qpdf's `writeOutfile` performs this rename for every caller that
    /// reaches it, with no cleanup path if writing itself fails first (the
    /// temporary output, if partially written, is simply left behind when
    /// the exception unwinds past `run()`). flpdf matches that: there is no
    /// corresponding "delete the temporary file" helper for the failure
    /// case.
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
            let mut message = self.message_prefix_bytes.clone();
            message.extend_from_slice(b": there are warnings; original file kept in ");
            message.extend_from_slice(&path_description_bytes(&backup));
            message.push(b'\n');
            self.logger.error(message)?; // cov:ignore: llvm-cov attributes this successful warning logger write to its opening expressions
        } else if let Err(error) = std::fs::remove_file(&backup) {
            // cov:ignore-start: backup deletion failure depends on external filesystem permissions or races
            let mut message = self.message_prefix_bytes.clone();
            message.extend_from_slice(b": unable to delete original file (");
            message.extend_from_slice(error.to_string().as_bytes());
            message.extend_from_slice(b"); original file left in ");
            message.extend_from_slice(&path_description_bytes(&backup));
            message.extend_from_slice(b", but the input was successfully replaced\n");
            self.logger.error(message)?;
            // cov:ignore-end
        }
        Ok(())
    }

    /// Apply qpdf's pre-open output destination and file-identity checks.
    ///
    /// This is the portable subset of `QPDFJob::checkConfiguration`
    /// (`libqpdf/QPDFJob.cc:567-631`): stdout is reserved before the input is
    /// opened, and `QUtil::same_file` rejects destructive aliases before the
    /// writer can truncate them.
    ///
    /// The receiver is mutable because qpdf's own check is not a pure
    /// predicate: it assigns the implicit JSON destination `-` to
    /// `m->outfilename` when `--json` was requested without an output file
    /// (`libqpdf/QPDFJob.cc:582-586`), and every later query of
    /// [`Self::creates_output`] observes that assignment.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Usage`] for each command-line consistency
    /// failure qpdf reports from `checkConfiguration`.
    pub fn check_configuration(&mut self) -> Result<()> {
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
        if self.configuration.json_version.is_some() && self.configuration.output_file.is_none() {
            // The output file is optional with --json for backward
            // compatibility and defaults to standard output
            // (`libqpdf/QPDFJob.cc:582-586`). Every check below, and every
            // later `creates_output` query, sees that assignment.
            self.configuration.output_file = Some(PathBuf::from("-"));
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
            && (self.configuration.output_file.is_some() || self.configuration.replace_input)
        {
            return Err(UsageError::new("no output file may be given for this option").into());
        }
        if self.configuration.is_encrypted && self.configuration.requires_password {
            return Err(UsageError::new(
                "--requires-password and --is-encrypted may not be given together",
            )
            .into());
        }
        if !self.configuration.encryption_defaults.allow_insecure {
            if let Some(params) = self.configuration.writer.encryption_parameters() {
                if matches!(
                    params.method,
                    EncryptMethod::V5R5Aes256 | EncryptMethod::V5R6Aes256
                ) && params.owner_password.is_empty()
                    && !params.user_password.is_empty()
                {
                    return Err(UsageError::new(
                        "A PDF with a non-empty user password and an empty owner password encrypted with a 256-bit key is insecure as it can be opened without a password. If you really want to do this, you must also give the --allow-insecure option before the -- that follows --encrypt.",
                    )
                    .into());
                }
            }
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
            //
            // The destination can be the literal `-` here, either because it
            // was given on the command line or because the JSON default above
            // assigned it. qpdf compares that name with `QUtil::same_file`
            // like any other (`libqpdf/QUtil.cc:574-610`), so a `-` that names
            // no file simply fails to `stat` and the check passes; a file
            // actually named `-` in the working directory is compared as
            // itself and aliasing the input is rejected.
            if !self.configuration.replace_input
                && !self.configuration.split_pages.is_some_and(|size| size != 0)
                && crate::qutil::same_file(input, output)
            {
                return Err(UsageError::new(
                    "input file and output file are the same; use --replace-input to intentionally overwrite the input file",
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
            .write(&self.message_prefix_bytes)
            .map_err(Error::from)?;
        pipeline.write(b": ").map_err(Error::from)?;
        pipeline
            .write(&self.job_error_message_with_input(error))
            .map_err(Error::from)?;
        pipeline.write(b"\n").map_err(Error::from)
    }

    fn job_error_message_with_input(&self, error: &Error) -> Vec<u8> {
        if let Some(message) = error.raw_message() {
            return message.to_vec();
        }
        match error {
            Error::OpenFailure { source, .. } => self.job_error_message_with_input(source),
            Error::Encrypted(crate::EncryptedError::BadPassword)
                if !self.input_name_bytes.is_empty() =>
            {
                let mut rendered = self.input_name_bytes.clone();
                rendered.extend_from_slice(b": invalid password");
                rendered
            }
            // An output-sink failure never reaches this arm: the writer's file
            // sink reports itself as qpdf's `qpdf output` pipeline
            // (`QPDFWriter.cc:101-110`), so a bare `Error::Io` here comes from
            // the input side and keeps the input name qpdf prints for it.
            // A failed open carries the path as a `PathBuf`, so render it
            // through the byte-preserving helper. Falling through to the
            // `Display` formatting below would substitute U+FFFD for any byte
            // that is not valid UTF-8, and qpdf prints the original bytes.
            Error::FileIo {
                operation,
                path,
                source,
            } => {
                let mut rendered = operation.as_bytes().to_vec();
                rendered.push(b' ');
                rendered.extend_from_slice(&path_description_bytes(path));
                rendered.extend_from_slice(b": ");
                rendered.extend_from_slice(qpdf_file_io_source_message(source).as_bytes());
                rendered
            }
            Error::Io(error) if !self.input_name_bytes.is_empty() => {
                let mut rendered = self.input_name_bytes.clone();
                rendered.extend_from_slice(b": ");
                rendered.extend_from_slice(qpdf_file_io_source_message(error).as_bytes());
                rendered
            }
            Error::Parse { offset, message } if !self.input_name_bytes.is_empty() => {
                let mut rendered = self.input_name_bytes.clone();
                rendered.extend_from_slice(b": ");
                if message == "unable to find trailer dictionary while recovering damaged file" {
                    // qpdf's reconstruction terminal error already is a
                    // complete QPDFExc detail (`QPDF.cc:604`); do not add the
                    // Rust parser prefix around that qpdf-shaped message.
                    rendered.extend_from_slice(message.as_bytes());
                } else {
                    // Other parser failures retain flpdf's established CLI
                    // source diagnostic (`error_with_file`), including the
                    // explicit byte offset. They have not crossed the
                    // QPDFExc normalization boundary yet.
                    rendered.extend_from_slice(
                        format!("parse error at byte {offset}: {message}").as_bytes(),
                    );
                }
                rendered
            }
            _ => Self::job_error_message(error),
        }
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
                let source = qpdf_file_io_source_message(source);
                format!("{operation} {}: {source}", path.display()).into_bytes()
            }
            Error::Io(error) => qpdf_file_io_source_message(error).into_bytes(),
            Error::Encrypted(crate::EncryptedError::BadPassword) => b"invalid password".to_vec(),
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
        options.message_prefix = self.message_prefix_bytes.clone();
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
        options.message_prefix = self.message_prefix_bytes.clone();
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
        self.drain_document_warnings(pdf);
        self.complete(false)?;
        Ok(self.get_exit_code())
    }

    /// Serialize one already-created document and then complete the shared
    /// warning/exit-status boundary.
    ///
    /// JSON construction and stream side-file handling remain owned by the
    /// existing serializer; this method owns only the qpdf `writeQPDF`
    /// lifecycle boundary (`QPDFJob.cc:484-563`). The warning-summary
    /// destination is derived from [`JsonJobOutput`] itself, so callers cannot
    /// provide a destination and an inconsistent `creates_output` flag.
    // `pub(crate)`: qpdf's own `QPDFJob::writeJSON` is private
    // (`include/qpdf/QPDFJob.hh:549`), flpdf-cli's `--json` route reaches JSON
    // output through `write_qpdf` rather than calling this method directly
    // (`flpdf-3yn9.48.150.3`), and neither this method nor `write_json` is
    // documented as an intentional library feature in `lib.rs`'s opening doc
    // block, so none of the `pub` grounds in
    // `.claude/rules/qpdf-port-design-patterns.md` section 8 apply. The
    // narrowing leaves this method with no non-test caller (`write_qpdf`'s
    // JSON branch reaches `write_json_without_completion` directly and owns
    // its own completion boundary), matching the existing
    // `#[cfg_attr(not(test), allow(dead_code))]` precedent for other
    // qpdf-mirroring `pub(crate)` methods exercised only by unit tests
    // (`reader/resolver.rs::input_source_name`,
    // `writer/plain/body.rs::emit_content_container_from_handle_with_ref_map`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn write_json<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
    ) -> std::result::Result<JobExitCode, JsonJobError>
    where
        R: Read + Seek,
    {
        self.write_json_with_version(pdf, 2, false, false, false, false, options, output)
    }

    /// Serialize one already-created document with the requested qpdf JSON
    /// version and optional generated-schema validation.
    // Same `pub(crate)` rationale as `write_json` above.
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn write_json_with_version<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        version: i32,
        test_json_schema: bool,
        json_output: bool,
        show_encryption_key: bool,
        verbose: bool,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
    ) -> std::result::Result<JobExitCode, JsonJobError>
    where
        R: Read + Seek,
    {
        // qpdf keeps the output name for its own verbose report;
        // `m->outfilename` is already null for `-` (`QPDFJob.cc:3036-3040`),
        // which the caller mirrors by selecting `JsonJobOutput::Stdout`.
        let output_filename = match &output {
            JsonJobOutput::File { filename, .. } => Some((*filename).to_path_buf()),
            JsonJobOutput::Stdout(_) => None,
        };
        let creates_output = output_filename.is_some();
        self.write_json_without_completion(
            pdf,
            version,
            test_json_schema,
            json_output,
            show_encryption_key,
            options,
            output,
        )?;
        // qpdf reports the written file from inside `writeOutfile`, after
        // `writeJSON` closed the file pipeline and before `writeQPDF` emits the
        // warning summary (`QPDFJob.cc:3042-3062` then `:493-503`), so a merged
        // capture sees `wrote file` first. The raw output bytes are kept
        // because qpdf prints `m->outfilename` itself rather than a lossy
        // rendering of it.
        // The non-JSON writer route gates the same report on
        // `self.configuration.verbose` (see `write_qpdf`), so a library caller
        // that turned verbosity on through `set_verbose` / `config().verbose()`
        // gets the report here too; the argument only adds the CLI's own flag.
        if verbose || self.configuration.verbose {
            if let Some(filename) = output_filename {
                let mut message = self.message_prefix_bytes.clone();
                message.extend_from_slice(b": wrote file ");
                message.extend_from_slice(&path_description_bytes(&filename));
                message.push(b'\n');
                self.logger.info(message)?;
            }
        }
        self.drain_document_warnings(pdf);
        self.complete(creates_output)?;
        Ok(self.get_exit_code())
    }

    /// Serialize JSON without draining warnings or emitting the enclosing
    /// job's completion summary. `writeQPDF` uses this report-independent
    /// operation body before its single shared completion boundary.
    #[allow(clippy::too_many_arguments)]
    fn write_json_without_completion<R>(
        &mut self,
        pdf: &mut Pdf<R>,
        version: i32,
        test_json_schema: bool,
        json_output: bool,
        show_encryption_key: bool,
        options: JsonJobOptions<'_>,
        output: JsonJobOutput<'_>,
    ) -> std::result::Result<(), JsonJobError>
    where
        R: Read + Seek,
    {
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
        Ok(())
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
        if pdf.any_warnings() {
            self.record_warnings();
        }
    }

    /// Drain a document's warnings at the qpdf job completion boundary.
    ///
    /// Intermediate stages use [`Self::record_document_warnings`] so foreign
    /// documents and open-time diagnostics remain available to their later
    /// consumers. qpdf's `writeQPDF` instead drains the live document warning
    /// list after its selected operation (`libqpdf/QPDFJob.cc:483-511`); keep
    /// that ownership transfer explicit here.
    pub(crate) fn drain_document_warnings<R>(&mut self, pdf: &Pdf<R>)
    where
        R: Read + Seek,
    {
        if !pdf.get_warnings().is_empty() {
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

    /// Report whether this job currently creates output.
    ///
    /// This is `QPDFJob::createsOutput` (`libqpdf/QPDFJob.cc:528-531`), a
    /// query over state that the job rewrites while it runs rather than a
    /// fixed property of the configuration:
    ///
    /// * [`Self::check_configuration`] assigns the implicit JSON destination
    ///   `-` when `--json` was requested without an output file;
    /// * the write stage replaces the destination with a temporary path for
    ///   `--replace-input`, and clears it outright when the destination is
    ///   `-`, before it clears the temporary path again after the rename
    ///   (`libqpdf/QPDFJob.cc:3031-3041,3063-3065`).
    ///
    /// The same query therefore answers differently before and after the
    /// write: standard output is an output destination when `writeQPDF`
    /// dispatches, but not when it selects the warning-summary spelling.
    #[must_use]
    pub fn creates_output(&self) -> bool {
        self.configuration.output_file.is_some() || self.configuration.replace_input
    }

    /// Return qpdf's status for the current job state without logging or
    /// draining any document warnings.
    ///
    /// This is the side-effect-free `QPDFJob::getExitCode` query
    /// (`libqpdf/QPDFJob.cc:535-564`). Encryption-status jobs take precedence
    /// over ordinary warning status, exactly as qpdf does.
    #[must_use]
    pub fn get_exit_code(&self) -> JobExitCode {
        if self.configuration.is_encrypted {
            return if self.encryption_status.encrypted {
                JobExitCode::Success
            } else {
                JobExitCode::Error
            };
        }
        if self.configuration.requires_password {
            return if !self.encryption_status.encrypted {
                JobExitCode::Error
            } else if self.encryption_status.password_incorrect {
                JobExitCode::Success
            } else {
                JobExitCode::Warning
            };
        }
        if self.warnings && !self.warnings_exit_zero {
            JobExitCode::Warning
        } else {
            JobExitCode::Success
        }
    }

    /// Complete the shared warning boundary after output or inspection.
    ///
    /// All operation output must be completed by the caller before this
    /// method is invoked. It emits at most the one qpdf-shaped summary; status
    /// is queried separately through [`Self::get_exit_code`]
    /// (`QPDFJob.cc:484-563`).
    pub fn complete(&self, creates_output: bool) -> Result<()> {
        if self.warnings && !self.suppress_warnings {
            let suffix = if creates_output {
                "; resulting file may have some problems"
            } else {
                ""
            };
            let mut message = self.message_prefix_bytes.clone();
            message.extend_from_slice(b": operation succeeded with warnings");
            message.extend_from_slice(suffix.as_bytes());
            message.push(b'\n');
            self.logger.warn(message)?;
        }

        Ok(())
    }
}

/// Render a filesystem error at qpdf's `QPDFSystemError::createWhat` boundary.
///
/// qpdf uses its portable C-runtime spelling for a missing path even on
/// Windows (`QPDFSystemError.cc:13-29`); Rust's Windows `io::Error` display
/// otherwise exposes the native `The system cannot find...` text. Keep the
/// existing native fallback for error kinds that qpdf does not normalize here,
/// while removing Rust's numeric suffix from both forms.
pub(crate) fn qpdf_file_io_source_message(source: &std::io::Error) -> String {
    let message = match source.kind() {
        std::io::ErrorKind::NotFound => Some("No such file or directory"),
        std::io::ErrorKind::PermissionDenied => Some("Permission denied"),
        std::io::ErrorKind::AlreadyExists => Some("File exists"),
        std::io::ErrorKind::InvalidInput => Some("Invalid argument"),
        std::io::ErrorKind::IsADirectory => Some("Is a directory"),
        std::io::ErrorKind::NotADirectory => Some("Not a directory"),
        _ => None,
    };
    if let Some(message) = message {
        return message.to_owned();
    }
    let rendered = source.to_string();
    source
        .raw_os_error()
        .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&rendered)
        .to_owned()
}

impl QPDFJobConfig<'_> {
    /// Configure qpdf's copy-encryption donor and the password used when it
    /// is opened during the write stage.
    ///
    /// `QPDFJob::Config::copyEncryption` stores the donor filename and clears
    /// explicit encryption/decryption state; `writeQPDF` opens the donor only
    /// after `createQPDF` has completed (`QPDFJob.cc:2891-2899`). Keeping the
    /// filename on the job is also required by `handlePageSpecs`, which reuses
    /// `encryptionFilePassword` for a page specification with the same raw
    /// filename and no explicit password (`QPDFJob.cc:2405-2410`).
    pub fn copy_encryption(
        &mut self,
        path: impl Into<PathBuf>,
        password: impl Into<Vec<u8>>,
    ) -> &mut Self {
        self.job.configuration.copy_encryption = Some(path.into());
        self.job.configuration.copy_encryption_applies_to_writer = true;
        self.job.configuration.encryption_file_password = password.into();
        self.job.configuration.writer.clear_encryption_parameters();
        self
    }

    /// Drop the writer side of a previous [`copy_encryption`] call while
    /// keeping the donor filename and password for page-specification
    /// authentication.
    ///
    /// qpdf holds these separately: `--decrypt` and `--encrypt` clear only the
    /// `copy_encryption` flag that gates
    /// `QPDFWriter::copyEncryptionParameters`
    /// (`libqpdf/QPDFJob_config.cc:155-157,1164-1166`, applied at
    /// `QPDFJob.cc:2891-2900`), while `encryption_file` and
    /// `encryption_file_password` are never cleared and remain available to
    /// the page-specification password fallback at `QPDFJob.cc:2405-2410`.
    ///
    /// [`copy_encryption`]: Self::copy_encryption
    pub fn clear_copy_encryption_for_writer(&mut self) -> &mut Self {
        self.job.configuration.copy_encryption_applies_to_writer = false;
        self
    }

    /// Set the primary input filename, rejecting duplicate input selection.
    pub fn input_file(&mut self, input_file: impl Into<PathBuf>) -> Result<&mut Self> {
        self.job.set_input_file(input_file)?;
        Ok(self)
    }

    /// Select qpdf's empty primary input.
    ///
    /// This is `QPDFJob::Config::emptyInput` (`libqpdf/QPDFJob_config.cc:27-40`).
    /// Keep the explicit configuration bit separate from the empty document's
    /// display description: `QPDFJob::create_qpdf` uses it to choose the
    /// `emptyPDF` creation path, while the resulting document is still named
    /// `empty PDF` for diagnostics. Like qpdf, selecting an empty input after
    /// an input file (or selecting it twice) is a usage error.
    pub fn empty_input(&mut self) -> Result<&mut Self> {
        if self.job.configuration.input_file.is_some() || self.job.configuration.empty_input {
            return Err(Error::Usage(UsageError::new(
                "empty input can't be used since input file has already been given",
            )));
        }
        self.job.configuration.empty_input = true;
        Ok(self)
    }

    /// Set the output filename, rejecting duplicate output selection.
    pub fn output_file(&mut self, output_file: impl Into<PathBuf>) -> Result<&mut Self> {
        self.job.set_output_file(output_file)?;
        Ok(self)
    }

    /// Configure qpdf's `replaceInput` output mode.
    pub fn replace_input(&mut self) -> Result<&mut Self> {
        if self.job.configuration.output_file.is_some() || self.job.configuration.replace_input {
            return Err(Error::Usage(UsageError::new(
                "replace-input can't be used since output file has already been given",
            )));
        }
        self.job.configuration.replace_input = true;
        Ok(self)
    }

    /// Configure qpdf's `jsonInput` source mode.
    pub fn json_input(&mut self) -> &mut Self {
        self.job.configuration.json_input = true;
        self
    }

    /// Select qpdf's JSON output at the requested version.
    ///
    /// This is `QPDFJob::Config::json` (`libqpdf/QPDFJob_config.cc:253-265`).
    /// qpdf parses and range-checks the version spelling inside the callback;
    /// the argv boundaries own that parse here, so this takes the parsed
    /// version. `check_configuration` then defaults the destination to
    /// standard output when no output file was given, and `write_qpdf`
    /// dispatches JSON output through `writeOutfile`'s destination rewrite.
    pub fn json(&mut self, version: i32) -> &mut Self {
        self.job.configuration.json_version = Some(version);
        self
    }

    /// Select qpdf's `--json-output` mode at the requested version.
    ///
    /// This is `QPDFJob::Config::jsonOutput`
    /// (`libqpdf/QPDFJob_config.cc:312-326`): it selects JSON output, defaults
    /// stream data to inline and the decode level to none unless either was
    /// set explicitly, and adds the `qpdf` key.
    pub fn json_output(&mut self, version: i32) -> &mut Self {
        self.job.configuration.json_output = true;
        self.json(version);
        if !self.job.configuration.json_stream_data_set {
            self.job.configuration.json_stream_data = JsonStreamData::Inline;
        }
        if !self.job.configuration.json_decode_level_set {
            self.job.configuration.json_decode_level = crate::writer::DecodeLevel::None;
        }
        self.json_key(JsonKey::Qpdf);
        self
    }

    /// Request one top-level qpdf JSON key.
    ///
    /// This is `QPDFJob::Config::jsonKey`
    /// (`libqpdf/QPDFJob_config.cc:267-272`). qpdf keeps the keys in a
    /// `std::set`, so requesting the same key twice records it once.
    pub fn json_key(&mut self, key: JsonKey) -> &mut Self {
        if !self.job.configuration.json_keys.contains(&key) {
            self.job.configuration.json_keys.push(key);
        }
        self
    }

    /// Retain one raw `--json-object` selector.
    ///
    /// This is `QPDFJob::Config::jsonObject`
    /// (`libqpdf/QPDFJob_config.cc:274-279`). qpdf stores the spelling and
    /// parses it only while the object section is emitted
    /// (`libqpdf/QPDFJob.cc:929-997`).
    pub fn json_object(&mut self, selector: impl Into<String>) -> &mut Self {
        self.job.configuration.json_objects.push(selector.into());
        self
    }

    /// Select how stream payloads appear in JSON output.
    ///
    /// This is `QPDFJob::Config::jsonStreamData`
    /// (`libqpdf/QPDFJob_config.cc:281-296`), including the explicit-selection
    /// flag that keeps `--json-output` from defaulting the mode to inline.
    /// qpdf parses the spelling inside the callback; the argv boundaries own
    /// that parse here.
    pub fn json_stream_data(&mut self, stream_data: JsonStreamData) -> &mut Self {
        self.job.configuration.json_stream_data_set = true;
        self.job.configuration.json_stream_data = stream_data;
        self
    }

    /// Set the prefix for JSON stream side files.
    ///
    /// This is `QPDFJob::Config::jsonStreamPrefix`
    /// (`libqpdf/QPDFJob_config.cc:298-303`).
    pub fn json_stream_prefix(&mut self, prefix: impl Into<Vec<u8>>) -> &mut Self {
        self.job.configuration.json_stream_prefix = Some(prefix.into());
        self
    }

    /// Validate generated JSON output against qpdf's own schema.
    ///
    /// This is `QPDFJob::Config::testJsonSchema`
    /// (`libqpdf/QPDFJob_config.cc:335-340`).
    pub fn test_json_schema(&mut self) -> &mut Self {
        self.job.configuration.test_json_schema = true;
        self
    }

    /// Set the stream decoding level used for JSON output.
    ///
    /// This is `QPDFJob::Config::decodeLevel`
    /// (`libqpdf/QPDFJob_config.cc:717-732`), including the explicit-selection
    /// flag that keeps `--json-output` from defaulting the level to none.
    /// qpdf's single `m->decode_level` serves both the writer and JSON output;
    /// flpdf carries the writer's copy in the writer configuration, so this
    /// sets the JSON consumer's level only.
    pub fn decode_level(&mut self, decode_level: crate::writer::DecodeLevel) -> &mut Self {
        self.job.configuration.json_decode_level_set = true;
        self.job.configuration.json_decode_level = decode_level;
        self
    }

    /// Configure qpdf's `updateFromJson` create-stage input.
    pub fn update_from_json(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.job.configuration.update_from_json = Some(path.into());
        self
    }

    /// Configure qpdf's `setPageLabels` option-table result.
    pub fn set_page_labels<I, S>(&mut self, specs: I) -> Result<&mut Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let specs = specs
            .into_iter()
            .map(|spec| parse_page_label_spec(spec.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        self.job.configuration.set_page_labels = Some(specs);
        Ok(self)
    }

    /// Configure qpdf's `removePageLabels` bare option.
    pub fn remove_page_labels(&mut self) -> &mut Self {
        self.job.configuration.remove_page_labels = true;
        self
    }

    /// Route qpdf's document transformations through the canonical job stage.
    pub fn remove_restrictions(&mut self) -> &mut Self {
        self.job.configuration.remove_restrictions = true;
        self
    }

    /// Enable qpdf's provider-backed page-content coalescing.
    pub fn coalesce_contents(&mut self) -> &mut Self {
        self.job.configuration.coalesce_contents = true;
        self
    }

    /// Select qpdf's `doInspection` document-check branch.
    pub fn check(&mut self) -> &mut Self {
        self.job.configuration.check = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's linearization-check inspection branch.
    pub fn check_linearization(&mut self) -> &mut Self {
        self.job.configuration.check_linearization = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's raw page-count inspection branch.
    pub fn show_npages(&mut self) -> &mut Self {
        self.job.configuration.show_npages = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's page-list inspection branch.
    pub fn show_pages(&mut self) -> &mut Self {
        self.job.configuration.show_pages = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's cross-reference inspection branch.
    pub fn show_xref(&mut self) -> &mut Self {
        self.job.configuration.show_xref = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's linearization-data inspection branch.
    pub fn show_linearization(&mut self) -> &mut Self {
        self.job.configuration.show_linearization = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's encryption-parameters inspection branch.
    pub fn show_encryption(&mut self) -> &mut Self {
        self.job.configuration.show_encryption = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's embedded-file listing inspection branch.
    pub fn list_attachments(&mut self) -> &mut Self {
        self.job.configuration.list_attachments = true;
        self.job.configuration.require_output = false;
        self
    }

    /// Select qpdf's embedded-file extraction inspection branch.
    pub fn show_attachment(&mut self, key: impl Into<Vec<u8>>) -> &mut Self {
        self.job.configuration.show_attachment = Some(key.into());
        self.job.configuration.require_output = false;
        self
    }

    /// Select raw stream output for qpdf's object inspection branch.
    pub fn raw_stream_data(&mut self) -> &mut Self {
        self.job.configuration.show_raw_stream_data = true;
        self
    }

    /// Select filtered stream output for qpdf's object inspection branch.
    pub fn filtered_stream_data(&mut self) -> &mut Self {
        self.job.configuration.show_filtered_stream_data = true;
        self
    }

    /// Configure qpdf's job-level `normalizeContent` setting.
    pub fn normalize_content(&mut self, value: bool) -> &mut Self {
        self.job.set_content_normalization(value);
        self
    }

    /// Enable qpdf's form-appearance generation phase.
    pub fn generate_appearances(&mut self) -> &mut Self {
        self.job.configuration.generate_appearances = true;
        self
    }

    /// Enable qpdf's annotation flattening mode.
    pub fn flatten_annotations(&mut self, mode: FlattenAnnotationsMode) -> &mut Self {
        self.job.configuration.flatten_annotations = Some(mode);
        self
    }

    /// Enable qpdf's page-rotation flattening phase.
    pub fn flatten_rotation(&mut self) -> &mut Self {
        self.job.configuration.flatten_rotation = true;
        self
    }

    /// Queue one qpdf `--rotate` parameter for the create-stage rotation map.
    ///
    /// qpdf's public `QPDFJob::Config::rotate` delegates to the private
    /// `parseRotationParameter` helper and stores the result keyed by its raw
    /// page-range string (`QPDFJob_config.cc:786-790`, `QPDFJob.cc:369-415`).
    /// Keep the byte-preserving parameter at this Config boundary so direct
    /// argv and job-JSON callers share the same parser and last-write-wins
    /// map semantics.
    pub fn rotate(&mut self, parameter: impl AsRef<[u8]>) -> Result<&mut Self> {
        let parameter = parse_rotation_parameter(parameter.as_ref())?;
        self.job
            .configuration
            .rotations
            .insert(parameter.range, parameter.spec);
        Ok(self)
    }

    /// Append qpdf's `collate` page-group sizes.
    ///
    /// This is `QPDFJob::Config::collate` (`libqpdf/QPDFJob_config.cc:95-125`).
    /// qpdf permits repeated calls and appends each call's comma-separated
    /// values to one ordered vector; an empty parameter appends the default
    /// group size of one. Reuse the byte-oriented parser shared with the job
    /// JSON boundary so its unsigned-prefix and error behavior remains one
    /// canonical implementation.
    pub fn collate(&mut self, parameter: impl AsRef<[u8]>) -> Result<&mut Self> {
        let values = parse_qpdf_collate_parameter(parameter.as_ref())?;
        self.job
            .configuration
            .collate
            .get_or_insert_with(Vec::new)
            .extend(values);
        Ok(self)
    }

    /// Configure qpdf's `splitPages` writer-stage dispatch.
    ///
    /// qpdf's Config stores the signed `int` produced by `string_to_int` and
    /// leaves the later `split-pages` conversion to the write path
    /// (`QPDFJob_config.cc:597-609`, `QPDFJob.cc:3218-3240`). Reuse the job
    /// parser here rather than narrowing the CLI value before the canonical
    /// writer sees it.
    pub fn split_pages(&mut self, parameter: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.job.configuration.split_pages = Some(parse_job_split_pages(parameter.as_ref())?);
        Ok(self)
    }

    /// Configure qpdf's resource-pruning policy for split-page output.
    ///
    /// Plain rewrites do not consume this field; qpdf's `doSplitPages` path
    /// does, after create-stage transformations have completed
    /// (`QPDFJob.cc:2939-3027`).
    pub fn remove_unreferenced_resources(
        &mut self,
        mode: RemoveUnreferencedResources,
    ) -> &mut Self {
        self.job.configuration.remove_unreferenced_resources = mode;
        self
    }

    /// Configure qpdf's inline-image externalization phase.
    pub fn externalize_inline_images(&mut self, min_bytes: usize) -> &mut Self {
        self.job.configuration.externalize_inline_images = true;
        self.job.configuration.image_options.inline_min_bytes = min_bytes;
        self
    }

    /// Enable qpdf's inline-image externalization flag without changing the
    /// separately configured `iiMinBytes` threshold.
    pub fn set_externalize_inline_images(&mut self) -> &mut Self {
        self.job.configuration.externalize_inline_images = true;
        self
    }

    /// Configure qpdf's image optimization phase and its thresholds.
    pub fn optimize_images(&mut self, options: ImageOptimizationOptions) -> &mut Self {
        self.job.configuration.optimize_images = true;
        self.job.configuration.image_options = options;
        self
    }

    /// Enable qpdf's image optimization flag without changing its thresholds
    /// or the inline-image policy already layered into the configuration.
    pub fn set_optimize_images(&mut self) -> &mut Self {
        self.job.configuration.optimize_images = true;
        self
    }

    /// Enable qpdf's `keepInlineImages` option in the shared job configuration.
    pub fn keep_inline_images(&mut self) -> &mut Self {
        self.job.configuration.image_options.keep_inline_images = true;
        self
    }

    /// Set qpdf's unsigned `iiMinBytes` threshold at the configuration boundary.
    pub fn ii_min_bytes(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.job.configuration.image_options.inline_min_bytes =
            parse_qpdf_collate_uint(value.as_ref())?;
        Ok(self)
    }

    /// Set qpdf's unsigned `oiMinWidth` threshold at the configuration boundary.
    pub fn oi_min_width(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.job.configuration.image_options.min_width =
            parse_qpdf_collate_uint(value.as_ref())? as u32;
        Ok(self)
    }

    /// Set qpdf's unsigned `oiMinHeight` threshold at the configuration boundary.
    pub fn oi_min_height(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.job.configuration.image_options.min_height =
            parse_qpdf_collate_uint(value.as_ref())? as u32;
        Ok(self)
    }

    /// Set qpdf's unsigned `oiMinArea` threshold at the configuration boundary.
    pub fn oi_min_area(&mut self, value: impl AsRef<[u8]>) -> Result<&mut Self> {
        self.job.configuration.image_options.min_area =
            parse_qpdf_collate_uint(value.as_ref())? as u32;
        Ok(self)
    }

    /// Queue an overlay source for qpdf's create-stage underlay/overlay pass.
    pub fn overlay(
        &mut self,
        path: impl Into<PathBuf>,
        password: impl Into<Vec<u8>>,
        from: PageRange,
        to: PageRange,
        repeat: Option<PageRange>,
    ) -> &mut Self {
        self.job.configuration.overlays.push(JobOverlayConfig {
            path: path.into(),
            password: password.into(),
            from,
            to,
            repeat,
            kind: OverlayKind::Overlay,
        });
        self
    }

    /// Queue an underlay source for qpdf's create-stage underlay/overlay pass.
    pub fn underlay(
        &mut self,
        path: impl Into<PathBuf>,
        password: impl Into<Vec<u8>>,
        from: PageRange,
        to: PageRange,
        repeat: Option<PageRange>,
    ) -> &mut Self {
        self.job.configuration.underlays.push(JobOverlayConfig {
            path: path.into(),
            password: password.into(),
            from,
            to,
            repeat,
            kind: OverlayKind::Underlay,
        });
        self
    }

    /// Apply the canonical writer settings used by `write_qpdf`.
    pub fn writer_configuration(&mut self, configuration: WriterConfiguration) -> &mut Self {
        self.job.configuration.writer = configuration;
        self
    }

    /// Queue one `--pages` file specification for
    /// `QPDFJob::PagesConfig::pageSpec` (`QPDFJob_config.cc:963-969`), which
    /// `QPDFJob::handlePageSpecs` later consumes (`QPDFJob.cc:2359-2440`).
    /// Multiple calls remain in the same Config pages group; a JSON pages
    /// group cannot be added before or after them, matching qpdf's
    /// `Config::pages()` one-time guard (`QPDFJob_config.cc:945-961`).
    ///
    /// `range` uses [`PageRange`]'s qpdf `QUtil::parse_numrange` grammar,
    /// including `x` exclusion groups. An empty string selects every page,
    /// matching qpdf's `1-z` default (`QPDFJob.cc:2364-2372`). `password` is
    /// optional so the qpdf null-password state remains distinct from an
    /// explicitly supplied empty password.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Usage`] when `range` does not parse, matching the
    /// job-JSON path for the same field.
    pub fn add_page_spec(
        &mut self,
        file: impl Into<PathBuf>,
        range: &str,
        password: Option<Vec<u8>>,
    ) -> Result<&mut Self> {
        if self.job.configuration.page_specs_origin == PageSpecsOrigin::Json {
            return Err(Error::Usage(UsageError::new(
                "--pages may only be specified one time",
            )));
        }
        let range = if range.is_empty() {
            PageRange::all()
        } else {
            PageRange::parse_numrange(range)
                .map_err(|error| Error::Usage(UsageError::new(error.to_string())))?
        };
        self.job.configuration.page_specs_origin = PageSpecsOrigin::Config;
        self.job.configuration.page_specs.push(JobPageConfig {
            path: file.into(),
            password,
            range,
        });
        Ok(self)
    }

    /// Queue one `--add-attachment` file for `QPDFJob::addAttachments`
    /// (`QPDFJob.cc:2044-2083`).
    pub fn add_attachment(&mut self, options: AttachmentAddOptions) -> &mut Self {
        self.job.configuration.attachments_to_add.push(options);
        self
    }

    /// Queue one `--remove-attachment` key for the removal pass in
    /// `QPDFJob::handleTransformations` (`QPDFJob.cc:2223-2233`).
    pub fn remove_attachment(&mut self, key: impl Into<Vec<u8>>) -> &mut Self {
        self.job
            .configuration
            .attachments_to_remove
            .push(key.into());
        self
    }

    /// Queue one `--copy-attachments-from` donor for
    /// `QPDFJob::copyAttachments` (`QPDFJob.cc:2089-2135`).
    pub fn copy_attachments_from(
        &mut self,
        path: impl Into<PathBuf>,
        password: impl Into<Vec<u8>>,
        prefix: impl Into<Vec<u8>>,
    ) -> &mut Self {
        self.job
            .configuration
            .attachments_to_copy
            .push(JobCopyAttachmentsConfig {
                path: path.into(),
                password: password.into(),
                prefix: prefix.into(),
            });
        self
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
    use crate::job::overlay::OverlayVerboseSource;
    use crate::{Error, ObjectHandle, PageDocumentHelper, PageInput, PdfOpenOptions};
    use std::io::Cursor;

    #[test]
    fn config_verbose_enables_the_job_verbose_setting() {
        let mut job = QPDFJob::new();
        job.config().verbose();

        assert!(job.verbose());
    }

    #[test]
    fn config_image_setters_layer_individual_options_on_partial_job_json_state() {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(
            r#"{"keepInlineImages":"","iiMinBytes":"7","oiMinWidth":"8"}"#,
        )
        .expect("job JSON image options parse");

        {
            let mut config = job.config();
            config.keep_inline_images();
            config.ii_min_bytes("+42").expect("qpdf unsigned threshold");
            config.oi_min_width("43").expect("qpdf width threshold");
            config.oi_min_height("44").expect("qpdf height threshold");
            config.oi_min_area("45").expect("qpdf area threshold");
        }

        assert!(job.configuration.image_options.keep_inline_images);
        assert_eq!(job.configuration.image_options.inline_min_bytes, 42);
        assert_eq!(job.configuration.image_options.min_width, 43);
        assert_eq!(job.configuration.image_options.min_height, 44);
        assert_eq!(job.configuration.image_options.min_area, 45);

        let mut invalid = QPDFJob::new();
        assert!(matches!(
            invalid.config().ii_min_bytes("-1"),
            Err(Error::System(message)) if message.contains("underflow")
        ));
    }

    #[test]
    fn config_normalize_content_enables_the_job_inspection_setting() {
        let mut job = QPDFJob::new();
        job.config().normalize_content(true);

        assert!(job.content_normalization_enabled());
    }

    #[test]
    fn input_version_floor_exposes_the_highest_opened_source_version() {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/compat/one-page-v17.pdf"),
        )
        .expect("committed versioned fixture");
        let mut job = QPDFJob::new();
        job.open_document(
            Cursor::new(bytes),
            "one-page-v17.pdf",
            PdfOpenOptions::default(),
        )
        .expect("versioned fixture opens");

        assert_eq!(
            job.input_version_floor(),
            Some(PdfVersion::new(1, 7, 0)),
            "the public floor must expose qpdf's accumulated max_input_version"
        );
    }

    #[test]
    fn explicit_normalize_content_survives_a_later_writer_configuration_replacement() {
        let mut pdf = Pdf::empty().expect("empty PDF");
        let contents = pdf
            .new_stream_with_data(Rc::new(b"q\rQ".to_vec()))
            .expect("content stream");
        let page = ObjectHandle::dictionary(vec![
            (b"/Type".to_vec(), ObjectHandle::name(b"Page".to_vec())),
            (
                b"/MediaBox".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(0),
                    ObjectHandle::integer(100),
                    ObjectHandle::integer(100),
                ]),
            ),
            (b"/Contents".to_vec(), contents),
        ]);
        PageDocumentHelper::new(&mut pdf)
            .add_page(PageInput::direct(page), false)
            .expect("page insertion");

        let tempdir = tempfile::tempdir().expect("temporary directory");
        let output_path = tempdir.path().join("normalized.pdf");
        let mut job = QPDFJob::new();
        job.set_output_file(&output_path).expect("output path");
        job.set_content_normalization(true);
        job.config()
            .writer_configuration(WriterConfiguration::default());

        job.write_qpdf(&mut pdf)
            .expect("normalized document should be written");
        let mut written = Pdf::open(Cursor::new(
            std::fs::read(&output_path).expect("written PDF bytes"),
        ))
        .expect("written PDF should reopen");
        let page_ref = PageDocumentHelper::new(&mut written)
            .get_all_pages()
            .expect("written page list")
            .into_iter()
            .next()
            .expect("written page");
        let page = written.get_object_handle(page_ref);
        page.try_is_scalar().expect("written page resolves");
        let contents = page
            .try_get_key(b"/Contents")
            .expect("written contents key")
            .object_ref()
            .expect("written contents reference");
        let stream = written.get_object_handle(contents);

        assert_eq!(
            stream
                .get_stream_data(crate::writer::DecodeLevel::All)
                .expect("written stream data")
                .as_ref(),
            b"q\nQ"
        );
    }

    #[test]
    fn job_error_message_uses_qpdf_portable_not_found_text() {
        let missing = Error::file_io(
            "open",
            "missing-parent/output.pdf",
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert_eq!(
            QPDFJob::job_error_message(&missing),
            b"open missing-parent/output.pdf: No such file or directory"
        );

        let fallback = Error::file_io(
            "open",
            "output.pdf",
            std::io::Error::other("native fallback"),
        );
        assert_eq!(
            QPDFJob::job_error_message(&fallback),
            b"open output.pdf: native fallback"
        );
    }

    #[test]
    fn config_writer_configuration_reaches_the_job_writer() {
        let tempdir = tempfile::tempdir().expect("temporary output directory");
        let output = tempdir.path().join("static-id.pdf");
        let mut pdf = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/compat/one-page.pdf"),
            )
            .expect("committed one-page fixture"),
        ))
        .expect("one-page fixture parses");
        let mut job = QPDFJob::new();
        job.set_output_file(&output)
            .expect("output path is accepted");
        let mut writer = WriterConfiguration::default();
        writer.set_static_id(true);
        job.config().writer_configuration(writer);

        job.write_qpdf(&mut pdf).expect("job write succeeds");
        let bytes = std::fs::read(&output).expect("static-id output exists");
        assert!(
            bytes
                .windows(b"<31415926535897932384626433832795>".len())
                .any(|window| window == b"<31415926535897932384626433832795>"),
            "writer configuration must reach the canonical writer"
        );
    }

    #[test]
    fn overlay_verbose_progress_rejects_an_invalid_source_index() {
        let job = QPDFJob::new();
        let report = [OverlayVerbosePage {
            dest_page: 1,
            sources: vec![OverlayVerboseSource {
                spec_index: 0,
                kind: OverlayKind::Overlay,
                src_page: 1,
            }],
        }];
        let configuration = job.configuration.clone();

        let error = job
            .report_overlay_progress(&report, &configuration)
            .expect_err("an unpaired verbose source must be rejected");
        assert!(matches!(
            error,
            Error::Internal(message) if message == "overlay verbose source index out of range"
        ));
    }

    /// `QPDF::emptyPDF` is a document factory that leaves the job
    /// configuration untouched (`libqpdf/QPDF.cc:290-293`); only
    /// `Config::emptyInput` marks the job's input as empty. Creating an empty
    /// document must therefore keep the job reusable for a real input while
    /// still keying the empty primary with qpdf's empty source-map name.
    #[test]
    fn create_empty_document_keeps_the_input_configuration_reusable() {
        let mut job = QPDFJob::new();
        job.create_empty_document().expect("empty document");

        assert_eq!(
            job.page_spec_source_sort_key(0, b"empty PDF"),
            Vec::<u8>::new()
        );
        assert_eq!(
            job.page_spec_source_sort_key(1, b"other.pdf"),
            b"other.pdf".to_vec()
        );
        assert!(
            !job.configuration.empty_input,
            "the factory must not configure --empty"
        );
        job.set_input_file("input.pdf")
            .expect("a job that created an empty document can still take an input file");
        assert_eq!(
            job.page_spec_source_sort_key(0, b"input.pdf"),
            Vec::<u8>::new(),
            "the empty primary created earlier keeps qpdf's empty map key"
        );
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
            Err(Error::QpdfExc(warning)) if warning.get_message_detail() == b"unable to find /Root dictionary"
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
            Err(Error::QpdfExc(warning)) if warning.get_message_detail() == b"unable to find /Root dictionary"
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

    #[test]
    fn page_label_transform_noops_when_the_document_has_no_root() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.7\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("rootless trailer still opens");
        let mut job = QPDFJob::new();
        job.configuration.set_page_labels = Some(Vec::new());
        let configuration = job.configuration.clone();

        job.apply_page_label_transformations(&mut pdf, &configuration)
            .expect("missing root is a qpdf-tolerant no-op");
    }

    #[test]
    fn page_label_transform_noops_when_the_root_is_not_a_dictionary() {
        let mut pdf = Pdf::empty().expect("empty PDF has a root");
        let root_ref = pdf.root_ref().expect("empty PDF has a root");
        pdf.replace_object(root_ref, ObjectHandle::integer(0))
            .expect("replace the root with a scalar");
        let mut job = QPDFJob::new();
        job.configuration.set_page_labels = Some(Vec::new());
        let configuration = job.configuration.clone();

        job.apply_page_label_transformations(&mut pdf, &configuration)
            .expect("non-dictionary root is a qpdf-tolerant no-op");
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
            Err(Error::QpdfExc(warning)) => assert_eq!(
                warning.get_message_detail(),
                b"unable to find /Root dictionary"
            ),
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
    fn configured_inspection_maps_detected_check_errors_to_an_operation_error() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/compat/one-page.pdf"
            ))
            .to_vec(),
        ))
        .expect("one-page fixture parses");
        let page_ref = PageDocumentHelper::new(&mut pdf)
            .get_all_pages()
            .expect("page tree resolves")[0];
        let page = pdf.get_object_handle(page_ref);
        page.try_dereference().expect("page resolves");
        page.replace_key(b"/Contents", ObjectHandle::integer(42))
            .expect("page remains mutable");

        let mut job = QPDFJob::new();
        job.config().check();
        let failure = job
            .inspect_configured(&mut pdf)
            .expect_err("check errors must abort the enclosing inspection");
        // The check consumer already wrote qpdf's single `errors detected`
        // line, so the boundary must not report it again.
        assert!(matches!(failure, crate::job::CheckError::ErrorsDetected));
    }

    #[test]
    fn inspect_configured_completes_and_reports_memory_usage() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/minimal.pdf"
            ))
            .to_vec(),
        ))
        .expect("minimal fixture parses");
        let logger = QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(crate::pipeline::Discard)));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.config().show_npages();
        job.configuration.report_memory_usage = true;

        assert_eq!(
            job.inspect_configured(&mut pdf)
                .expect("configured inspection succeeds"),
            JobExitCode::Success
        );
    }

    #[test]
    fn job_output_pipeline_exposes_its_qpdf_identifier() {
        let pipeline = JobOutputPipeline(PipelineHandle::new(crate::pipeline::Discard));

        assert_eq!(pipeline.identifier(), "qpdf job output");
    }

    #[test]
    fn job_output_writer_forwards_bytes_and_flush() {
        let mut writer = JobOutputWriter::new(PipelineHandle::new(crate::pipeline::Discard));
        std::io::Write::write_all(&mut writer, b"job output").unwrap();
        std::io::Write::flush(&mut writer).unwrap();
    }

    #[test]
    fn job_output_writer_batches_fragments_until_the_buffer_is_full() {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut writer = JobOutputWriter::new(PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&bytes),
        }));

        std::io::Write::write_all(&mut writer, b"first").unwrap();
        std::io::Write::write_all(&mut writer, b" second").unwrap();
        assert!(
            bytes.lock().unwrap().is_empty(),
            "small fragments wait for the buffer"
        );

        std::io::Write::flush(&mut writer).unwrap();
        assert_eq!(bytes.lock().unwrap().as_slice(), b"first second");
    }

    #[test]
    fn job_output_writer_passes_oversized_fragments_straight_through() {
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut writer = JobOutputWriter::new(PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&bytes),
        }));
        let payload = vec![b'x'; JOB_OUTPUT_BUFFER_CAPACITY + 1];

        std::io::Write::write_all(&mut writer, b"prefix").unwrap();
        std::io::Write::write_all(&mut writer, &payload).unwrap();

        let recorded = bytes.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            b"prefix".len() + payload.len(),
            "a fragment larger than the buffer is written without being copied into it"
        );
        assert!(recorded.starts_with(b"prefix"));
        std::io::Write::flush(&mut writer).unwrap();
        assert_eq!(bytes.lock().unwrap().len(), recorded.len());
    }

    #[test]
    fn json_config_selectors_follow_qpdf_json_output_defaults() {
        let mut job = QPDFJob::new();
        {
            let mut configuration = job.config();
            configuration.json_key(JsonKey::Pages);
            configuration.json_output(2);
            configuration.json_object("trailer");
        }

        assert_eq!(job.configuration.json_version, Some(2));
        assert!(job.configuration.json_output);
        assert_eq!(
            job.configuration.json_keys,
            vec![JsonKey::Pages, JsonKey::Qpdf],
            "--json-output adds the qpdf key once, like qpdf's std::set"
        );
        assert_eq!(job.configuration.json_objects, vec!["trailer".to_owned()]);
        assert_eq!(job.configuration.json_stream_data, JsonStreamData::Inline);
        assert_eq!(
            job.configuration.json_decode_level,
            crate::writer::DecodeLevel::None
        );

        // Re-adding a key qpdf already selected leaves one entry behind.
        job.config().json_key(JsonKey::Qpdf);
        assert_eq!(
            job.configuration.json_keys,
            vec![JsonKey::Pages, JsonKey::Qpdf]
        );
    }

    #[test]
    fn explicit_json_selectors_survive_the_json_output_defaults() {
        let mut job = QPDFJob::new();
        {
            let mut configuration = job.config();
            configuration.json_stream_data(JsonStreamData::None);
            configuration.decode_level(crate::writer::DecodeLevel::All);
            configuration.json_stream_prefix(b"side".to_vec());
            configuration.test_json_schema();
            configuration.json_output(2);
        }

        assert_eq!(
            job.configuration.json_stream_data,
            JsonStreamData::None,
            "an explicit --json-stream-data keeps --json-output from selecting inline"
        );
        assert_eq!(
            job.configuration.json_decode_level,
            crate::writer::DecodeLevel::All,
            "an explicit --decode-level keeps --json-output from selecting none"
        );
        assert_eq!(
            job.configuration.json_stream_prefix.as_deref(),
            Some(b"side".as_slice())
        );
        assert!(job.configuration.test_json_schema);
    }

    #[test]
    fn json_selector_records_the_requested_version() {
        let mut job = QPDFJob::new();
        job.config().json(1);

        assert_eq!(job.configuration.json_version, Some(1));
        assert!(!job.configuration.json_output);
        assert_eq!(
            job.configuration.json_stream_data,
            JsonStreamData::None,
            "plain --json keeps the configuration's own stream-data default"
        );
    }

    #[test]
    fn write_qpdf_reports_verbose_auto_password_conversion() {
        let tempdir = tempfile::tempdir().expect("temporary output directory");
        let output = tempdir.path().join("output.pdf");
        let input = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_info(Some(PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&bytes),
        })));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_output_file(&output)
            .expect("output path is accepted");
        job.set_verbose(true);
        job.configuration
            .writer
            .set_encryption_parameters(crate::EncryptParams::v4_aes128(
                "café".as_bytes().to_vec(),
                b"owner".to_vec(),
            ));

        let mut input = input;
        job.write_qpdf(&mut input).expect("job write succeeds");
        assert_eq!(job.get_exit_code(), JobExitCode::Success);
        assert!(output.is_file());
        assert!(
            bytes.lock().unwrap().windows(
                b"qpdf: automatically converting Unicode password to single-byte encoding as required for 40-bit or 128-bit encryption\n".len()
            ).any(|window| window == b"qpdf: automatically converting Unicode password to single-byte encoding as required for 40-bit or 128-bit encryption\n"),
            "verbose writer output must include qpdf's auto-conversion info"
        );
    }

    #[test]
    fn write_qpdf_closes_retained_page_sources_before_replace_input() {
        let primary_fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let secondary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/attachment-two-page.pdf");
        let tempdir = tempfile::tempdir().expect("temporary replace-input directory");
        let primary = tempdir.path().join("primary.pdf");
        std::fs::copy(&primary_fixture, &primary).expect("copy primary fixture");
        let json = serde_json::json!({
            "inputFile": primary,
            "replaceInput": "",
            "pages": [
                {"file": ".", "range": "1"},
                {"file": secondary, "range": "1"}
            ]
        })
        .to_string();

        let mut job = QPDFJob::new();
        job.initialize_from_json(&json)
            .expect("multi-source replace-input configuration");
        let mut pdf = job
            .create_qpdf()
            .expect("create qpdf")
            .expect("primary document");
        assert_eq!(job.page_source_documents.len(), 2);
        assert!(job
            .page_source_documents
            .iter()
            .all(|source| !source.resolver.input_source_closed()));

        job.write_qpdf(&mut pdf)
            .expect("replace-input write succeeds");
        assert!(job.page_source_documents.iter().all(|source| {
            source.resolver.input_source_closed()
                && source
                    .input_source_control
                    .as_ref()
                    .is_none_or(|control| control.is_closed_for_test())
        }));
        assert!(primary.is_file(), "replace-input keeps the input path");
    }

    #[test]
    fn write_qpdf_closes_retained_overlay_sources_before_replace_input() {
        let primary_fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let tempdir = tempfile::tempdir().expect("temporary replace-input directory");
        let primary = tempdir.path().join("primary.pdf");
        std::fs::copy(&primary_fixture, &primary).expect("copy primary fixture");
        let json = serde_json::json!({
            "inputFile": primary,
            "replaceInput": "",
            "overlay": {"file": primary, "from": "1", "to": "1"}
        })
        .to_string();

        let mut job = QPDFJob::new();
        job.initialize_from_json(&json)
            .expect("self-overlay replace-input configuration");
        let mut pdf = job
            .create_qpdf()
            .expect("create qpdf")
            .expect("primary document");
        assert_eq!(job.overlay_sources.len(), 1);
        assert!(!job.overlay_sources[0].source.resolver.input_source_closed());

        job.write_qpdf(&mut pdf)
            .expect("replace-input write succeeds");
        let overlay = &job.overlay_sources[0].source;
        assert!(overlay.resolver.input_source_closed());
        assert!(overlay
            .input_source_control
            .as_ref()
            .is_none_or(|control| control.is_closed_for_test()));
        assert!(primary.is_file(), "replace-input keeps the input path");
    }

    #[test]
    fn write_qpdf_emits_auto_password_notices_in_user_owner_order() {
        let tempdir = tempfile::tempdir().expect("temporary output directory");
        let output = tempdir.path().join("output.pdf");
        let mut input = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        let sink = PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&bytes),
        });
        logger.set_info(Some(sink.clone()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(sink));

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_output_file(&output)
            .expect("output path is accepted");
        job.set_verbose(true);
        job.configuration.password_mode = PasswordMode::Auto;
        job.configuration
            .writer
            .set_encryption_parameters(crate::EncryptParams::v4_aes128(
                "日本".as_bytes().to_vec(),
                "café".as_bytes().to_vec(),
            ));

        job.write_qpdf(&mut input).expect("job write succeeds");
        assert_eq!(job.get_exit_code(), JobExitCode::Success);
        let output = bytes.lock().unwrap();
        let warning = b"qpdf: WARNING: supplied password looks like a Unicode password with characters not allowed in passwords for 40-bit and 128-bit encryption; most readers will not be able to open this file with the supplied password. (Use --password-mode=bytes to suppress this warning and use the password anyway.)\n";
        let info = b"qpdf: automatically converting Unicode password to single-byte encoding as required for 40-bit or 128-bit encryption\n";
        let warning_at = output
            .windows(warning.len())
            .position(|window| window == warning);
        let info_at = output.windows(info.len()).position(|window| window == info);
        assert!(
            warning_at
                .is_some_and(|warning_at| info_at.is_some_and(|info_at| warning_at < info_at)),
            "user warning must precede owner info in the shared logger stream"
        );
    }

    #[test]
    fn write_qpdf_propagates_verbose_auto_password_info_sink_error() {
        let tempdir = tempfile::tempdir().expect("temporary output directory");
        let output = tempdir.path().join("output.pdf");
        let mut input = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let logger = QPDFLogger::create();
        logger.set_info(Some(PipelineHandle::new(
            crate::pipeline::test_support::NthWriteFailure::new(1),
        )));

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_output_file(&output)
            .expect("output path is accepted");
        job.set_verbose(true);
        job.configuration
            .writer
            .set_encryption_parameters(crate::EncryptParams::v4_aes128(
                "café".as_bytes().to_vec(),
                b"owner".to_vec(),
            ));

        let error = job
            .write_qpdf(&mut input)
            .expect_err("info sink failure must propagate");
        assert!(matches!(
            error,
            Error::System(message) if message == "sink write failure 1"
        ));
    }

    #[test]
    fn write_qpdf_reserves_stdout_before_auto_password_diagnostic() {
        let mut input = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_error(Some(PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&errors),
        })));
        logger.set_warn(Some(logger.discard()));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_output_file("-").expect("stdout output is accepted");
        job.set_verbose(true);
        job.configuration.password_mode = PasswordMode::Auto;
        job.configuration.writer.set_deterministic_id(true);
        job.configuration
            .writer
            .set_encryption_parameters(crate::EncryptParams::v4_aes128(
                "café".as_bytes().to_vec(),
                b"owner".to_vec(),
            ));

        let error = job
            .write_qpdf(&mut input)
            .expect_err("writer preflight error must be returned to the caller");
        assert!(!error.to_string().is_empty());
        let error = String::from_utf8_lossy(&errors.lock().unwrap()).into_owned();
        assert!(
            error.contains("deterministic") && !error.contains("called setSave"),
            "stdout reservation must precede the diagnostic: {error:?}"
        );
    }

    #[test]
    fn apply_transformations_reserves_stdout_before_the_document_stage() {
        let mut pdf = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let logger = QPDFLogger::create();
        let mut job = QPDFJob::new();
        job.set_logger(logger.clone());
        job.set_output_file("-").expect("stdout output is accepted");
        job.set_verbose(true);

        job.apply_transformations(&mut pdf)
            .expect("the create stage succeeds for the minimal fixture");

        assert!(
            logger
                .get_save()
                .expect("save pipeline")
                .is_same(&logger.standard_output()),
            "qpdf reserves stdout in checkConfiguration, which createQPDF runs \
             before any transformation (QPDFJob.cc:428-431,614-626)"
        );
        assert!(
            logger
                .get_info()
                .expect("info pipeline")
                .is_same(&logger.standard_error()),
            "reserving stdout must reroute info output to stderr so verbose \
             transformations cannot consume the stream the PDF needs"
        );
    }

    #[test]
    fn password_interpretation_setters_reach_job_owned_opens() {
        let mut job = QPDFJob::new();
        job.set_password_mode(PasswordMode::HexBytes);
        job.set_password_is_hex_key(true);
        job.set_suppress_password_recovery(true);

        let options = job.configured_open_options(b"75".to_vec());

        assert_eq!(options.password_mode, PasswordMode::HexBytes);
        assert!(options.password_is_hex_key);
        assert!(options.suppress_password_recovery);
    }

    #[test]
    fn write_qpdf_maps_direct_stdout_reservation_failure_to_job_error() {
        let mut input = Pdf::open(Cursor::new(
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");
        let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = QPDFLogger::create();
        logger.set_error(Some(PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&errors),
        })));
        logger.set_warn(Some(logger.discard()));

        // A direct write_qpdf caller can use the job's info logger before the
        // two-stage write boundary reserves stdout for the binary output.
        logger
            .info(b"")
            .expect("empty info write marks stdout as used");

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_output_file("-").expect("stdout output is accepted");

        let error = job
            .write_qpdf(&mut input)
            .expect_err("stdout reservation failure must be returned to the caller");
        assert!(!error.to_string().is_empty());
        let error = String::from_utf8_lossy(&errors.lock().unwrap()).into_owned();
        assert!(
            error.contains(
                "called setSave on standard output after standard output has already been used"
            ),
            "direct write_qpdf must report the reservation failure: {error:?}"
        );
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
        let empty_range = job_json_members(&crate::json::Json::parse(br#"{"range":""}"#).unwrap());
        assert_eq!(
            job_json_range(empty_range.get(b"range".as_slice()), ".range")
                .unwrap()
                .resolve(3)
                .unwrap(),
            vec![1, 2, 3]
        );
        let bad_range_type =
            job_json_members(&crate::json::Json::parse(br#"{"range":false}"#).unwrap());
        assert!(job_json_range(bad_range_type.get(b"range".as_slice()), ".range").is_err());
        let bad_range_syntax =
            job_json_members(&crate::json::Json::parse(br#"{"range":"bad"}"#).unwrap());
        assert!(job_json_range(bad_range_syntax.get(b"range".as_slice()), ".range").is_err());

        let empty_overlay =
            crate::json::Json::parse(br#"[{"file":"source.pdf","from":"","to":"","repeat":""}]"#)
                .unwrap();
        let mut overlays = Vec::new();
        parse_job_overlay_specs(&mut overlays, &empty_overlay, OverlayKind::Overlay).unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].from.resolve(3).unwrap(), Vec::<u32>::new());
        assert_eq!(overlays[0].to.resolve(3).unwrap(), Vec::<u32>::new());
        assert_eq!(
            overlays[0]
                .repeat
                .as_ref()
                .expect("explicit empty repeat remains present")
                .resolve(3)
                .unwrap(),
            Vec::<u32>::new()
        );

        let absent_overlay = crate::json::Json::parse(br#"[{"file":"source.pdf"}]"#).unwrap();
        let mut overlays = Vec::new();
        parse_job_overlay_specs(&mut overlays, &absent_overlay, OverlayKind::Overlay).unwrap();
        assert_eq!(overlays[0].from.resolve(3).unwrap(), vec![1, 2, 3]);
        assert_eq!(overlays[0].to.resolve(3).unwrap(), vec![1, 2, 3]);
        assert!(overlays[0].repeat.is_none());
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

        let inherited = EncryptionDefaults::default();
        let encrypt_40 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","40bit":{"annotate":"y","extract":"n","modify":"none","print":"low"}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_40, true, &inherited).is_ok());
        let encrypt_128 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","128bit":{"accessibility":"y","annotate":"n","assemble":"y","cleartextMetadata":"","extract":"n","form":"y","modifyOther":"n","modify":"all","print":"full","forceV4":"","useAes":"n"}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_128, true, &inherited).is_ok());
        let encrypt_128_no_accessibility = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","128bit":{"accessibility":"n","useAes":"y"}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_128_no_accessibility, true, &inherited).is_ok());
        let encrypt_256 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","256bit":{"forceR5":"","allowInsecure":""}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&encrypt_256, true, &inherited).is_ok());
        let encrypt_128_rc4 =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"o","128bit":{}}"#)
                .unwrap();
        assert!(parse_job_encrypt(&encrypt_128_rc4, true, &inherited).is_ok());
        let encrypt_256_r6 =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"o","256bit":{}}"#)
                .unwrap();
        let (_, encrypt_256_defaults) =
            parse_job_encrypt(&encrypt_256_r6, true, &inherited).unwrap();
        assert!(encrypt_256_defaults.use_aes);
        let insecure_256 =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"","256bit":{}}"#)
                .unwrap();
        let (_, insecure_defaults) = parse_job_encrypt(&insecure_256, true, &inherited).unwrap();
        assert!(!insecure_defaults.allow_insecure);
        let allowed_insecure_256 = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"","256bit":{"allowInsecure":""}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&allowed_insecure_256, true, &inherited).is_ok());
        let missing_password = crate::json::Json::parse(br#"{"128bit":{}}"#).unwrap();
        assert!(parse_job_encrypt(&missing_password, true, &inherited).is_err());
        let duplicate_key_length = crate::json::Json::parse(
            br#"{"userPassword":"u","ownerPassword":"o","40bit":{},"128bit":{}}"#,
        )
        .unwrap();
        assert!(parse_job_encrypt(&duplicate_key_length, true, &inherited).is_err());
        let no_key_length =
            crate::json::Json::parse(br#"{"userPassword":"u","ownerPassword":"o"}"#).unwrap();
        assert!(parse_job_encrypt(&no_key_length, true, &inherited).is_err());
        assert!(parse_job_encrypt(&encrypt_40, false, &inherited).is_ok());

        let (_, aes_defaults) =
            parse_job_encrypt(&encrypt_128_no_accessibility, true, &inherited).unwrap();
        let (_, r2_defaults) = parse_job_encrypt(&encrypt_40, true, &aes_defaults).unwrap();
        let (params, _) = parse_job_encrypt(&encrypt_128_rc4, true, &r2_defaults).unwrap();
        assert_eq!(params.method, EncryptMethod::V4Aes128);
    }

    #[test]
    fn job_json_split_pages_rejects_an_i32_overflow_at_the_qpdf_boundary() {
        // qpdf 11.9.0, `--job-json-file` with `"splitPages":"2147483648"`:
        //   qpdf: error with job-json file sp.json: integer out of range
        //   converting 2147483648 from a 8-byte signed type to a 4-byte
        //   signed type
        let error = parse_job_split_pages(b"2147483648").expect_err("i32 overflow must fail");
        assert_eq!(
            error.to_string(),
            "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type"
        );
    }

    #[test]
    fn job_json_split_pages_rejects_an_i64_overflow_at_the_qpdf_boundary() {
        // qpdf 11.9.0: `qpdf: overflow/underflow converting
        // 999999999999999999999 to 64-bit integer`
        let error =
            parse_job_split_pages(b"999999999999999999999").expect_err("i64 overflow must fail");
        assert_eq!(
            error.to_string(),
            "overflow/underflow converting 999999999999999999999 to 64-bit integer"
        );
    }

    /// A logger whose save pipeline is already claimed, so a job that writes
    /// to standard output neither emits the document into the test harness's
    /// own stream nor competes with another test for the process-wide one.
    fn discarding_save_logger() -> QPDFLogger {
        let logger = QPDFLogger::create();
        logger
            .set_save(Some(PipelineHandle::new(crate::pipeline::Discard)), false)
            .expect("claim the save pipeline");
        logger
    }

    #[test]
    fn write_qpdf_clears_the_output_name_for_standard_output() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let mut job = QPDFJob::new();
        job.set_input_file(&fixture).expect("input path");
        job.set_output_file("-").expect("standard output");
        job.set_logger(discarding_save_logger());
        let mut pdf = job
            .create_qpdf()
            .expect("create qpdf")
            .expect("primary document");
        assert!(
            job.creates_output(),
            "standard output is a destination when writeQPDF dispatches"
        );

        job.write_qpdf(&mut pdf).expect("standard-output write");

        assert!(
            !job.creates_output(),
            "the write stage clears the standard-output name before the summary"
        );
    }

    #[test]
    fn write_qpdf_clears_the_replace_input_temporary_name() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let tempdir = tempfile::tempdir().expect("temporary replace-input directory");
        let primary = tempdir.path().join("primary.pdf");
        std::fs::copy(&fixture, &primary).expect("copy fixture");
        let mut job = QPDFJob::new();
        job.set_input_file(&primary).expect("input path");
        job.config().replace_input().expect("replace input");
        let mut pdf = job
            .create_qpdf()
            .expect("create qpdf")
            .expect("primary document");

        job.write_qpdf(&mut pdf).expect("replace-input write");

        assert_eq!(
            job.configuration.output_file, None,
            "the temporary replacement name must not survive the rename"
        );
        assert!(
            job.creates_output(),
            "--replace-input still creates output once the name is cleared"
        );
    }

    #[test]
    fn write_qpdf_propagates_a_json_usage_error_without_reporting_it() {
        // `writeJSON` calls `usage()` when file-mode stream data has no prefix
        // and no output name to derive one from
        // (`libqpdf/QPDFJob.cc:3105-3110`). The resulting `QPDFUsage` escapes
        // `writeQPDF` uncaught, so the CLI renders qpdf's usage block instead
        // of a bare error line.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let errors = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = discarding_save_logger();
        logger.set_error(Some(PipelineHandle::new(RecordingInfoSink {
            bytes: std::sync::Arc::clone(&errors),
        })));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_input_file(&fixture).expect("input path");
        job.configuration.json_version = Some(2);
        job.configuration.json_stream_data = JsonStreamData::File;
        job.check_configuration()
            .expect("the implicit JSON destination is standard output");
        let mut pdf = job
            .create_qpdf()
            .expect("create qpdf")
            .expect("primary document");

        let error = job
            .write_qpdf(&mut pdf)
            .expect_err("file-mode stream data without a prefix is a usage error");

        assert!(
            matches!(error, Error::Usage(_)),
            "the write stage must keep qpdf's usage classification: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "please specify --json-stream-prefix since the input file name is unknown"
        );
        assert!(
            errors.lock().unwrap().is_empty(),
            "a usage error must not also be reported as a job error"
        );
    }

    #[test]
    fn run_propagates_a_json_usage_error_to_its_caller() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let mut job = QPDFJob::new();
        job.set_logger(discarding_save_logger());
        job.set_input_file(&fixture).expect("input path");
        job.configuration.json_version = Some(2);
        job.configuration.json_stream_data = JsonStreamData::File;

        let error = job
            .run()
            .expect_err("a write-stage usage error must reach the caller");

        assert!(
            matches!(error, Error::Usage(_)),
            "run() must not fold a usage error into an exit status: {error:?}"
        );
    }

    #[test]
    fn write_qpdf_reports_a_json_output_open_failure_in_qpdf_wording() {
        // qpdf opens the JSON destination with `QUtil::safe_fopen`
        // (`libqpdf/QPDFJob.cc:3103-3104`), whose `QPDFSystemError` reads
        // `open <path>: <strerror>` with no operation prefix.
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/one-page.pdf");
        let directory = tempfile::tempdir().expect("temporary directory");
        let unwritable = directory.path().join("missing").join("out.json");
        let mut job = QPDFJob::new();
        job.set_logger(discarding_save_logger());
        job.set_input_file(&fixture).expect("input path");
        job.set_output_file(&unwritable).expect("output path");
        job.configuration.json_version = Some(2);
        let mut pdf = job
            .create_qpdf()
            .expect("create qpdf")
            .expect("primary document");

        let error = job
            .write_qpdf(&mut pdf)
            .expect_err("an unopenable JSON destination fails the write");

        assert_eq!(
            error.to_string(),
            format!("open {}: No such file or directory", unwritable.display())
        );
    }

    #[test]
    fn check_configuration_assigns_the_implicit_json_output_destination() {
        let mut job = QPDFJob::new();
        job.set_logger(discarding_save_logger());
        job.initialize_from_json_partial(r#"{"inputFile":"input.pdf","json":"2"}"#)
            .unwrap();
        assert!(
            !job.creates_output(),
            "no destination has been assigned before the check runs"
        );

        job.check_configuration()
            .expect("--json without an output file defaults to standard output");

        assert!(
            job.creates_output(),
            "the implicit JSON destination must make the job an output producer"
        );
        assert_eq!(
            job.configuration.output_file.as_deref(),
            Some(Path::new("-")),
            "qpdf assigns the literal standard-output name"
        );
    }

    #[test]
    fn check_configuration_keeps_an_explicit_json_output_destination() {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(
            r#"{"inputFile":"input.pdf","outputFile":"out.json","json":"2"}"#,
        )
        .unwrap();

        job.check_configuration()
            .expect("explicit JSON output file");

        assert_eq!(
            job.configuration.output_file.as_deref(),
            Some(Path::new("out.json")),
            "an explicit destination must not be replaced by the JSON default"
        );
    }

    #[test]
    fn check_configuration_compares_the_implicit_json_destination_with_the_input() {
        // qpdf compares the assigned `-` with the input through
        // `QUtil::same_file` like any other destination
        // (`libqpdf/QPDFJob.cc:627-631`). A `-` that names no file in the
        // working directory simply fails to `stat`, so the check passes.
        let directory = tempfile::tempdir().expect("temporary directory");
        let input = directory.path().join("input.pdf");
        std::fs::write(&input, b"%PDF-1.4\n").expect("input file");
        let mut job = QPDFJob::new();
        job.set_logger(discarding_save_logger());
        job.set_input_file(input).expect("input path");
        job.configuration.json_version = Some(2);

        job.check_configuration()
            .expect("a `-` that names no file cannot alias the input");

        assert_eq!(
            job.configuration.output_file.as_deref(),
            Some(Path::new("-")),
            "the JSON default must survive the identity check"
        );
    }

    #[test]
    fn job_json_implicit_stdout_rejects_split_pages_before_writing() {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(
            r#"{"inputFile":"input.pdf","json":"2","splitPages":"1"}"#,
        )
        .unwrap();

        let error = job
            .check_configuration()
            .expect_err("implicit JSON stdout must be visible to split validation");
        assert_eq!(
            error.to_string(),
            "--split-pages may not be used when writing to standard output"
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
        let parse = |specs: &[&str]| {
            specs
                .iter()
                .map(|spec| parse_page_label_spec(spec.as_bytes()).unwrap())
                .collect::<Vec<_>>()
        };
        let entries =
            parse_job_page_labels(&parse(&["1:D", "2:a", "3:A", "4:r", "5:R", "6:"]), 6).unwrap();
        assert_eq!(entries.len(), 6);
        assert!(parse_page_label_spec(b"bad").is_err());
        assert!(parse_page_label_spec(b"q:D").is_err());
        assert!(parse_job_page_labels(&parse(&["2:D"]), 6).is_err());
        assert!(parse_job_page_labels(&parse(&["1:D", "1:a"]), 6).is_err());
        assert!(parse_job_page_labels(&parse(&["7:D"]), 6).is_err());
        assert!(parse_page_label_spec(b"1:X").is_err());
        assert!(parse_page_label_spec(b"1:D/foo").is_err());
        assert!(parse_page_label_spec(b"1:D/0").is_err());
        assert!(parse_page_label_spec(b"rx:D").is_err());
        let relative =
            parse_job_page_labels(&parse(&["1:D", "r2:a/2/prefix", "z:R//end"]), 6).unwrap();
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
        job.initialize_from_json_partial(r#"{"jsonObject":["2147483648"]}"#)
            .expect("jsonObject selector parsing is deferred until JSON output");
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
        assert_eq!(job.configuration.rotations[b"1".as_slice()].angle, 180);
    }

    #[test]
    fn job_json_rotation_keeps_qpdf_raw_range_and_relative_state() {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(&serde_json::json!({"rotate": "+90:1-5,x3"}).to_string())
            .unwrap();
        let rotation = &job.configuration.rotations[b"1-5,x3".as_slice()];
        assert_eq!(rotation.angle, 90);
        assert!(rotation.relative);
        assert_eq!(
            crate::qutil::parse_numrange(b"1-5,x3", 5).unwrap(),
            vec![1, 2, 4, 5]
        );
    }

    #[test]
    fn job_json_rotation_applies_qpdf_c_string_boundary_before_parsing() {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(
            &serde_json::json!({"rotate": "90:1\u{0000}junk"}).to_string(),
        )
        .unwrap();
        assert!(job.configuration.rotations.contains_key(b"1".as_slice()));
        assert!(!job
            .configuration
            .rotations
            .contains_key(b"1\0junk".as_slice()));
    }

    #[test]
    fn config_rotation_and_split_pages_store_qpdf_values() {
        let mut job = QPDFJob::new();
        {
            let mut configuration = job.config();
            configuration
                .rotate(b"+90:1")
                .expect("qpdf rotation parameter should parse");
            configuration
                .split_pages(b"2")
                .expect("qpdf split-pages parameter should parse");
        }

        let rotation = &job.configuration.rotations[b"1".as_slice()];
        assert_eq!(rotation.angle, 90);
        assert!(rotation.relative);
        assert_eq!(job.configuration.split_pages, Some(2));
    }

    #[test]
    fn config_rotation_replaces_duplicate_ranges_in_lexical_order() {
        let mut job = QPDFJob::new();
        {
            let mut configuration = job.config();
            configuration
                .rotate(b"90:1-3")
                .expect("first rotation parameter should parse");
            configuration
                .rotate(b"+90:2")
                .expect("relative rotation parameter should parse");
            configuration
                .rotate(b"90:1-3")
                .expect("duplicate rotation parameter should parse");
        }

        let keys: Vec<&[u8]> = job
            .configuration
            .rotations
            .keys()
            .map(Vec::as_slice)
            .collect();
        assert_eq!(keys, [b"1-3".as_slice(), b"2".as_slice()]);
        assert_eq!(job.configuration.rotations[b"1-3".as_slice()].angle, 90);
        assert!(!job.configuration.rotations[b"1-3".as_slice()].relative);
        assert_eq!(job.configuration.rotations[b"2".as_slice()].angle, 90);
        assert!(job.configuration.rotations[b"2".as_slice()].relative);
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
        assert_eq!(copy_job.run().unwrap(), JobExitCode::Success);
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

    #[test]
    fn partial_job_json_preserves_preconfigured_qpdf_state() {
        let mut job = QPDFJob::new();
        job.set_input_file("cli-input.pdf")
            .expect("the CLI input setter accepts the first input");
        job.set_password_mode(PasswordMode::HexBytes);
        job.set_password_is_hex_key(true);
        job.set_suppress_password_recovery(true);
        job.set_suppress_recovery(true);
        job.set_ignore_xref_streams(true);
        job.config().check_linearization();

        job.initialize_from_json_partial(r#"{"password":"json-password"}"#)
            .expect("partial JSON must layer onto the existing config");

        assert_eq!(
            job.configuration.input_file.as_deref(),
            Some(Path::new("cli-input.pdf"))
        );
        assert_eq!(job.configuration.password, b"json-password");
        assert_eq!(job.configuration.password_mode, PasswordMode::HexBytes);
        assert!(job.configuration.password_is_hex_key);
        assert!(job.configuration.suppress_password_recovery);
        assert!(job.configuration.suppress_recovery);
        assert!(job.configuration.ignore_xref_streams);
        assert!(job.configuration.check_linearization);
    }

    // `QPDFJob::write_json`/`write_json_with_version` moved in-crate:
    // `write_json`'s pub visibility has no rule-8 ground (qpdf's own
    // `QPDFJob::writeJSON` is private, flpdf-cli's `--json` route no longer
    // calls it directly since it joined `write_qpdf` in `flpdf-3yn9.48.150.3`,
    // and it is not documented as an intentional library feature in `lib.rs`),
    // so these tests -- which exercise the job lifecycle boundary the method
    // owns (warning drain, completion suffix, exit code) -- moved from
    // `tests/job_lifecycle_tests.rs` alongside the visibility narrowing
    // (`flpdf-3yn9.48.182`).
    const COMPLETE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.3"},
    {
      "obj:1 0 R": {"value": {"/Pages": "2 0 R", "/Type": "/Catalog"}},
      "obj:2 0 R": {"value": {"/Count": 0, "/Kids": [], "/Type": "/Pages"}},
      "trailer": {"value": {"/Root": "1 0 R", "/Size": 3}}
    }
  ]
}"#;

    const ROOTLESS_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2, "pdfversion": "1.3"},
    {"trailer": {"value": {}}}
  ]
}"#;

    const UPDATE_JSON: &[u8] = br#"{
  "qpdf": [
    {"jsonversion": 2},
    {"obj:1 0 R": {"value": {"/Marker": true, "/Pages": "2 0 R", "/Type": "/Catalog"}}}
  ]
}"#;

    #[derive(Default)]
    struct JsonSinkState {
        bytes: Vec<u8>,
    }

    struct JsonRecordingSink {
        state: std::sync::Arc<std::sync::Mutex<JsonSinkState>>,
    }

    struct JsonFailingSink;

    // `identifier`/`finish` on both sinks below are `Pipeline` trait-contract
    // boilerplate the job's warning/info logger routes never call (warnings
    // are forwarded by message text alone, and the logger never finishes a
    // diagnostic sink mid-job) -- only `write` is exercised, so each is
    // individually excluded from coverage below.
    impl Pipeline for JsonRecordingSink {
        // cov:ignore-start: never called; see the block comment above.
        fn identifier(&self) -> &str {
            "json lifecycle test sink"
        }
        // cov:ignore-end

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.state.lock().unwrap().bytes.extend_from_slice(data);
            Ok(())
        }

        // cov:ignore-start: never called; see the block comment above.
        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    impl Pipeline for JsonFailingSink {
        // cov:ignore-start: never called; see the block comment above.
        fn identifier(&self) -> &str {
            "json lifecycle failing sink"
        }
        // cov:ignore-end

        fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
            Err(crate::pipeline::PipelineError::runtime(
                "warning sink failed",
            ))
        }

        // cov:ignore-start: never called; see the block comment above.
        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    fn json_logger_with_warning_sink(
    ) -> (QPDFLogger, std::sync::Arc<std::sync::Mutex<JsonSinkState>>) {
        let logger = QPDFLogger::create();
        let state = std::sync::Arc::new(std::sync::Mutex::new(JsonSinkState::default()));
        logger.set_warn(Some(PipelineHandle::new(JsonRecordingSink {
            state: std::sync::Arc::clone(&state),
        })));
        (logger, state)
    }

    fn json_logger_with_info_sink() -> (QPDFLogger, std::sync::Arc<std::sync::Mutex<JsonSinkState>>)
    {
        let logger = QPDFLogger::create();
        let state = std::sync::Arc::new(std::sync::Mutex::new(JsonSinkState::default()));
        logger.set_info(Some(PipelineHandle::new(JsonRecordingSink {
            state: std::sync::Arc::clone(&state),
        })));
        (logger, state)
    }

    #[test]
    fn json_create_update_and_write_share_one_job_lifecycle() {
        let mut job = QPDFJob::new();
        let mut pdf = job
            .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
            .expect("complete JSON input");
        assert_eq!(pdf.root_ref(), Some(ObjectRef::new(1, 0)));

        job.update_from_json(&mut pdf, Cursor::new(UPDATE_JSON), "update.json")
            .expect("partial JSON update");
        assert_eq!(
            pdf.get_object_handle(ObjectRef::new(1, 0))
                .try_get_key(b"/Marker")
                .unwrap()
                .as_boolean(),
            Some(true)
        );

        let mut output = Vec::new();
        let status = job
            .write_json(
                &mut pdf,
                JsonJobOptions {
                    decode_level: JsonDecodeLevel::None,
                    stream_data: JsonStreamData::None,
                    stream_prefix: None,
                    keys: &[],
                    objects: &[],
                },
                JsonJobOutput::Stdout(&mut output),
            )
            .expect("JSON output");

        assert_eq!(status, JobExitCode::Success);
        assert!(String::from_utf8_lossy(&output).contains("\"jsonversion\": 2"));
    }

    /// A library caller that turns verbosity on through the job gets the same
    /// `wrote file` report the CLI flag produces.
    ///
    /// qpdf gates it on `m->verbose` inside `writeOutfile`
    /// (`libqpdf/QPDFJob.cc:3057-3062`), which `QPDFJob::Config::verbose` sets, so
    /// the JSON route must read the job-owned setting rather than requiring a
    /// separate value.
    #[test]
    fn json_write_reports_the_written_file_for_a_job_verbose_caller() {
        let (logger, state) = json_logger_with_info_sink();
        let directory = tempfile::tempdir().expect("tempdir");
        let output_path = directory.path().join("out.json");
        let mut file = std::fs::File::create(&output_path).expect("create output");
        let mut pdf = Pdf::open(BufReader::new(
            File::open(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
            )
            .expect("committed minimal fixture"),
        ))
        .expect("minimal fixture parses");

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.set_verbose(true);
        let status = job
            .write_json(
                &mut pdf,
                JsonJobOptions {
                    decode_level: JsonDecodeLevel::None,
                    stream_data: JsonStreamData::None,
                    stream_prefix: None,
                    keys: &[],
                    objects: &[],
                },
                JsonJobOutput::File {
                    filename: &output_path,
                    writer: &mut file,
                },
            )
            .expect("JSON output");

        assert_eq!(status, JobExitCode::Success);
        let info = String::from_utf8(state.lock().expect("sink state").bytes.clone())
            .expect("info output is utf-8");
        assert!(
            info.contains(&format!("wrote file {}", output_path.display())),
            "the job-owned verbose setting must reach the JSON report: {info:?}"
        );
    }

    #[test]
    fn json_write_derives_file_completion_suffix_from_output_destination() {
        let (logger, state) = json_logger_with_warning_sink();
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.record_warnings();
        let mut pdf = job
            .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
            .expect("complete JSON input");
        let mut output = Vec::new();
        let filename = Path::new("output.json");

        let status = job
            .write_json(
                &mut pdf,
                JsonJobOptions {
                    decode_level: JsonDecodeLevel::None,
                    stream_data: JsonStreamData::None,
                    stream_prefix: None,
                    keys: &[],
                    objects: &[],
                },
                JsonJobOutput::File {
                    filename,
                    writer: &mut output,
                },
            )
            .expect("JSON output");

        assert_eq!(status, JobExitCode::Warning);
        assert_eq!(
            state.lock().unwrap().bytes,
            b"qpdf: operation succeeded with warnings; resulting file may have some problems\n"
        );
    }

    #[test]
    fn json_write_reports_completion_sink_errors() {
        let logger = QPDFLogger::create();
        logger.set_warn(Some(PipelineHandle::new(JsonFailingSink)));
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.record_warnings();
        let mut pdf = job
            .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
            .expect("complete JSON input");
        let mut output = Vec::new();

        let error = job
            .write_json(
                &mut pdf,
                JsonJobOptions {
                    decode_level: JsonDecodeLevel::None,
                    stream_data: JsonStreamData::None,
                    stream_prefix: None,
                    keys: &[],
                    objects: &[],
                },
                JsonJobOutput::Stdout(&mut output),
            )
            .expect_err("completion warning sink failure must be reported");

        assert!(matches!(
            error,
            JsonJobError::Completion(Error::System(message))
                if message == "warning sink failed"
        ));
    }

    #[test]
    fn json_write_failure_does_not_emit_completion_summary() {
        let (logger, state) = json_logger_with_warning_sink();
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.record_warnings();
        let mut pdf = Pdf::create_from_json(Cursor::new(ROOTLESS_JSON), "input.json")
            .expect("rootless JSON input");
        let mut output = Vec::new();

        let error = job
            .write_json(
                &mut pdf,
                JsonJobOptions {
                    decode_level: JsonDecodeLevel::None,
                    stream_data: JsonStreamData::None,
                    stream_prefix: None,
                    keys: &[],
                    objects: &[],
                },
                JsonJobOutput::Stdout(&mut output),
            )
            .expect_err("serializer failure must abort before completion");

        assert!(matches!(error, JsonJobError::Output(_)));
        assert!(state.lock().unwrap().bytes.is_empty());
    }

    #[test]
    fn json_job_output_matches_qpdf_11_9_json_input_route() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/json-input/complete.json");
        let expected = match std::process::Command::new("qpdf")
            .args(["--json-input", "--json=2"])
            .arg(&path)
            .arg("-")
            .output()
        {
            Ok(output) if output.status.success() => output.stdout,
            // cov:ignore-start: this test environment always has a working qpdf
            // 11.9.0 (many other oracle tests in this crate rely on it), so
            // neither the failure nor the unavailable branch can be exercised
            // without uninstalling qpdf.
            Ok(output) => panic!(
                "qpdf JSON route failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => {
                eprintln!("skipping qpdf differential: {error}");
                return;
            } // cov:ignore-end
        };

        let mut job = QPDFJob::new();
        let mut pdf = job
            .create_from_json(
                File::open(&path).expect("complete JSON fixture"),
                path.display().to_string(),
            )
            .expect("complete JSON input");
        let mut actual = Vec::new();
        job.write_json(
            &mut pdf,
            JsonJobOptions {
                decode_level: JsonDecodeLevel::Generalized,
                stream_data: JsonStreamData::None,
                stream_prefix: None,
                keys: &[],
                objects: &[],
            },
            JsonJobOutput::Stdout(&mut actual),
        )
        .expect("JSON output");

        assert_eq!(actual, expected);
    }
}
