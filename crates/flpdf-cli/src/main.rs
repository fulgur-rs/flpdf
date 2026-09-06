#![forbid(unsafe_code)]

mod arg_parser;

use clap::{ArgGroup, Args as ClapArgs, CommandFactory, Parser, Subcommand, ValueEnum};
use flpdf::fix_qdf;
use flpdf::job::{
    apply_rotate_to_pages, copy_duplicate_page_annotations, flatten_rotation_on_pages,
    should_remove_unreferenced_resources, AttachmentAddOptions, AttachmentCopyOptions,
    AttachmentCopySource, CheckError, FlattenAnnotationsMode, ImageOptimizationOptions,
    JobExitCode, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData, PageSpecInput,
    PageSpecJobOutput, QPDFJob, RemoveUnreferencedResources, SplitPageOptions,
};
use flpdf::pipeline::{FlateAction, Pipeline, PipelineHandle, PlFlate, PlStdioFile};
use flpdf::qutil::same_file as qpdf_same_file;
use flpdf::writer::DecodeLevel as StreamDecodeLevel;
use flpdf::{
    json_inspect::{DecodeLevel, JsonKey, JsonObjectSelector},
    normalize_content_stream, pages, parse_pdf_version, AcroFormDocumentHelper, CompressStreams,
    CopyEncryptionSource, EncryptMethod, EncryptParams, Error, NewlineBeforeEndstream,
    ObjectHandle, ObjectKeyAlg, ObjectRef, ObjectStreamMode, PageDocumentHelper, PageObjectHelper,
    PasswordMode, Pdf, PdfOpenOptions, PdfVersion, PdfWriter, PermissionsConfig, PrintPermission,
    QPDFLogger, R2PermissionsConfig, StreamDataMode, UsageError, WriterConfiguration,
};
use flpdf::{
    pages::tree_rebuild::{rebuild_page_tree, RebuildResult},
    CombinedPage, InputSpec, PageRange, RotateSpec,
};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

struct PipelineWriter {
    pipeline: PipelineHandle,
}

impl Write for PipelineWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.pipeline.write(data).map_err(std::io::Error::other)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// CLI-owned writer configuration. The library's lower-level option bridge is
/// intentionally private; all CLI output is configured through PdfWriter.
#[derive(Debug, Clone)]
struct WriterOptions {
    object_streams: ObjectStreamMode,
    /// Explicit `--compress-streams` value; `None` preserves qpdf's default
    /// without overriding an explicitly selected stream-data mode.
    compress_streams: Option<CompressStreams>,
    content_normalization: bool,
    content_normalization_set: bool,
    qdf: bool,
    preserve_unreferenced_objects: bool,
    newline_before_endstream: NewlineBeforeEndstream,
    stream_data: Option<StreamDataMode>,
    decode_level: StreamDecodeLevel,
    decode_level_set: bool,
    recompress_flate: bool,
    compression_level: Option<i32>,
    progress: bool,
    static_id: bool,
    deterministic_id: bool,
    static_aes_iv: bool,
    no_original_object_ids: bool,
    input_version_floor: Option<PdfVersion>,
    min_version: Option<String>,
    min_extension_level: Option<i64>,
    force_version: Option<String>,
    force_extension_level: Option<i64>,
    encrypt: Option<EncryptParams>,
    copy_encryption: Option<CopyEncryptionSource>,
    preserve_encryption: bool,
    password_mode: PasswordMode,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            object_streams: ObjectStreamMode::Preserve,
            compress_streams: None,
            content_normalization: false,
            content_normalization_set: false,
            qdf: false,
            preserve_unreferenced_objects: false,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            stream_data: None,
            decode_level: StreamDecodeLevel::None,
            decode_level_set: false,
            recompress_flate: false,
            compression_level: None,
            progress: false,
            static_id: false,
            deterministic_id: false,
            static_aes_iv: false,
            no_original_object_ids: false,
            input_version_floor: None,
            min_version: None,
            min_extension_level: None,
            force_version: None,
            force_extension_level: None,
            encrypt: None,
            copy_encryption: None,
            preserve_encryption: true,
            password_mode: PasswordMode::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CliVersionOptions {
    min: Option<(String, i64)>,
    force: Option<(String, i64)>,
}

fn parse_cli_version_options(
    min: Option<&str>,
    force: Option<&str>,
) -> CliResult<CliVersionOptions> {
    let parse = |name: &str, value: Option<&str>| -> CliResult<Option<(String, i64)>> {
        value
            .map(|value| {
                flpdf::parse_pdf_version_spec(value)
                    .ok_or_else(|| format!("invalid {name} value: {value:?}").into())
            })
            .transpose()
    };
    Ok(CliVersionOptions {
        min: parse("--min-version", min)?,
        force: parse("--force-version", force)?,
    })
}

fn apply_cli_version_options(options: &mut WriterOptions, versions: &CliVersionOptions) {
    if let Some((version, extension_level)) = &versions.min {
        options.min_version = Some(version.clone());
        options.min_extension_level = Some(*extension_level);
    }
    if let Some((version, extension_level)) = &versions.force {
        options.force_version = Some(version.clone());
        options.force_extension_level = Some(*extension_level);
    }
}

/// Accumulate qpdf's `max_input_version` floor without touching the explicit
/// raw `--min-version` option. qpdf gathers input versions in `QPDFJob` and
/// applies that floor to the writer before applying the explicit minimum
/// (`libqpdf/QPDFJob.cc:1695-1716,2907-2924`).
fn update_input_version_floor<R: Read + Seek>(
    floor: &mut Option<PdfVersion>,
    pdf: &mut Pdf<R>,
) -> CliResult<()> {
    if let Some(source_version) = parse_pdf_version(pdf.version()) {
        let candidate = PdfVersion::new(
            source_version.major(),
            source_version.minor(),
            pdf.adobe_extension_level()?.unwrap_or(0),
        );
        if floor.is_none_or(|current| current < candidate) {
            *floor = Some(candidate);
        }
    }
    Ok(())
}

fn apply_cli_decode_level(options: &mut WriterOptions, decode_level: Option<CliDecodeLevel>) {
    if let Some(level) = decode_level {
        options.decode_level = level.into();
        options.decode_level_set = true;
    }
}

/// Build the writer settings shared by top-level rewrite-shaped routes.
///
/// qpdf applies these settings after each top-level transformation, including
/// `--add-attachment`, `--remove-attachment`, and `--copy-attachments-from`
/// (`QPDFJob.cc:484-507,2046-2248,2847-2945`). Keeping the assembly here
/// prevents an operation-specific writer from silently dropping a setting
/// that the canonical rewrite path already honors.
fn top_level_writer_options(
    args: &Cli,
    normalize_content: bool,
    compression_level: Option<i32>,
    version_options: &CliVersionOptions,
) -> WriterOptions {
    let mut options = WriterOptions {
        static_id: args.static_id,
        deterministic_id: args.deterministic_id,
        static_aes_iv: args.static_aes_iv,
        no_original_object_ids: args.no_original_object_ids,
        preserve_unreferenced_objects: args.preserve_unreferenced,
        progress: args.progress,
        recompress_flate: args.recompress_flate,
        compression_level,
        object_streams: args.object_streams.into(),
        stream_data: args.stream_data.map(Into::into),
        content_normalization: normalize_content,
        content_normalization_set: args.normalize_content.is_some(),
        qdf: args.qdf,
        newline_before_endstream: args.newline_before_endstream.into(),
        password_mode: args.password.password_mode.into(),
        ..WriterOptions::default()
    };
    apply_cli_decode_level(&mut options, args.decode_level);
    apply_cli_version_options(&mut options, version_options);

    if let Some(ref cs) = args.compress_streams {
        match cs.as_str() {
            "y" => options.compress_streams = Some(CompressStreams::Yes),
            "n" => options.compress_streams = Some(CompressStreams::No),
            other => {
                emit_logger_error(format!(
                    "flpdf: --compress-streams must be y or n, got: {:?}\n",
                    other
                ));
                std::process::exit(2);
            }
        }
    }

    apply_encryption_options(
        &mut options,
        args.raw_encrypt.as_deref(),
        args.copy_encryption.as_deref(),
        args.raw_encryption_file_password.as_deref(),
        &args.password,
        args.no_warn,
    );
    if args.decrypt {
        options.preserve_encryption = false;
    }
    options
}

fn parse_compression_level(value: Option<&str>) -> CliResult<Option<i32>> {
    value.map(qpdf_selector_integer).transpose()
}

/// Parse the unsigned image thresholds through the same qpdf `strtoull` /
/// `QIntC::to_uint` boundary used by `QPDFJob::Config::oiMin*` and
/// `iiMinBytes` (`libqpdf/QPDFJob_config.cc:232-234,422-445`).
fn parse_image_uint(value: Option<&str>) -> CliResult<Option<u32>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(Some(0));
    }
    let parsed = QPDFJob::parse_collate(value)?
        .into_iter()
        .next()
        .unwrap_or(0);
    // QPDFJob::parse_collate uses the same QUtil::string_to_uint conversion
    // and rejects values above u32::MAX before returning the usize.
    Ok(Some(parsed as u32))
}

fn image_optimization_options(
    keep_inline_images: bool,
    oi_min_width: Option<&str>,
    oi_min_height: Option<&str>,
    oi_min_area: Option<&str>,
    ii_min_bytes: Option<&str>,
) -> CliResult<ImageOptimizationOptions> {
    let mut options = ImageOptimizationOptions::default();
    if let Some(value) = parse_image_uint(oi_min_width)? {
        options.min_width = value;
    }
    if let Some(value) = parse_image_uint(oi_min_height)? {
        options.min_height = value;
    }
    if let Some(value) = parse_image_uint(oi_min_area)? {
        options.min_area = value;
    }
    if let Some(value) = parse_image_uint(ii_min_bytes)? {
        options.inline_min_bytes = value as usize;
    }
    options.keep_inline_images = keep_inline_images;
    Ok(options)
}

/// Translate the CLI's effective writer options into the reusable library
/// configuration that qpdf reapplies to every split-page output writer.
fn writer_configuration(
    options: &WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
) -> CliResult<WriterConfiguration> {
    let mut configuration = WriterConfiguration::default();
    configuration.set_object_stream_mode(options.object_streams);
    if let Some(mode) = options.stream_data {
        configuration.set_stream_data_mode(mode);
    }
    if let Some(mode) = options.compress_streams {
        configuration.set_compress_streams(matches!(mode, CompressStreams::Yes));
    }
    if options.decode_level_set {
        configuration.set_decode_level(options.decode_level);
    }
    configuration.set_recompress_flate(options.recompress_flate);
    if let Some(level) = options.compression_level {
        configuration.set_compression_level(level);
    }
    configuration.set_qdf_mode(options.qdf && !linearize);
    configuration.set_linearization(linearize);
    if let Some(path) = linearize_pass1 {
        configuration.set_linearization_pass1_filename(path.to_path_buf());
    }
    if options.content_normalization_set {
        configuration.set_content_normalization(options.content_normalization);
    }
    configuration.set_preserve_unreferenced_objects(options.preserve_unreferenced_objects);
    configuration.set_newline_before_endstream(matches!(
        options.newline_before_endstream,
        NewlineBeforeEndstream::Yes
    ));
    configuration.set_static_id(options.static_id);
    configuration.set_deterministic_id(options.deterministic_id);
    configuration.set_static_aes_iv(options.static_aes_iv);
    configuration.set_suppress_original_object_ids(options.no_original_object_ids);
    configuration.set_preserve_encryption(options.preserve_encryption);
    if let Some(version) = options.input_version_floor {
        let (version, extension_level) = version.get_version();
        configuration.set_minimum_pdf_version(version, extension_level);
    }
    if let Some(version) = options.min_version.as_deref() {
        configuration.set_minimum_pdf_version(version, options.min_extension_level.unwrap_or(0));
    }
    if let Some(version) = options.force_version.as_deref() {
        configuration.force_pdf_version(version, options.force_extension_level.unwrap_or(0));
    }
    if let Some(params) = options.encrypt.clone() {
        configuration.set_encryption_parameters(params);
    }
    if let Some(source) = options.copy_encryption.clone() {
        configuration.copy_encryption_parameters(source);
    }
    let warning_count = configuration.normalize_encryption_passwords(options.password_mode)?;
    for _ in 0..warning_count {
        emit_logger_error(format!(
            "{}: WARNING: supplied password looks like a Unicode password with characters not allowed in passwords for 40-bit and 128-bit encryption; most readers will not be able to open this file with the supplied password. (Use --password-mode=bytes to suppress this warning and use the password anyway.)\n",
            progname()
        ));
    }
    Ok(configuration)
}

fn configure_pdf_writer<R: Read + Seek + 'static>(
    writer: &mut PdfWriter<'_, R>,
    options: &WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
) -> CliResult<()> {
    writer_configuration(options, linearize, linearize_pass1)?.apply_to(writer);
    Ok(())
}

/// Attach qpdf's logger-backed progress reporter to a direct CLI writer.
///
/// The event accounting remains owned by `PdfWriter`; `QPDFJob` owns the
/// message prefix, output identity, and info/save routing just as qpdf's
/// `setWriterOptions` does (`libqpdf/QPDFJob.cc:2926-2935`).
fn configure_cli_progress<R: Read + Seek + 'static>(
    writer: &mut PdfWriter<'_, R>,
    output: &Path,
    enabled: bool,
) -> CliResult<()> {
    if !enabled {
        return Ok(());
    }
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_output_file(output.to_path_buf())?;
    job.set_progress(true);
    job.configure_writer_progress(writer);
    Ok(())
}

fn write_with_pdf_writer<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    output: &Path,
    standard_output: &mut Option<PipelineWriter>,
    options: &WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
) -> CliResult<()> {
    let mut writer = PdfWriter::new(pdf);
    configure_pdf_writer(&mut writer, options, linearize, linearize_pass1)?;
    configure_cli_progress(&mut writer, output, options.progress)?;
    if let Some(sink) = standard_output.take() {
        writer.set_output_writer(sink)?;
    } else {
        writer.set_output_file(output)?;
    }
    writer.write()?;
    Ok(())
}

fn write_qpdf_to_memory<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    output: &Path,
    options: &WriterOptions,
    chunks_linearized: bool,
) -> CliResult<Vec<u8>> {
    let mut writer = PdfWriter::new(pdf);
    // qpdf's linearized writers clear QDF mode before deriving QDF's
    // decode/uncompress defaults (`QPDFWriter.cc:2068-2080`). This memory
    // rewrite is flpdf's internal preparation for split chunks, so when those
    // chunks will be linearized it must not apply QDF either; otherwise a
    // `--stream-data=preserve` chunk would lose the source filters the QDF
    // pass decoded.
    let intermediate = WriterOptions {
        qdf: options.qdf && !chunks_linearized,
        ..options.clone()
    };
    configure_pdf_writer(&mut writer, &intermediate, false, None)?;
    configure_cli_progress(&mut writer, output, options.progress)?;
    writer.set_output_memory()?;
    writer.write()?;
    Ok(writer.get_buffer()?)
}

// ---------------------------------------------------------------------------
// qpdf-compatible exit-code infrastructure
//
// Source: qpdf manual §"Exit Status"
//   https://qpdf.readthedocs.io/en/stable/cli.html#exit-status
// Confirmed by qpdf C header (qpdf/include/qpdf/Constants.h):
//   qpdf_exit_success  = 0   (no errors or warnings)
//   qpdf_exit_error    = 2   (errors found)
//   qpdf_exit_warning  = 3   (warnings found, no errors)
//
// Note: exit code 1 is intentionally unused by qpdf (shells use it for
// command-not-found); flpdf follows the same convention.
//
// Each subcommand expresses its exit-code semantics by constructing a
// `CliExitError` with the appropriate
// `ExitCode` variant — the enum is generic enough for `--is-encrypted` (0/2)
// and `--requires-password` (0/2/3) once those subcommands are added.
// ---------------------------------------------------------------------------

/// qpdf-compatible CLI exit codes.
///
/// Matches `qpdf_exit_code_e` from `qpdf/include/qpdf/Constants.h`:
/// - `Ok` = 0: success, no errors or warnings
/// - `Errors` = 2: errors detected (file invalid / unprocessable)
/// - `Warnings` = 3: warnings found but no errors (recoverable issues)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// 0 — no errors or warnings detected.
    Ok = 0,
    /// 2 — errors found; file could not be fully processed.
    Errors = 2,
    /// 3 — warnings found (recoverable issues) but no hard errors.
    Warnings = 3,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// An error type that carries an explicit [`ExitCode`] so that `main()` can
/// use that code rather than defaulting to 2.
///
/// Use this (instead of a plain string error) whenever a CLI path needs to
/// communicate a specific exit code to the shell (e.g. `--check` warning-only
/// result → 3).  All other `CliResult::Err` values fall back to exit 2 via
/// the existing generic handler in `main()`.
#[derive(Debug)]
pub struct CliExitError {
    /// The exit code to pass to `std::process::exit`.
    pub code: ExitCode,
    /// Human-readable message printed to stderr.
    pub message: String,
}

impl std::fmt::Display for CliExitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliExitError {}

#[derive(Debug)]
struct CliPathError {
    path: Vec<u8>,
    operation: Option<&'static str>,
    message: String,
    source: Box<dyn std::error::Error>,
}

impl std::fmt::Display for CliPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(operation) = self.operation {
            write!(formatter, "{operation} ")?;
        }
        write!(
            formatter,
            "{}: {}",
            String::from_utf8_lossy(&self.path),
            self.message
        )
    }
}

impl std::error::Error for CliPathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug, Parser)]
#[command(name = "flpdf")]
#[command(about = "Pure Rust qpdf-style PDF tool")]
// Top-level option flags (--json, --check, --linearize, …) are mutually
// exclusive with subcommands. Without this, `flpdf --json rewrite in out`
// would parse as the rewrite subcommand while silently keeping --json,
// never reaching the JSON branch. Conflicting instead surfaces the
// ambiguity as a clean usage error.
#[command(args_conflicts_with_subcommands = true)]
// The five attachment operations are dispatched by an ordered `else if`
// chain in `main`, so supplying two at once would silently run only the
// first. Make them mutually exclusive at the parser level: clap rejects
// e.g. `--add-attachment … -- --copy-attachments-from …` with a usage
// error instead of discarding the second operation. (`--verbose` is a
// sub-modifier, not an operation, so it is intentionally NOT a member of
// this group.)
#[command(group(
    ArgGroup::new("attachment_op")
        .multiple(false)
        .args([
            "add_attachment",
            "remove_attachment",
            "list_attachments",
            "show_attachment",
            "copy_attachments_from",
        ])
))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // Legacy options kept for compatibility.
    #[arg(long, conflicts_with = "output")]
    check: bool,
    /// Suppress warning delivery while retaining qpdf's warning exit status.
    #[arg(long)]
    no_warn: bool,
    /// Check whether the input's linearization hint tables are correct
    /// (qpdf --check-linearization).
    #[arg(
        long = "check-linearization",
        conflicts_with_all = [
            "check",
            "show_object",
            "show_npages",
            "show_pages",
            "show_xref",
            "show_linearization",
            "show_encryption",
            "job_json_file",
            "json",
            "json_input",
            "update_from_json",
            "json_output",
            "json_key",
            "json_object",
            "json_stream_data",
            "json_stream_prefix",
            "linearize",
            "static_id",
            "deterministic_id",
            "static_aes_iv",
            "remove_restrictions",
            "decrypt",
            "qdf",
            "preserve_unreferenced",
            "coalesce_contents",
            "pages",
            "rotate",
            "split_pages",
            "collate",
            "empty",
            "overlay",
            "underlay",
            "add_attachment",
            "remove_attachment",
            "list_attachments",
            "show_attachment",
            "copy_attachments_from",
            "encrypt",
            "copy_encryption",
            "encryption_file_password",
            "flatten_annotations",
            "generate_appearances",
            "output",
        ]
    )]
    check_linearization: bool,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    #[arg(long, conflicts_with = "output")]
    show_object: Option<String>,
    /// Emit stored stream bytes for `--show-object` (qpdf --raw-stream-data).
    #[arg(long = "raw-stream-data", requires = "show_object")]
    raw_stream_data: bool,
    /// Emit decoded stream bytes for `--show-object` (qpdf --filtered-stream-data).
    #[arg(long = "filtered-stream-data", requires = "show_object")]
    filtered_stream_data: bool,
    #[arg(long, conflicts_with = "output")]
    show_npages: bool,
    #[arg(long, conflicts_with = "output")]
    show_pages: bool,
    /// Include image XObject details in `--show-pages` output (qpdf
    /// `--with-images`). This is a modifier and does not itself select an
    /// inspection mode.
    #[arg(long = "with-images")]
    with_images: bool,
    #[arg(long, conflicts_with = "output")]
    show_xref: bool,
    #[arg(long, conflicts_with = "output")]
    show_linearization: bool,
    /// Show encryption parameters on the qpdf-compatible top-level surface.
    /// This is the argv form used by qtest (`qpdf --show-encryption FILE`);
    /// the native `show-encryption` subcommand dispatches to the same
    /// inspection implementation below.
    ///
    /// qpdf marks `showEncryption()` as `require_outfile = false`
    /// (`QPDFJob_config.cc:554-559`), so `checkConfiguration()`
    /// (`QPDFJob.cc:593-594`) rejects an output file argument outright with
    /// "no output file may be given for this option", regardless of what
    /// other flags accompany it.
    #[arg(long = "show-encryption", conflicts_with = "output")]
    show_encryption: bool,

    /// Exit 0 when INPUT is encrypted and 2 otherwise (qpdf
    /// `--is-encrypted`). This is an inspection-only top-level argv option;
    /// it does not authenticate the input or produce output.
    #[arg(
        long = "is-encrypted",
        conflicts_with_all = ["requires_password", "output"]
    )]
    is_encrypted: bool,

    /// Exit 3 when INPUT is encrypted and the supplied password opens it, 0
    /// when another password is required, and 2 when it is not encrypted
    /// (qpdf `--requires-password`).
    #[arg(
        long = "requires-password",
        conflicts_with_all = ["is_encrypted", "output"]
    )]
    requires_password: bool,

    /// Include the derived encryption key in JSON's `encrypt.parameters.key`
    /// field (qpdf `--show-encryption-key`).
    #[arg(long = "show-encryption-key")]
    show_encryption_key: bool,

    /// Run a complete qpdf job JSON file through the production QPDFJob
    /// lifecycle (qpdf `--job-json-file`).
    #[arg(long = "job-json-file", value_name = "PATH", require_equals = true)]
    job_json_file: Option<PathBuf>,

    // ── JSON inspection flags ─────────────────────────────────────────────
    // These mirror qpdf's --json / --json-output / --json-key / --json-object
    // / --json-stream-data / --json-stream-prefix flags.
    /// Enable qpdf JSON output mode. Pass `--json` alone or `--json=1|2`.
    /// The value, when given, must be supplied with an equals sign to avoid
    /// ambiguity with the positional input argument.
    // JSON mode is exclusive with the other top-level inspection / write
    // modes and with the OUTPUT positional. Without these conflicts, e.g.
    // `flpdf --json --check in` or `flpdf --json in out` would silently
    // ignore the second mode (run_json wins in main's dispatch chain).
    // Listing them as clap conflicts surfaces the mistake as a usage error
    // instead of doing one thing while the user asked for two.
    #[arg(long, num_args = 0..=1, default_missing_value = "2",
          require_equals = true,
          value_name = "VERSION", value_parser = ["1", "2", "latest"],
          conflicts_with_all = [
              "check", "linearize", "static_id", "deterministic_id", "static_aes_iv",
              "show_object",
              "show_npages", "show_pages", "show_xref", "show_linearization",
              "show_encryption",
              "is_encrypted", "requires_password",
              "compress_streams", "recompress_flate", "compression_level",
              "linearize_pass1", "remove_restrictions",
              "decrypt", "encrypt", "copy_encryption",
              "add_attachment", "remove_attachment", "list_attachments",
              "show_attachment", "copy_attachments_from",
              "no_original_object_ids", "qdf", "coalesce_contents",
              "flatten_annotations", "generate_appearances",
              "preserve_unreferenced",
          ],
          help = "Generate JSON v2 output (qpdf --json compatible)")]
    json: Option<String>,

    /// Treat INPUT as a qpdf JSON v2 document instead of a PDF
    /// (qpdf `--json-input`). The imported document then follows the same
    /// rewrite, page-operation, or JSON-output job routes as a PDF input.
    #[arg(
        long = "json-input",
        conflicts_with_all = [
            "show_object",
            "show_linearization", "list_attachments", "show_attachment",
            "remove_attachment", "add_attachment", "copy_attachments_from",
        ],
        help = "Treat INPUT as qpdf JSON v2 (qpdf --json-input)"
    )]
    json_input: bool,

    /// Apply a partial qpdf JSON v2 update to INPUT before any other job
    /// transformations (qpdf `--update-from-json`). qpdf requires the equals
    /// form so the JSON path cannot be confused with the PDF input path.
    #[arg(
        long = "update-from-json",
        value_name = "QPDF-JSON",
        require_equals = true,
        conflicts_with_all = [
            "show_object",
            "show_linearization", "list_attachments", "show_attachment",
            "remove_attachment", "add_attachment", "copy_attachments_from",
        ],
        help = "Apply a qpdf JSON update before processing (qpdf --update-from-json)"
    )]
    update_from_json: Option<PathBuf>,

    /// Write JSON output to PATH instead of stdout.
    #[arg(
        long = "json-output",
        num_args = 0..=1,
        default_missing_value = "2",
        require_equals = true,
        value_name = "VERSION",
        value_parser = ["1", "2", "latest"],
        conflicts_with_all = [
            "check", "linearize", "static_id", "deterministic_id", "static_aes_iv",
            "show_object",
            "show_npages", "show_pages", "show_xref", "show_linearization",
            "show_encryption",
            "is_encrypted", "requires_password",
            "compress_streams", "recompress_flate", "compression_level",
            "linearize_pass1", "remove_restrictions",
            "decrypt", "encrypt", "copy_encryption",
            "add_attachment", "remove_attachment", "list_attachments",
            "show_attachment", "copy_attachments_from",
            "no_original_object_ids", "qdf", "coalesce_contents",
            "flatten_annotations", "generate_appearances",
            "preserve_unreferenced",
        ],
        help = "Generate qpdf JSON output; VERSION defaults to 2 and the output file is positional"
    )]
    json_output: Option<String>,

    /// Validate generated JSON against qpdf's own output schema. This is a
    /// testing option used by qpdf's qtest suite.
    #[arg(long = "test-json-schema")]
    test_json_schema: bool,

    /// Limit JSON output to the specified top-level key (repeatable).
    /// Valid JSON v2 keys: acroform, attachments, encrypt, outlines,
    /// pagelabels, pages, qpdf.
    #[arg(
        long = "json-key",
        value_name = "KEY",
        help = "This option is repeatable. If given, only the specified \
                top-level keys will be included in the JSON output. \
                Otherwise, all keys will be included."
    )]
    json_key: Vec<String>,

    /// Restrict JSON qpdf section to a single object (repeatable).
    /// Format: `trailer`, `N`, or `N,G`.
    #[arg(
        long = "json-object",
        value_name = "SELECTOR",
        help = "This option is repeatable. If given, only specified objects \
                will be shown in the \"qpdf\" key of the JSON output. \
                Otherwise, all objects will be shown. Format: trailer, N, \
                or N,G."
    )]
    json_object: Vec<String>,

    /// How to include stream data in JSON output.
    /// `none` (default) omits data; `inline` base64-encodes it; `file` writes
    /// side files named `<prefix>-NNN`.
    #[arg(
        long = "json-stream-data",
        value_name = "MODE",
        help = "When used with --json, this option controls whether streams \
                in json output should be omitted, written inline \
                (base64-encoded), or written to a file. If \"file\" is \
                chosen, the file name is the --json-stream-prefix value \
                appended with -nnn where nnn is the object number. The \
                default is \"none\"."
    )]
    json_stream_data: Option<String>,

    /// Prefix for side-file names when --json-stream-data=file.
    /// With --json-output, defaults to the JSON output path. With JSON on
    /// stdout, file stream data requires an explicit non-empty prefix.
    #[arg(
        long = "json-stream-prefix",
        value_name = "PREFIX",
        help = "Prefix for side files with --json-stream-data=file. With --json-output, \
                defaults to the JSON output path; with JSON on stdout, an explicit \
                non-empty prefix is required. An empty prefix is treated as absent."
    )]
    json_stream_prefix: Option<OsString>,

    // qpdf-style top-level write flags. When `--linearize` is set together
    // with INPUT and OUTPUT, behave as if `flpdf rewrite --linearize ...`
    // had been invoked. This exists so the qpdf qtest acceptance harness
    // (PATH-shimmed `qpdf` → `flpdf`) can issue qpdf-shaped commands
    // without an arg-translating wrapper.
    /// Produce a linearized ("fast web view") output PDF (top-level alias
    /// of `flpdf rewrite --linearize`).
    #[arg(long)]
    linearize: bool,

    /// Set a minimum PDF version for the output header. An optional third
    /// component is qpdf's Adobe extension level, e.g. `1.7.3`.
    #[arg(long = "min-version")]
    min_version: Option<String>,

    /// Force the output PDF version and optional Adobe extension level,
    /// ignoring the input version (qpdf `--force-version`).
    #[arg(long = "force-version")]
    force_version: Option<String>,

    /// Use a fixed value for the trailer /ID's changing identifier
    /// (top-level alias of `flpdf rewrite --static-id`). Testing only;
    /// never for production output. This qpdf-shaped alias mirrors qpdf,
    /// which is silent for `--static-id`, so it emits no warning; the
    /// test-only diagnostic lives on the native `rewrite --static-id`
    /// surface instead.
    #[arg(long = "static-id")]
    static_id: bool,
    /// Generate a deterministic trailer /ID[1] from an MD5 of the rewritten
    /// output body instead of a random value (top-level alias of `flpdf rewrite
    /// --deterministic-id`; qpdf `--deterministic-id` equivalent). The permanent
    /// identifier /ID[0] is preserved from the input. Implies a full rewrite.
    /// If both this flag and `--static-id` are supplied, qpdf gives
    /// `--static-id` precedence for the emitted ID. The deterministic setting
    /// still makes the combination incompatible with encrypted output.
    #[arg(long = "deterministic-id")]
    deterministic_id: bool,
    /// Force every AES CBC IV to all-zero bytes instead of a random value
    /// (top-level alias of `flpdf rewrite --static-aes-iv`).
    /// **Testing only; produces insecure deterministic IVs, NOT for
    /// production.** Mirrors `qpdf --static-aes-iv`.
    #[arg(long = "static-aes-iv", hide = true)]
    static_aes_iv: bool,
    /// Remove digital-signature restrictions while preserving authenticated
    /// source encryption (top-level alias of `flpdf rewrite
    /// --remove-restrictions`; qpdf `--remove-restrictions` equivalent).
    /// Combine with `--decrypt` to strip encryption too. Does NOT bypass
    /// authentication.
    // This is a rewrite-path modifier. main()'s dispatch chain runs the
    // inspection modes (--check / --show-object / --show-*) before the
    // rewrite branch, so without these conflicts `flpdf --check
    // --remove-restrictions in out` would silently ignore the flag (and the
    // OUTPUT positional). Listing the inspection modes as clap conflicts
    // surfaces the mistake as a usage error instead. (--json already lists
    // remove_restrictions in its own conflicts_with_all; rewrite/linearize/
    // static_id/output are compatible rewrite modifiers and intentionally
    // excluded here.)
    #[arg(long = "remove-restrictions",
          conflicts_with_all = [
              "check", "show_object",
              "show_npages", "show_pages", "show_xref", "show_linearization",
              "show_encryption",
          ])]
    remove_restrictions: bool,
    /// Strip the `/Encrypt` dictionary from the output (top-level alias of
    /// `flpdf rewrite --decrypt`; qpdf `--decrypt` equivalent). On
    /// encrypted input requires `--password` to authenticate; on plaintext
    /// input it is a no-op pass-through. Silent in both cases (matching
    /// qpdf). Both flags are silent when the operation succeeds.
    ///
    /// Relationship with `--remove-restrictions`: this flag removes source
    /// encryption, while `--remove-restrictions` preserves it and only removes
    /// digital-signature restrictions. Neither flag invents a success diagnostic.
    // Same conflict semantics as --remove-restrictions: this is a
    // rewrite-path modifier and must be rejected against the inspection
    // subcommands so `flpdf --check --decrypt in out` is a usage error
    // rather than silently ignoring the flag (and OUTPUT).
    #[arg(long = "decrypt",
          conflicts_with_all = [
              "check", "show_object",
              "show_npages", "show_pages", "show_xref", "show_linearization",
              "show_encryption",
          ])]
    decrypt: bool,
    /// `qpdf --compress-streams=y|n` compatibility flag.  Accepted but
    /// currently a no-op: flpdf does not re-encode stream contents on
    /// rewrite.  Provided so qtest commands parse cleanly.
    #[arg(long = "compress-streams")]
    compress_streams: Option<String>,
    /// Re-encode streams that are already a lone `/FlateDecode` (qpdf
    /// `--recompress-flate`).
    #[arg(long = "recompress-flate")]
    recompress_flate: bool,
    /// Set the zlib compression level used when emitting Flate streams
    /// (qpdf `--compression-level=level`).
    #[arg(long = "compression-level", value_name = "LEVEL")]
    compression_level: Option<String>,
    /// Control which qpdf stream filters are decoded during rewrite.
    /// Values are ordered from least to most decoding: none, generalized,
    /// specialized, and all.
    #[arg(long = "decode-level", value_enum)]
    decode_level: Option<CliDecodeLevel>,
    /// Control what qpdf does regarding object streams. `preserve` preserves
    /// original object streams (the default), `disable` creates output with no
    /// object streams, and `generate` creates object streams and compresses
    /// objects when possible.
    #[arg(
        long = "object-streams",
        value_enum,
        default_value_t = CliObjectStreamMode::Preserve
    )]
    object_streams: CliObjectStreamMode,
    /// Control how streams are compressed in the output. `compress` is the
    /// same as `--compress-streams=y --decode-level=generalized`, `preserve`
    /// is the same as `--compress-streams=n --decode-level=none`, and
    /// `uncompress` is the same as `--compress-streams=n --decode-level=generalized`.
    #[arg(long = "stream-data", value_enum)]
    stream_data: Option<CliStreamDataMode>,
    /// Insert a newline before each `endstream` keyword (qpdf
    /// `--newline-before-endstream`). The `y` and `n` spellings both select
    /// qpdf's enabled boolean setting; `never` retains the default framing.
    #[arg(long = "newline-before-endstream", value_enum, num_args = 0..=1,
          require_equals = true, default_missing_value = "y",
          default_value_t = CliNewlineBeforeEndstream::Never)]
    newline_before_endstream: CliNewlineBeforeEndstream,
    /// `qpdf --linearize-pass1=PATH` compatibility flag. Writes the
    /// linearization writer's distinct pass-1 intermediate file.
    #[arg(long = "linearize-pass1")]
    linearize_pass1: Option<PathBuf>,
    /// Omit the `%% Original object ID: N M` comments that QDF output would
    /// otherwise carry (top-level alias of `flpdf rewrite
    /// --no-original-object-ids`; qpdf `--no-original-object-ids`
    /// equivalent). A compatible rewrite/QDF modifier — like `--static-id` it
    /// does not conflict with the rewrite-mode positionals.
    #[arg(long = "no-original-object-ids")]
    no_original_object_ids: bool,
    /// Create a PDF in QDF form: uncompressed, normalized,
    /// human-readable/editable; pair with the qdf-fix subcommand after manual
    /// edits (qpdf --qdf equivalent). Top-level alias of `flpdf rewrite
    /// --qdf`. Like `--static-id`/`--no-original-object-ids` it is a
    /// compatible rewrite/QDF modifier and does not conflict with the
    /// rewrite-mode positionals.
    #[arg(long = "qdf")]
    qdf: bool,

    /// Preserve input objects that are not reachable from trailer roots
    /// (qpdf `--preserve-unreferenced`). The default is disabled.
    #[arg(
        long = "preserve-unreferenced",
        help = "Preserve unreferenced input objects in writer output"
    )]
    preserve_unreferenced: bool,

    /// Remove unreferenced page-resource entries using qpdf's job policy.
    ///
    /// `auto` (the default) runs qpdf's shared-resource heuristic, `yes`
    /// always runs the page-level pruning pass, and `no` leaves `/Resources`
    /// entries untouched. The policy is consumed by page-copy and split jobs;
    /// plain rewrites accept the option but do not prune resource entries,
    /// matching qpdf 11.9.0.
    #[arg(
        long = "remove-unreferenced-resources",
        value_enum,
        default_value_t = CliRemoveUnreferencedResources::Auto,
        help = "Remove unreferenced page resources (qpdf default: auto)"
    )]
    remove_unreferenced_resources: CliRemoveUnreferencedResources,

    /// Normalize PDF page content streams (qpdf --normalize-content=y|n).
    ///
    /// The absence of this option is distinct from an explicit `n`: qpdf
    /// enables normalization by default in QDF mode, but an explicit `n`
    /// overrides that QDF default.
    #[arg(
        long = "normalize-content",
        value_enum,
        help = "Normalize page content streams (qpdf default: n; --qdf default: y)"
    )]
    normalize_content: Option<CliYesNo>,

    /// Coalesce multiple /Contents streams into a single stream per page
    /// (top-level alias of `flpdf rewrite --coalesce-contents`; qpdf
    /// `--coalesce-contents` equivalent). Requires a full rewrite of the
    /// document. Rejected against inspection, attachment, `--linearize`,
    /// and page-operation modes: the linearize branch of `run_rewrite`
    /// and the page-op dispatch never read `args.coalesce_contents`, so
    /// without these conflicts the flag would be silently dropped and the
    /// user's requested coalescing would not appear in the output.
    #[arg(long = "coalesce-contents",
          conflicts_with_all = [
              "check", "show_object",
              "show_npages", "show_pages", "show_xref", "show_linearization",
              "show_encryption",
              "list_attachments", "show_attachment", "remove_attachment",
              "add_attachment", "copy_attachments_from",
              "linearize", "pages", "rotate", "split_pages", "empty",
          ])]
    coalesce_contents: bool,

    /// Flatten annotations into page content (top-level alias of
    /// `rewrite --flatten-annotations`; qpdf `--flatten-annotations`
    /// equivalent). Values are `all`, `screen`, or `print`.
    // `json_output` has no dedicated dispatch check of its own (unlike
    // `json`, which lists `flatten_annotations` on its own conflicts_with_all
    // for the same reason): without it, `--flatten-annotations=all
    // --json-output=2 in out` exits 0 and silently writes a JSON dump of the
    // unmodified input while dropping the requested transformation, since
    // main's dispatch chain routes to run_json before either rewrite path
    // that consumes flatten_annotations. Confirmed live.
    #[arg(
        long = "flatten-annotations",
        value_enum,
        value_name = "MODE",
        conflicts_with_all = [
            "check", "show_object",
            "show_npages", "show_pages", "show_xref", "show_linearization",
            "show_encryption",
            "list_attachments", "show_attachment", "remove_attachment",
            "add_attachment", "copy_attachments_from",
            "pages", "rotate", "split_pages", "empty",
            "json_output",
        ],
        help = "Flatten annotations into page content; MODE is all, screen, or print"
    )]
    flatten_annotations: Option<CliFlattenMode>,

    /// Generate appearance streams for form fields that need them (qpdf
    /// `--generate-appearances`). Rejected against inspection, attachment,
    /// and page-operation modes: the page-op dispatch never reads
    /// `args.generate_appearances`, so without these conflicts the flag
    /// would be silently dropped and the requested appearance generation
    /// would not appear in the output. Combining with `--linearize` is
    /// supported (threaded through the linearize branch of `run_rewrite`),
    /// so it is intentionally absent from this list.
    #[arg(long = "generate-appearances",
          conflicts_with_all = [
              "check", "show_object",
              "show_npages", "show_pages", "show_xref", "show_linearization",
              "show_encryption",
              "list_attachments", "show_attachment", "remove_attachment",
              "add_attachment", "copy_attachments_from",
              "pages", "rotate", "split_pages", "empty", "json_output",
          ])]
    generate_appearances: bool,

    /// Recompress eligible non-JPEG images as DCT/JPEG (qpdf
    /// `--optimize-images`). Rejected against inspection and attachment
    /// modes: those dispatch branches call their dedicated writers without
    /// ever consuming the computed image options, so without these
    /// conflicts an accepted `--optimize-images` would be silently dropped.
    /// `--pages`/`--rotate`/`--split-pages`/`--empty`/`--json`/
    /// `--json-output` are intentionally absent: all of those routes are
    /// already threaded through (see `top_level_image_options` at each call
    /// site).
    #[arg(long = "optimize-images",
          conflicts_with_all = [
              "check", "show_object",
              "show_npages", "show_pages", "show_xref", "show_linearization",
              "show_encryption",
              "list_attachments", "show_attachment", "remove_attachment",
              "add_attachment", "copy_attachments_from",
          ])]
    optimize_images: bool,
    /// Exclude inline images from the optimization pass.
    #[arg(long = "keep-inline-images")]
    keep_inline_images: bool,
    /// Minimum image width for `--optimize-images` (qpdf default: 128).
    #[arg(long = "oi-min-width", value_name = "WIDTH")]
    oi_min_width: Option<String>,
    /// Minimum image height for `--optimize-images` (qpdf default: 128).
    #[arg(long = "oi-min-height", value_name = "HEIGHT")]
    oi_min_height: Option<String>,
    /// Minimum image area for `--optimize-images` (qpdf default: 16384).
    #[arg(long = "oi-min-area", value_name = "AREA")]
    oi_min_area: Option<String>,
    /// Minimum inline-image payload to externalize (qpdf default: 1024).
    #[arg(long = "ii-min-bytes", value_name = "BYTES")]
    ii_min_bytes: Option<String>,

    // ── Page-operation flags ──────────────────────────────────────────────
    // These mirror qpdf's page-selection / page-transformation surface.
    // Observed against /usr/bin/qpdf 11.9.0:
    //   qpdf --help=--pages / --rotate / --split-pages / --collate
    //   qpdf in.pdf --pages . a.pdf b.pdf 1-z:even -- out.pdf
    #[command(flatten)]
    page_ops: PageOpArgs,

    // ── Overlay / underlay flags, top-level alias ──────────────────────────
    // Mirror qpdf's top-level `qpdf in --overlay f -- out` form. Like the
    // `rewrite` subcommand fields, the per-group boundaries are extracted from
    // raw argv by `preprocess_qpdf_args` before clap parses; these fields
    // exist only for `--help` documentation and to accept a leaked token.
    /// Overlay pages from another file on top of the output (qpdf `--overlay`;
    /// top-level alias of `rewrite --overlay`). Repeatable; terminate each
    /// group with `--`.
    #[arg(
        long = "overlay",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "[--file=]FILE [sub-flags]",
        help = "Overlay pages from FILE on top of the output (qpdf --overlay); \
                repeatable, terminate each group with --"
    )]
    overlay: Vec<OsString>,

    /// Underlay pages from another file beneath the output (qpdf `--underlay`;
    /// top-level alias of `rewrite --underlay`). Repeatable; terminate each
    /// group with `--`.
    #[arg(
        long = "underlay",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "[--file=]FILE [sub-flags]",
        help = "Underlay pages from FILE beneath the output (qpdf --underlay); \
                repeatable, terminate each group with --"
    )]
    underlay: Vec<OsString>,

    // ── Attachment flags ──────────────────────────────────────────────────
    // Five qpdf-compatible attachment operations.  Each is a top-level flag
    // dispatched before the default rewrite branch.
    //
    // --add-attachment and --copy-attachments-from use value_terminator="--"
    // and allow_hyphen_values=true so that their sub-flags (--key, --filename,
    // --prefix, --password, …) are captured verbatim in the token Vec rather
    // than being parsed as global clap flags.
    /// Add an attachment to the input PDF (qpdf --add-attachment compatible).
    ///
    /// Syntax: `--add-attachment FILE [--key=K] [--filename=F] [--mimetype=M]
    ///           [--description=D] [--creationdate=D] [--moddate=D]
    ///           [--replace] --`
    ///
    /// The `--` terminator ends the sub-flag segment. The token after `--` is
    /// the OUTPUT positional.
    #[arg(
        long = "add-attachment",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "FILE [sub-flags]",
        help = "Add a file attachment (qpdf --add-attachment compatible); \
                terminate segment with --"
    )]
    add_attachment: Vec<OsString>,

    /// Remove an attachment by key (qpdf --remove-attachment compatible).
    ///
    /// KEY is the name-tree key used when the attachment was added. The flag
    /// may be repeated; keys are removed in argv order.
    #[arg(
        long = "remove-attachment",
        value_name = "KEY",
        help = "Remove the embedded file with the given key (qpdf --remove-attachment)"
    )]
    remove_attachment: Vec<OsString>,

    /// List all embedded-file attachments (qpdf --list-attachments compatible).
    #[arg(
        long = "list-attachments",
        conflicts_with = "output",
        help = "List all embedded-file attachments (qpdf --list-attachments)"
    )]
    list_attachments: bool,

    /// Print verbose progress and diagnostic messages (mirrors qpdf --verbose).
    #[arg(
        long = "verbose",
        help = "Print verbose progress and diagnostic messages \
                (mirrors qpdf --verbose)"
    )]
    verbose: bool,

    /// Report approximate write progress (qpdf --progress).
    #[arg(long = "progress")]
    progress: bool,

    /// Extract an attachment by key (qpdf --show-attachment compatible).
    ///
    /// KEY is the name-tree key used when the attachment was added. The raw
    /// bytes are written to stdout.
    #[arg(
        long = "show-attachment",
        conflicts_with = "output",
        value_name = "KEY",
        help = "Extract the embedded file with the given key to stdout \
                (qpdf --show-attachment)"
    )]
    show_attachment: Option<OsString>,

    /// Copy attachments from another PDF (qpdf --copy-attachments-from compatible).
    ///
    /// Syntax: `--copy-attachments-from FILE [--password=P] [--prefix=X] --`
    ///
    /// The `--` terminator ends the sub-flag segment.
    #[arg(
        long = "copy-attachments-from",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "FILE [sub-flags]",
        help = "Copy attachments from another PDF (qpdf --copy-attachments-from compatible); \
                terminate segment with --"
    )]
    copy_attachments_from: Vec<OsString>,

    /// Encrypt the output (qpdf `--encrypt` compatible).
    ///
    /// Syntax: `--encrypt USER-PW OWNER-PW KEY-LEN [sub-flags] --`
    ///
    /// USER-PW / OWNER-PW are the two password strings; KEY-LEN selects the
    /// qpdf Standard security handler family (`40`, `128`, or `256`). The
    /// AES, R=5, weak-crypto, and permission sub-flags follow qpdf's
    /// compatibility rules and are validated before writing.
    ///
    /// The `--` terminator ends the sub-flag segment. The tokens after
    /// `--` are the INPUT / OUTPUT positionals.
    #[arg(
        long = "encrypt",
        num_args = 0..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "USER-PW OWNER-PW KEY-LEN [sub-flags]",
        // Reject combinations that don't make sense on the rewrite path.
        // --remove-restrictions / --decrypt overlap with --encrypt and are
        // rejected because they imply contradictory output forms; --check /
        // --show-object / --show-* are inspection paths that don't produce an
        // output file at all. --linearize is NOT rejected: qpdf itself
        // supports `--linearize --encrypt ...` (verified: `qpdf --linearize
        // --encrypt "" "" 128 --use-aes=y --` produces a valid,
        // `qpdf --check`-clean linearized+encrypted file), and
        // `write_linearized` threads `options.encrypt` through correctly.
        conflicts_with_all = [
            "check", "show_object",
            "show_npages", "show_pages", "show_xref", "show_linearization",
            "show_encryption",
            "remove_restrictions", "decrypt",
            "copy_encryption",
        ],
        help = "Encrypt output (qpdf --encrypt compatible): \
                USER-PW OWNER-PW KEY-LEN [sub-flags] --"
    )]
    encrypt: Option<Vec<OsString>>,
    #[arg(skip)]
    raw_encrypt: Option<Vec<Vec<u8>>>,

    /// Copy the /Encrypt dictionary from a donor PDF and use its passwords for
    /// output encryption (qpdf --copy-encryption equivalent).
    ///
    /// Supply the donor's password via `--encryption-file-password` (empty
    /// string if the donor has no user password).  Only V=4 AES-128 donors are
    /// supported; other schemes are rejected
    /// with a "not yet supported" diagnostic.
    ///
    /// Mutually exclusive with `--encrypt`. `--linearize` may be combined with
    /// this option; qpdf supports copying encryption into a linearized output.
    #[arg(
        long = "copy-encryption",
        value_name = "FILE",
        conflicts_with_all = [
            "encrypt",
            "check", "show_object",
            "show_npages", "show_pages", "show_xref", "show_linearization",
            "show_encryption",
            "remove_restrictions", "decrypt",
        ],
        help = "Copy /Encrypt from donor PDF (qpdf --copy-encryption); \
                pair with --encryption-file-password"
    )]
    copy_encryption: Option<PathBuf>,

    /// Password to open the donor PDF specified by `--copy-encryption`.
    ///
    /// Omit (or pass an empty string) if the donor has no user password.
    /// This is the *donor's* password, not the output file's password
    /// (the output inherits the donor's passwords exactly).
    #[arg(
        long = "encryption-file-password",
        value_name = "PW",
        requires = "copy_encryption",
        help = "User password to open the donor PDF for --copy-encryption"
    )]
    encryption_file_password: Option<OsString>,
    #[arg(skip)]
    raw_encryption_file_password: Option<Vec<u8>>,

    #[arg(skip)]
    raw_copy_attachments_from: Option<Vec<Vec<Vec<u8>>>>,

    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

/// qpdf-compatible page-operation flags, shared by the top-level CLI and the
/// `rewrite` subcommand.
///
/// `--pages SPEC... --` captures the raw multi-input page-selection segment
/// verbatim (clap `value_terminator = "--"`, `allow_hyphen_values = true`) so
/// the embedded `--password=` / `--file=` / `--range=` tokens reach the
/// hand-written segment parser rather than being eaten as global flags. This
/// was verified empirically: with this attribute set, the top-level
/// `--password` field stays `None` while the segment vec captures
/// `["--password=x", …]`.
#[derive(Debug, Clone, Default, ClapArgs)]
struct PageOpArgs {
    /// Manage whether qpdf keeps secondary `--pages` input files open
    /// (`--keep-files-open=y|n`). When omitted, qpdf selects the value from
    /// the distinct page-spec source count and [`Self::keep_files_open_threshold`].
    #[arg(long = "keep-files-open", value_enum)]
    keep_files_open: Option<CliYesNo>,

    /// Distinct page-spec source count at which qpdf automatically switches
    /// `--keep-files-open` off (default 200).
    #[arg(long = "keep-files-open-threshold", value_name = "COUNT")]
    keep_files_open_threshold: Option<String>,

    /// Select pages from one or more input files (qpdf `--pages`).
    ///
    /// Syntax (qpdf 11.9.0 `--help=page-selection`):
    ///   `--pages [--file=]file [--password=pw] [page-range] [...] -- out.pdf`
    /// `.` is a shorthand for the primary input file. An omitted page-range
    /// selects all pages of that file. The `--` terminator ends the segment;
    /// the token after it is the OUTPUT positional.
    ///
    /// Distinct source files are opened and copied through the qpdf-shaped
    /// `QPDFJob::handle_page_specs` boundary; JSON-created primaries retain
    /// their separate single-document restriction until their source heap is
    /// implemented.
    #[arg(
        long = "pages",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "SPEC",
        help = "Select pages from input files: --pages [--file=]f [--password=p] \
                [range] [...] -- (qpdf-compatible). '.' = primary input; omitted \
                range = all pages."
    )]
    pages: Vec<OsString>,
    #[arg(skip)]
    raw_pages: Option<Vec<Vec<u8>>>,

    /// Rotate pages by a multiple of 90 degrees (qpdf `--rotate`).
    ///
    /// Form: `[+|-]angle[:page-range]` where angle ∈ {0,90,180,270}.
    /// Repeatable; specs are applied in argument order. In `--pages` mode the
    /// page-range refers to OUTPUT page numbers (qpdf 11.9.0-observed:
    /// `qpdf in --pages . 2-3 -- --rotate=+90:1 out` rotates the first
    /// extracted page).
    #[arg(
        long = "rotate",
        action = clap::ArgAction::Append,
        value_name = "[+|-]angle[:range]",
        help = "Rotate pages by 0/90/180/270 degrees (qpdf --rotate); repeatable"
    )]
    rotate: Vec<String>,

    /// Write one output file per N-page group instead of a single file
    /// (qpdf `--split-pages[=n]`, default n=1).
    ///
    /// File names follow qpdf's `doSplitPages` naming: a `-first-last`
    /// suffix is inserted before the `.pdf` extension. Compatible with
    /// `--pages`.
    #[arg(
        long = "split-pages",
        num_args = 0..=1,
        default_missing_value = "1",
        require_equals = true,
        value_name = "N",
        help = "Split output into N-page files (qpdf --split-pages[=n], default 1)"
    )]
    split_pages: Option<String>,

    /// Collate (interleave) pages selected with `--pages` instead of
    /// concatenating (qpdf `--collate[=n[,m,...]]`, default n=1).
    ///
    /// Only meaningful together with `--pages`; qpdf 11.9.0 accepts it as a
    /// no-op otherwise (exit 0), and flpdf matches that.
    #[arg(
        long = "collate",
        num_args = 0..=1,
        action = clap::ArgAction::Append,
        default_missing_value = "1",
        require_equals = true,
        value_name = "N[,M,...]",
        help = "Collate --pages selections in groups of N[,M,...] (qpdf --collate[=n[,m,...]], default 1)"
    )]
    collate: Vec<String>,

    /// `qpdf --empty` — start from an empty document for a `--pages` job.
    /// A bare empty rewrite remains unsupported at this layer.
    #[arg(
        long = "empty",
        help = "(qpdf --empty) start from an empty document for --pages"
    )]
    empty: bool,
}

// The RewriteCommand variant is large by design (it holds many optional flags).
// Boxing it would require matching `Commands::Rewrite(cmd)` with a deref in
// every match arm — a larger refactor than warranted for this lint.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Validate PDF structure and report diagnostics")]
    Check(CheckCommand),
    #[command(
        name = "check-linearization",
        about = "Validate linearization structure (param dict, hint stream, offsets)"
    )]
    CheckLinearization(CheckLinearizationCommand),
    #[command(name = "dump-object", about = "Dump one indirect object as PDF syntax")]
    DumpObject(DumpObjectCommand),
    #[command(about = "Show page structure summary or detail")]
    Pages(PagesCommand),
    #[command(about = "Create a PDF in QDF form (alias of `rewrite --qdf`)")]
    Qdf(QdfCommand),
    #[command(
        name = "qdf-fix",
        about = "Repair stream /Length, xref offsets, /Size and startxref in a hand-edited QDF file (qpdf fix-qdf equivalent)"
    )]
    QdfFix(QdfFixCommand),
    #[command(
        name = "zlib-flate",
        about = "Compress or uncompress a raw zlib stream on stdin/stdout"
    )]
    ZlibFlate(ZlibFlateCommand),
    #[command(about = "Rewrite the input PDF to a normalized output")]
    Rewrite(RewriteCommand),
    #[command(
        name = "show-stream",
        about = "Show a stream object's decoded (or raw) data"
    )]
    ShowStream(ShowStreamCommand),
    #[command(
        name = "show-encryption",
        about = "Show encryption parameters (qpdf --show-encryption compatible)",
        long_about = "\
Print a parseable, greppable encryption report for FILE.

The qpdf `--show-encryption` lines are emitted verbatim (`R = `, `P = `,
the `extract/print/modify ...: allowed|not allowed` block, and the
`stream/string/file encryption method:` lines for V>=4) so scripts that
including `User password = ...` and V<5 owner-password recovery. If the
password is wrong, qpdf's `Incorrect password supplied` line is emitted
before the parsed encryption report. If FILE is not encrypted, prints qpdf's
`File is not encrypted` and exits 0."
    )]
    ShowEncryption(EncryptionInspectCommand),
    #[command(
        name = "is-encrypted",
        about = "Exit 0 if FILE is encrypted, 2 if not (qpdf --is-encrypted)",
        long_about = "\
Silently exit 0 if FILE is encrypted, 2 if it is not encrypted. Works for
password-protected files even without the password. Mirrors qpdf
`--is-encrypted` (qpdf_exit_is_not_encrypted=2)."
    )]
    IsEncrypted(IsEncryptedCommand),
    #[command(
        name = "requires-password",
        about = "Exit 0/2/3 reporting whether a password is required (qpdf --requires-password)",
        long_about = "\
Silently exit reporting FILE's password requirement (qpdf
--requires-password):
  0 = encrypted and a password other than the one supplied is required
  2 = not encrypted (qpdf_exit_is_not_encrypted)
  3 = encrypted and the supplied/empty password opens it
      (qpdf_exit_correct_password)"
    )]
    RequiresPassword(EncryptionInspectCommand),
    #[command(
        name = "show-encryption-key",
        about = "Print the file encryption key as lowercase hex (qpdf --show-encryption-key)",
        long_about = "\
Authenticate FILE with the supplied/empty password, then print the
derived file encryption key as lowercase hex. Mirrors qpdf
`--show-encryption-key`. Errors (exit 2) if FILE is not encrypted or the
password is incorrect."
    )]
    ShowEncryptionKey(EncryptionInspectCommand),
}

/// Args for inspection subcommands that authenticate the document
/// (`show-encryption`, `requires-password`, `show-encryption-key`).
#[derive(Debug, ClapArgs)]
struct EncryptionInspectCommand {
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

/// Args for `is-encrypted`. No password: qpdf detects encryption without
/// authenticating, so a password would be meaningless here.
#[derive(Debug, ClapArgs)]
struct IsEncryptedCommand {
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    recovery: RecoveryArgs,
}

#[derive(Debug, ClapArgs)]
struct CheckCommand {
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct CheckLinearizationCommand {
    /// Input PDF file to validate.
    input: PathBuf,
}

#[derive(Debug, ClapArgs)]
struct DumpObjectCommand {
    object_ref: String,
    input: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct ShowStreamCommand {
    /// Object reference, e.g. "7 0" or "7 0 R".
    object_ref: String,
    input: PathBuf,
    /// Emit unfiltered stored bytes instead of decoding (qpdf --raw-stream-data).
    #[arg(long = "raw-stream-data")]
    raw_stream_data: bool,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct PagesCommand {
    input: PathBuf,
    /// Print only the page count (qpdf --show-npages).
    #[arg(long = "show-npages")]
    show_npages: bool,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct QdfCommand {
    input: PathBuf,
    output: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    /// Preserve input objects that are not reachable from trailer roots
    /// (qpdf `--preserve-unreferenced`).
    #[arg(long = "preserve-unreferenced")]
    preserve_unreferenced: bool,
}

/// Args for `qdf-fix` (qpdf `fix-qdf` equivalent). No password / no Pdf
/// open: fix_qdf operates byte-for-byte on a (possibly hand-edited) QDF file
/// and must not reparse or reformat it.
#[derive(Debug, ClapArgs)]
struct QdfFixCommand {
    input: PathBuf,
    output: PathBuf,
}

/// Args for the qpdf `zlib-flate` utility surface.
#[derive(Debug, ClapArgs)]
struct ZlibFlateCommand {
    /// `-uncompress`, `-compress`, or `-compress=n`.
    #[arg(value_name = "MODE", allow_hyphen_values = true, num_args = 0..)]
    modes: Vec<OsString>,
}

#[derive(Debug, ClapArgs)]
struct RewriteCommand {
    input: PathBuf,
    output: PathBuf,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    /// Produce a linearized ("fast web view") output PDF.
    #[arg(long)]
    linearize: bool,
    /// Use a fixed value for the trailer /ID's changing identifier (qpdf
    /// --static-id equivalent). Testing only; not for production output.
    /// Emits a stderr warning when used (suppress with the
    /// FLPDF_STATIC_ID_QUIET env var).
    #[arg(long = "static-id")]
    static_id: bool,
    /// Generate a deterministic trailer /ID[1] from an MD5 of the rewritten
    /// output body instead of a random value (qpdf `--deterministic-id`
    /// equivalent).
    ///
    /// The changing identifier /ID[1] is an MD5 over the rewritten output body;
    /// the permanent identifier /ID[0] is preserved from the input (matching
    /// `--static-id` and ISO 32000-1 §14.4). Uses the canonical writer. When
    /// combined with `--static-id`, qpdf gives `--static-id` precedence for
    /// the emitted ID. Any use of this flag remains incompatible with
    /// encrypted output (the /ID feeds the encryption key). Unlike
    /// `--static-id` it is a production-safe flag and emits no testing-only
    /// warning.
    #[arg(long = "deterministic-id")]
    deterministic_id: bool,
    /// Force every AES CBC IV to all-zero bytes instead of a random value.
    /// **Testing only; produces insecure deterministic IVs, NOT for
    /// production.** Mirrors `qpdf --static-aes-iv`.
    #[arg(long = "static-aes-iv", hide = true)]
    static_aes_iv: bool,
    /// Remove digital-signature restrictions while preserving authenticated
    /// source encryption (qpdf `--remove-restrictions` equivalent). Combine
    /// with `--decrypt` to strip encryption too.
    ///
    /// A normal rewrite preserves authenticated source encryption. This flag
    /// removes qpdf's digital-signature restrictions without inventing a
    /// success diagnostic. It does NOT bypass authentication.
    ///
    /// See `--decrypt` for the silent qpdf-compatible encryption-removal flag.
    #[arg(long = "remove-restrictions")]
    remove_restrictions: bool,
    /// Strip the `/Encrypt` dictionary from the output (qpdf `--decrypt`
    /// equivalent). On encrypted input requires `--password` to
    /// authenticate; on plaintext input it is a no-op pass-through. Silent
    /// in both cases, matching qpdf `--decrypt`.
    ///
    /// Relationship with `--remove-restrictions`: this flag removes source
    /// encryption, while `--remove-restrictions` preserves it and only removes
    /// digital-signature restrictions. Neither flag invents a success diagnostic.
    #[arg(long = "decrypt")]
    decrypt: bool,
    /// Encrypt the output (qpdf `--encrypt` compatible). See the top-level
    /// `--encrypt` documentation for the full syntax and supported modes.
    /// `--linearize` is not rejected: qpdf itself supports
    /// `--linearize --encrypt ...`, and `write_linearized` threads
    /// `options.encrypt` through correctly.
    #[arg(
        long = "encrypt",
        num_args = 0..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "USER-PW OWNER-PW KEY-LEN [sub-flags]",
        conflicts_with_all = [
            "remove_restrictions", "decrypt",
            "copy_encryption",
        ]
    )]
    encrypt: Option<Vec<OsString>>,
    #[arg(skip)]
    raw_encrypt: Option<Vec<Vec<u8>>>,
    /// Copy the /Encrypt dictionary from a donor PDF and use its passwords for
    /// output encryption (qpdf --copy-encryption equivalent).
    ///
    /// Supply the donor's password via `--encryption-file-password` (empty
    /// string if the donor has no user password).  Only V=4 AES-128 donors are
    /// supported; other schemes are rejected
    /// with a "not yet supported" diagnostic.
    ///
    /// Mutually exclusive with `--encrypt`. `--linearize` may be combined with
    /// this option; qpdf supports copying encryption into a linearized output.
    #[arg(
        long = "copy-encryption",
        value_name = "FILE",
        conflicts_with_all = [
            "encrypt",
            "remove_restrictions", "decrypt",
        ],
        help = "Copy /Encrypt from donor PDF (qpdf --copy-encryption); \
                pair with --encryption-file-password"
    )]
    copy_encryption: Option<PathBuf>,
    /// Password to open the donor PDF specified by `--copy-encryption`.
    ///
    /// Omit (or pass an empty string) if the donor has no user password.
    #[arg(
        long = "encryption-file-password",
        value_name = "PW",
        requires = "copy_encryption",
        help = "User password to open the donor PDF for --copy-encryption"
    )]
    encryption_file_password: Option<OsString>,
    #[arg(skip)]
    raw_encryption_file_password: Option<Vec<u8>>,

    #[arg(skip)]
    raw_copy_attachments_from: Option<Vec<Vec<Vec<u8>>>>,
    /// Set a minimum PDF version for the output header.
    ///
    /// The effective version is `max(source_version, min_version)`.
    /// Mirrors `qpdf --min-version`.
    #[arg(long = "min-version")]
    min_version: Option<String>,
    /// Force the output PDF version header to exactly this value.
    ///
    /// Overrides source version and the linearize 1.2 floor.
    /// Mirrors `qpdf --force-version`.
    #[arg(long = "force-version")]
    force_version: Option<String>,
    /// Omit the `%% Original object ID: N M` comments that QDF output would
    /// otherwise carry. Mirrors `qpdf --no-original-object-ids`.
    ///
    /// Observed (qpdf 11.9.0): this flag changes only QDF output; qpdf JSON
    /// v1/v2 is byte-identical with or without it, so flpdf does not wire it
    /// into any JSON path. flpdf's QDF writer does not yet emit these
    /// comments; the flag is
    /// accepted and plumbed for forward-compatibility, so today it is a
    /// byte-level no-op.
    #[arg(long = "no-original-object-ids")]
    no_original_object_ids: bool,
    /// Create a PDF in QDF form: uncompressed, normalized,
    /// human-readable/editable; pair with the qdf-fix subcommand after manual
    /// edits (qpdf --qdf equivalent).
    ///
    /// Uses the canonical writer and preserves the explicit --object-streams mode. Cannot be
    /// combined with --linearize (QDF is inherently non-linearized).
    #[arg(long = "qdf")]
    qdf: bool,
    /// Preserve input objects that are not reachable from trailer roots
    /// (qpdf `--preserve-unreferenced`). The default is disabled.
    #[arg(long = "preserve-unreferenced")]
    preserve_unreferenced: bool,
    /// Object stream behaviour for the output. Mirrors qpdf
    /// `--object-streams=preserve|disable|generate`. Default: `preserve`.
    ///
    /// - `preserve` (default): reuse the source document's existing ObjStm
    ///   grouping.
    /// - `disable`: emit every eligible object as a plain indirect object.
    /// - `generate`: pack eligible objects into freshly generated ObjStm
    ///   containers.
    ///
    /// Applies to the canonical qpdf writer output.
    #[arg(long = "object-streams", value_enum, default_value_t = CliObjectStreamMode::Preserve)]
    object_streams: CliObjectStreamMode,

    /// Apply FlateDecode compression to output streams (qpdf --compress-streams=y|n).
    ///
    /// `y` (default): decode each source stream and re-emit with a single /FlateDecode
    /// filter, matching qpdf's default behaviour.
    /// `n`: decode each source stream and emit raw bytes without any filter.
    ///
    /// Only affects the full-rewrite path.
    #[arg(
        long = "compress-streams",
        value_enum,
        help = "Compress output streams with FlateDecode (qpdf default: y)"
    )]
    compress_streams: Option<CliYesNo>,

    /// Control which qpdf stream filters are decoded during rewrite.
    #[arg(long = "decode-level", value_enum)]
    decode_level: Option<CliDecodeLevel>,

    /// Normalize PDF content streams (qpdf --normalize-content=y|n).
    ///
    /// `y`: re-tokenize each page content stream and emit a canonical whitespace-
    /// normalized form, matching qpdf's `--normalize-content=y`.
    /// `n` (default): leave content streams untouched (qpdf default).
    ///
    /// When enabled, each page's content stream is updated in-place before writing,
    /// which requires a full rewrite of the document.
    #[arg(
        long = "normalize-content",
        value_enum,
        help = "Normalize page content streams (qpdf default: n)"
    )]
    normalize_content: Option<CliYesNo>,

    /// Coalesce multiple /Contents streams into a single stream per page
    /// (qpdf --coalesce-contents).
    ///
    /// When a page's /Contents is an array of two or more stream references,
    /// merge them into a single stream. Default: off (qpdf default: off).
    ///
    /// Requires a full rewrite of the document when enabled.
    #[arg(
        long = "coalesce-contents",
        help = "Merge per-page /Contents arrays into a single stream (qpdf default: off)"
    )]
    coalesce_contents: bool,

    /// Remove unreferenced /Resources entries from each page
    /// (qpdf --remove-unreferenced-resources=auto|yes|no).
    ///
    /// - `auto` (default): prune only pages whose /Resources are not shared with
    ///   another page — safe heuristic, qpdf-compatible.
    /// - `yes`: prune on a per-page basis regardless of sharing (union of
    ///   all referencing pages' used names is kept to avoid breakage).
    /// - `no`: leave all /Resources entries untouched.
    ///
    /// Requires a full rewrite when set to `yes` or `auto`.
    #[arg(long = "remove-unreferenced-resources", value_enum,
          default_value_t = CliRemoveUnreferencedResources::Auto,
          help = "Remove unreferenced /Resources entries (qpdf default: auto)")]
    remove_unreferenced_resources: CliRemoveUnreferencedResources,

    /// Insert a newline before each `endstream` keyword
    /// (qpdf --newline-before-endstream=y|n|never).
    ///
    /// `never` (default): never insert a newline, so exactly `/Length` bytes sit
    /// between `stream` and `endstream`. Reproduces qpdf's default output and is
    /// required for byte-identical qpdf-equivalent rewrites.
    /// `y` and `n`: enable qpdf's boolean option and always write exactly one
    /// `\n` before `endstream`. qpdf 11.9.0 accepts both value spellings as
    /// the presence of the flag.
    /// Unrecognized attached values are also treated as flag presence, as in
    /// qpdf's bare-option parser.
    ///
    /// Only affects the full-rewrite path.
    #[arg(long = "newline-before-endstream", value_enum, num_args = 0..=1,
          require_equals = true, default_missing_value = "y",
          default_value_t = CliNewlineBeforeEndstream::Never,
          help = "Insert newline before endstream keyword (qpdf default: never)")]
    newline_before_endstream: CliNewlineBeforeEndstream,

    /// Stream data mode (qpdf --stream-data={preserve,uncompress,compress}).
    ///
    /// Higher-level policy that sets stream decode behavior before an explicitly
    /// supplied --compress-streams value, matching qpdf's writer setter order.
    /// - `preserve`: pass streams through verbatim — no decode or re-encode.
    /// - `uncompress`: decode streams and emit raw bytes (no /Filter).
    /// - `compress`: decode streams and re-encode with /FlateDecode.
    ///
    /// Default: not set (falls back to --compress-streams).
    /// When both are supplied explicitly, --compress-streams wins.
    /// Only affects the full-rewrite path.
    #[arg(long = "stream-data", value_enum)]
    stream_data: Option<CliStreamDataMode>,

    /// Re-encode streams that are already a lone /FlateDecode (default: preserve
    /// them verbatim, matching qpdf). Mirrors `qpdf --recompress-flate`.
    #[arg(long = "recompress-flate")]
    recompress_flate: bool,

    /// Set the zlib compression level used when emitting Flate streams
    /// (qpdf `--compression-level=level`).
    #[arg(long = "compression-level", value_name = "LEVEL")]
    compression_level: Option<String>,

    /// Flatten annotations into page content (qpdf `--flatten-annotations`).
    ///
    /// MODE is `all`, `screen`, or `print`:
    /// - `all`: bake every visible annotation into the page content stream.
    /// - `screen`: annotations that render on screen, including printable annotations.
    /// - `print`: only annotations flagged for printing.
    ///
    /// Combine with `--generate-appearances` to first synthesize missing
    /// form-field appearance streams; generation always runs before
    /// flattening. Requires a full rewrite of the document.
    #[arg(
        long = "flatten-annotations",
        value_enum,
        value_name = "MODE",
        help = "Flatten annotations into page content; MODE is all, screen, or print"
    )]
    flatten_annotations: Option<CliFlattenMode>,

    /// Generate appearance streams for form fields that lack them
    /// (qpdf `--generate-appearances`).
    ///
    /// Only runs if the document's `/AcroForm` indicates its appearances are
    /// out of date (`/NeedAppearances true`); otherwise this is a no-op.
    /// When it runs, form fields whose widgets have no `/AP` `/N` appearance
    /// are rendered from their current value (`/V`) and default appearance
    /// (`/DA`). Useful before `--flatten-annotations` so value-only fields
    /// are not dropped. Requires a full rewrite of the document.
    #[arg(
        long = "generate-appearances",
        help = "Generate appearance streams for form fields that lack them"
    )]
    generate_appearances: bool,

    /// Recompress eligible non-JPEG images as DCT/JPEG (qpdf
    /// `--optimize-images`).
    #[arg(long = "optimize-images")]
    optimize_images: bool,
    /// Exclude inline images from the optimization pass.
    #[arg(long = "keep-inline-images")]
    keep_inline_images: bool,
    /// Minimum image width for `--optimize-images` (qpdf default: 128).
    #[arg(long = "oi-min-width", value_name = "WIDTH")]
    oi_min_width: Option<String>,
    /// Minimum image height for `--optimize-images` (qpdf default: 128).
    #[arg(long = "oi-min-height", value_name = "HEIGHT")]
    oi_min_height: Option<String>,
    /// Minimum image area for `--optimize-images` (qpdf default: 16384).
    #[arg(long = "oi-min-area", value_name = "AREA")]
    oi_min_area: Option<String>,
    /// Minimum inline-image payload to externalize (qpdf default: 1024).
    #[arg(long = "ii-min-bytes", value_name = "BYTES")]
    ii_min_bytes: Option<String>,

    /// Flatten page rotation by baking `/Rotate` into page content
    /// (qpdf `--flatten-rotation`).
    ///
    /// Removes each page's `/Rotate` entry and rewrites its content,
    /// `/MediaBox`, and annotation rectangles so the visible orientation is
    /// unchanged. Requires a full rewrite of the document.
    #[arg(
        long = "flatten-rotation",
        help = "Flatten page rotation by baking /Rotate into content"
    )]
    flatten_rotation: bool,

    /// qpdf-compatible page-operation flags (--pages / --rotate /
    /// --split-pages / --collate / --empty). See [`PageOpArgs`].
    #[command(flatten)]
    page_ops: PageOpArgs,

    // ── Overlay / underlay flags ──────────────────────────────────────────
    // qpdf --overlay / --underlay impose pages from another file on top of
    // (overlay) or beneath (underlay) the destination pages. Both are
    // REPEATABLE and each group is terminated by a bare `--`. Within a group
    // the file token and sub-options may appear in any order:
    //   {--overlay|--underlay} [--file=]f [--password=p] [--to=R] [--from=R]
    //                          [--repeat=R] --
    //
    // The repeated occurrences and their per-group boundaries are extracted
    // from the raw argv by `preprocess_qpdf_args` BEFORE clap parses (clap's
    // derive flattens repeated `Vec<String>` occurrences, losing the group
    // boundary and the per-group declaration order needed for byte-identical
    // composition). These two fields exist only so `--help` documents the
    // flags and so a leaked token is accepted; the value vectors are not read.
    /// Overlay pages from another file on top of the destination pages (qpdf
    /// `--overlay`). Repeatable; terminate each group with `--`.
    ///
    /// Syntax: `--overlay [--file=]FILE [--password=PW] [--to=R] [--from=R]
    ///          [--repeat=R] --`. Within a group the file token and sub-options
    /// may appear in any order. Pages are stacked in order of appearance: first
    /// underlays, then the original page, then overlays.
    #[arg(
        long = "overlay",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "[--file=]FILE [sub-flags]",
        help = "Overlay pages from FILE on top of the output (qpdf --overlay); \
                repeatable, terminate each group with --"
    )]
    overlay: Vec<OsString>,

    /// Underlay pages from another file beneath the destination pages (qpdf
    /// `--underlay`). Repeatable; terminate each group with `--`.
    ///
    /// Syntax: `--underlay [--file=]FILE [--password=PW] [--to=R] [--from=R]
    ///          [--repeat=R] --`. Within a group the file token and sub-options
    /// may appear in any order. Pages are stacked in order of appearance: first
    /// underlays, then the original page, then overlays.
    #[arg(
        long = "underlay",
        num_args = 1..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "[--file=]FILE [sub-flags]",
        help = "Underlay pages from FILE beneath the output (qpdf --underlay); \
                repeatable, terminate each group with --"
    )]
    underlay: Vec<OsString>,

    /// Print verbose progress and diagnostic messages (mirrors qpdf --verbose).
    #[arg(
        long = "verbose",
        help = "Print verbose progress and diagnostic messages \
                (mirrors qpdf --verbose)"
    )]
    verbose: bool,

    /// Report approximate write progress (qpdf --progress).
    #[arg(long = "progress")]
    progress: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum CliObjectStreamMode {
    #[default]
    Preserve,
    Disable,
    Generate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliDecodeLevel {
    None,
    Generalized,
    Specialized,
    All,
}

impl From<CliDecodeLevel> for StreamDecodeLevel {
    fn from(value: CliDecodeLevel) -> Self {
        match value {
            CliDecodeLevel::None => Self::None,
            CliDecodeLevel::Generalized => Self::Generalized,
            CliDecodeLevel::Specialized => Self::Specialized,
            CliDecodeLevel::All => Self::All,
        }
    }
}

impl From<CliDecodeLevel> for DecodeLevel {
    fn from(value: CliDecodeLevel) -> Self {
        match value {
            CliDecodeLevel::None => Self::None,
            CliDecodeLevel::Generalized => Self::Generalized,
            CliDecodeLevel::Specialized => Self::Specialized,
            CliDecodeLevel::All => Self::All,
        }
    }
}

impl From<CliObjectStreamMode> for ObjectStreamMode {
    fn from(value: CliObjectStreamMode) -> Self {
        match value {
            CliObjectStreamMode::Preserve => ObjectStreamMode::Preserve,
            CliObjectStreamMode::Disable => ObjectStreamMode::Disable,
            CliObjectStreamMode::Generate => ObjectStreamMode::Generate,
        }
    }
}

/// Stream data mode for `--stream-data` (qpdf-compatible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliStreamDataMode {
    /// Pass streams through verbatim — no decode or re-encode.
    Preserve,
    /// Decode streams and emit raw bytes (no /Filter).
    Uncompress,
    /// Decode streams and re-encode with /FlateDecode.
    Compress,
}

impl From<CliStreamDataMode> for StreamDataMode {
    fn from(v: CliStreamDataMode) -> Self {
        match v {
            CliStreamDataMode::Preserve => StreamDataMode::Preserve,
            CliStreamDataMode::Uncompress => StreamDataMode::Uncompress,
            CliStreamDataMode::Compress => StreamDataMode::Compress,
        }
    }
}

/// `--flatten-annotations=all|screen|print` (qpdf-compatible).
///
/// Selects which annotations are baked into page content by
/// [`flatten_annotations`]:
/// - `all`: every visible annotation.
/// - `screen`: annotations that render on screen, including printable annotations.
/// - `print`: annotations flagged for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliFlattenMode {
    All,
    Screen,
    Print,
}

impl CliFlattenMode {
    /// Delegate qpdf's mode-to-mask mapping to the canonical job boundary.
    fn flags(self) -> (i64, i64) {
        FlattenAnnotationsMode::from(self).qpdf_flags()
    }
}

impl From<CliFlattenMode> for FlattenAnnotationsMode {
    fn from(value: CliFlattenMode) -> Self {
        match value {
            CliFlattenMode::All => Self::All,
            CliFlattenMode::Screen => Self::Screen,
            CliFlattenMode::Print => Self::Print,
        }
    }
}

/// y|n toggle used by --compress-streams, --normalize-content.
/// Clap variant names are `y` and `n` (lowercase single letter, qpdf-compatible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliYesNo {
    #[clap(name = "y")]
    Yes,
    #[clap(name = "n")]
    No,
}

/// Resolve qpdf's two-bit content-normalization state.
///
/// qpdf's `QPDFJob` only calls its content-normalization setter for an
/// explicit `--normalize-content` option. The writer then enables normalization
/// for QDF only when no explicit value was set, so `--qdf
/// --normalize-content=n` must remain distinguishable from an absent option.
fn normalize_content_enabled(setting: Option<CliYesNo>, qdf: bool) -> bool {
    match setting {
        Some(CliYesNo::Yes) => true,
        Some(CliYesNo::No) => false,
        None => qdf,
    }
}

/// `--newline-before-endstream=y|n|never` (qpdf default: never).
///
/// `never` requests qpdf's default framing (no newline between the stream
/// payload and `endstream`); `y` and `n` both enable qpdf's boolean option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliNewlineBeforeEndstream {
    #[clap(name = "y")]
    Yes,
    #[clap(name = "n")]
    No,
    #[clap(name = "never")]
    Never,
}

impl From<CliNewlineBeforeEndstream> for NewlineBeforeEndstream {
    fn from(v: CliNewlineBeforeEndstream) -> Self {
        match v {
            CliNewlineBeforeEndstream::Yes => NewlineBeforeEndstream::Yes,
            // qpdf treats `--newline-before-endstream=<value>` as the presence
            // of its boolean option, so `=n` has the same output as `=y` in
            // the 11.9.0 CLI.
            CliNewlineBeforeEndstream::No => NewlineBeforeEndstream::Yes,
            CliNewlineBeforeEndstream::Never => NewlineBeforeEndstream::Never,
        }
    }
}

/// `--remove-unreferenced-resources=auto|yes|no` (qpdf-compatible).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum CliRemoveUnreferencedResources {
    #[default]
    Auto,
    Yes,
    No,
}

impl From<CliRemoveUnreferencedResources> for RemoveUnreferencedResources {
    fn from(v: CliRemoveUnreferencedResources) -> Self {
        match v {
            CliRemoveUnreferencedResources::Auto => RemoveUnreferencedResources::Auto,
            CliRemoveUnreferencedResources::Yes => RemoveUnreferencedResources::Yes,
            CliRemoveUnreferencedResources::No => RemoveUnreferencedResources::No,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ClapArgs)]
struct RecoveryArgs {
    /// Ignore any cross-reference streams in the file, falling back to
    /// cross-reference tables or triggering document recovery.
    #[arg(long = "ignore-xref-streams")]
    ignore_xref_streams: bool,
    /// Avoid attempting to recover when errors are found in a file's
    /// cross reference table or stream lengths.
    #[arg(long = "suppress-recovery", conflicts_with = "repair")]
    suppress_recovery: bool,
}

#[derive(Debug, Clone, Default, ClapArgs)]
struct PasswordArgs {
    #[command(flatten)]
    recovery: RecoveryArgs,
    #[arg(skip)]
    verbose: bool,
    #[arg(skip)]
    raw_password: Option<Vec<u8>>,
    /// Password bytes for encrypted PDFs.
    #[arg(long, conflicts_with = "password_file")]
    password: Option<OsString>,
    /// File containing password bytes. Only the first LF-delimited line is
    /// used; a trailing CR before that LF is stripped. `-` reads from stdin.
    #[arg(long = "password-file", value_name = "PATH")]
    password_file: Option<PathBuf>,
    /// How qpdf-style password modes interpret --password bytes. On read
    /// paths, only `hex-bytes` transforms the bytes; `auto`, `bytes`, and
    /// `unicode` pass them through unchanged. Mirrors qpdf's flag.
    #[arg(long = "password-mode", value_enum, default_value_t = CliPasswordMode::Auto)]
    password_mode: CliPasswordMode,
    /// Permit creating deprecated RC4-backed handlers and revision 5
    /// encryption. Reading existing weakly encrypted PDFs does not require it.
    #[arg(long = "allow-weak-crypto")]
    allow_weak_crypto: bool,
    /// Interpret --password as the precomputed file encryption key in hex,
    /// not a user/owner password (qpdf --password-is-hex-key).
    #[arg(
        long = "password-is-hex-key",
        long_help = "Interpret the --password value as the precomputed file \
encryption key encoded as hex, NOT a user or owner password. All \
password→key derivation (Algorithm 2 / 2.A / 2.B / 6 / 7) is skipped and the \
decoded bytes are used directly as the file key for stream/string \
decryption. Upper- or lower-case hex and embedded whitespace are accepted; \
the decoded key must be at most 32 bytes. Mirrors qpdf \
--password-is-hex-key. Pair with `show-encryption-key` to recover the key \
from a known password, then reopen the file with that key."
    )]
    password_is_hex_key: bool,
    /// Disable qpdf's alternate password-encoding recovery attempts.
    #[arg(
        long = "suppress-password-recovery",
        long_help = "Accepted for qpdf script compatibility. qpdf retries \
alternate password encodings (UTF-8 / PDFDocEncoding) when authentication \
fails; this flag disables that recovery."
    )]
    suppress_password_recovery: bool,
}

impl PasswordArgs {
    fn password_bytes(&self) -> Option<Vec<u8>> {
        self.raw_password.clone().or_else(|| {
            self.password
                .as_ref()
                .map(|password| arg_parser::os_bytes(password))
        })
    }

    fn set_password_bytes(&mut self, password: Option<Vec<u8>>) {
        self.raw_password = password.clone();
        self.password = password.map(|password| arg_parser::os_string_from_bytes(&password));
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum CliPasswordMode {
    #[default]
    Auto,
    Bytes,
    #[clap(name = "hex-bytes")]
    HexBytes,
    Unicode,
}

impl From<CliPasswordMode> for PasswordMode {
    fn from(value: CliPasswordMode) -> Self {
        match value {
            CliPasswordMode::Auto => PasswordMode::Auto,
            CliPasswordMode::Bytes => PasswordMode::Bytes,
            CliPasswordMode::HexBytes => PasswordMode::HexBytes,
            CliPasswordMode::Unicode => PasswordMode::Unicode,
        }
    }
}

/// Env var that suppresses the `--static-id` "testing only" warning.
///
/// `--static-id` exists purely so test/parity harnesses can produce a
/// byte-stable trailer `/ID`; it must never be used for production output.
/// flpdf's *native* surface (`rewrite --static-id`) therefore emits a stderr
/// warning whenever the flag is requested. The top-level
/// qpdf-shaped alias (`flpdf --static-id …`) exists solely to mirror qpdf's
/// command surface, and qpdf emits no such warning — so the alias stays
/// silent to honour that contract and keep the qtest parity suite green.
///
/// This env var opts the *native* surface out of the diagnostic: harnesses
/// that exercise `rewrite --static-id` and assert on a clean stderr set it.
/// It is deliberately *not* a CLI flag (the qpdf-shaped alias has no such
/// switch).
const STATIC_ID_QUIET_ENV: &str = "FLPDF_STATIC_ID_QUIET";

/// Returns true when `--static-id` was requested via flpdf's native
/// `rewrite` subcommand. The top-level qpdf-shaped alias deliberately does
/// *not* count here: it mirrors qpdf, which is silent for `--static-id`.
fn static_id_warning_applies(args: &Cli) -> bool {
    matches!(&args.command, Some(Commands::Rewrite(cmd)) if cmd.static_id)
}

/// Emit the test-only warning for `--static-id` exactly once, unless
/// suppressed via [`STATIC_ID_QUIET_ENV`]. Writes to stderr only and never
/// changes the process exit code. Only the native `rewrite` surface warns;
/// the top-level qpdf-shaped alias stays silent for qpdf parity.
fn warn_if_static_id(args: &Cli) {
    if !static_id_warning_applies(args) {
        return;
    }
    if std::env::var_os(STATIC_ID_QUIET_ENV).is_some() {
        return;
    }
    emit_logger_error(
        "flpdf: warning: --static-id is for testing only and must not be used for production output\n",
    );
}

fn preprocess_qpdf_args<T: Into<OsString>>(args: Vec<T>) -> CliResult<PreprocessedArgs> {
    let args = args.into_iter().map(Into::into).collect();
    let parsed = arg_parser::ArgParser::from_command(cli_command()).parse_os(args)?;
    let mut overlay_specs = Vec::new();
    let mut attachment_segments = Vec::new();
    let mut raw_encrypt = None;
    let mut raw_pages = None;
    let mut raw_copy_attachments_from = Vec::new();

    for segment in parsed.raw_named_segments {
        let option = segment.option;
        let tokens = segment
            .tokens
            .into_iter()
            .map(|token| token.as_bytes().to_vec())
            .collect::<Vec<_>>();
        match option.as_str() {
            "overlay" => overlay_specs.push(parse_overlay_segment(OverlayKind::Overlay, &tokens)?),
            "underlay" => {
                overlay_specs.push(parse_overlay_segment(OverlayKind::Underlay, &tokens)?)
            }
            "add-attachment" => attachment_segments.push(tokens),
            "encrypt" => raw_encrypt = Some(tokens),
            "pages" => {
                // qpdf rejects a second --pages group as a usage error
                // (`QPDFJob_config.cc:945-951`).
                if raw_pages.is_some() {
                    return Err(Box::new(UsageError::new(
                        "--pages may only be specified one time".to_owned(),
                    )));
                }
                raw_pages = Some(tokens)
            }
            "copy-attachments-from" => {
                // qpdf accumulates every --copy-attachments-from group and
                // copies from all of them (`QPDFJob.hh:683`,
                // `QPDFJob_config.cc:825-833`, `QPDFJob.cc:2089-2100`).
                raw_copy_attachments_from.push(tokens)
            }
            _ => {}
        }
    }

    Ok(PreprocessedArgs {
        residual_args: parsed.residual_args,
        overlay_specs,
        attachment_segments,
        raw_overrides: RawCliOverrides {
            password: raw_option_value(&parsed.raw_residual_args, "password"),
            encryption_file_password: raw_option_value(
                &parsed.raw_residual_args,
                "encryption-file-password",
            ),
            raw_encrypt,
            raw_pages,
            raw_copy_attachments_from: (!raw_copy_attachments_from.is_empty())
                .then_some(raw_copy_attachments_from),
        },
    })
}

fn raw_option_value(args: &[arg_parser::RawArg], name: &str) -> Option<Vec<u8>> {
    let mut attached_prefix = b"--".to_vec();
    attached_prefix.extend_from_slice(name.as_bytes());
    let mut separate_option = b"--".to_vec();
    separate_option.extend_from_slice(name.as_bytes());
    let mut found = None;
    let mut index = 1;
    while index < args.len() {
        let bytes = args[index].as_bytes();
        if bytes == b"--" {
            break;
        }
        if is_named_segment_option(bytes) {
            index += 1;
            while index < args.len() && args[index].as_bytes() != b"--" {
                index += 1;
            }
            index += usize::from(index < args.len());
            continue;
        }
        if let Some(value) = bytes
            .strip_prefix(attached_prefix.as_slice())
            .and_then(|value| {
                value
                    .first()
                    .is_some_and(|byte| *byte == b'=')
                    .then(|| value[1..].to_vec())
            })
        {
            found = Some(value);
        } else if bytes == separate_option.as_slice() {
            if let Some(value) = args.get(index + 1) {
                found = Some(value.as_bytes().to_vec());
                index += 1;
            }
        }
        index += 1;
    }
    found
}

fn is_named_segment_option(bytes: &[u8]) -> bool {
    matches!(
        bytes,
        b"--encrypt"
            | b"--pages"
            | b"--add-attachment"
            | b"--copy-attachments-from"
            | b"--overlay"
            | b"--underlay"
    )
}

fn raw_os_args(args: &[OsString]) -> Vec<Vec<u8>> {
    args.iter().map(|arg| arg_parser::os_bytes(arg)).collect()
}

fn apply_raw_overrides(args: &mut Cli, overrides: RawCliOverrides) {
    let RawCliOverrides {
        password,
        encryption_file_password,
        raw_encrypt,
        raw_pages,
        raw_copy_attachments_from,
    } = overrides;
    args.password.raw_password = password.clone();
    args.raw_encryption_file_password = encryption_file_password.clone().or_else(|| {
        args.encryption_file_password
            .as_ref()
            .map(|value| arg_parser::os_bytes(value))
    });
    args.raw_encrypt =
        raw_encrypt.or_else(|| args.encrypt.as_ref().map(|tokens| raw_os_args(tokens)));
    args.page_ops.raw_pages = raw_pages
        .or_else(|| (!args.page_ops.pages.is_empty()).then(|| raw_os_args(&args.page_ops.pages)));
    args.raw_copy_attachments_from = raw_copy_attachments_from.or_else(|| {
        (!args.copy_attachments_from.is_empty())
            .then(|| vec![raw_os_args(&args.copy_attachments_from)])
    });

    if let Some(command) = args.command.as_mut() {
        match command {
            Commands::Check(command) => command.password.raw_password = password.clone(),
            Commands::DumpObject(command) => command.password.raw_password = password.clone(),
            Commands::Pages(command) => command.password.raw_password = password.clone(),
            Commands::Qdf(command) => command.password.raw_password = password.clone(),
            Commands::ShowStream(command) => command.password.raw_password = password.clone(),
            Commands::ShowEncryption(command)
            | Commands::RequiresPassword(command)
            | Commands::ShowEncryptionKey(command) => {
                command.password.raw_password = password.clone()
            }
            Commands::Rewrite(command) => {
                command.password.raw_password = password.clone();
                command.raw_encryption_file_password = args.raw_encryption_file_password.clone();
                command.raw_encrypt = args.raw_encrypt.clone();
                command.page_ops.raw_pages = args.page_ops.raw_pages.clone();
                command.raw_copy_attachments_from = args.raw_copy_attachments_from.clone();
            }
            Commands::CheckLinearization(_)
            | Commands::IsEncrypted(_)
            | Commands::QdfFix(_)
            | Commands::ZlibFlate(_) => {}
        }
    }
}

// The flattened clap model is deep enough that constructing it can exhaust
// Windows' default process stack after a small option addition. Keep every
// production command-construction boundary on a grown stack; the returned
// command itself and all parsing behavior remain unchanged.
const CLI_COMMAND_STACK_RED_ZONE: usize = 1024 * 1024;
const CLI_COMMAND_STACK_GROWTH_SIZE: usize = 1024 * 1024;

fn cli_command() -> clap::Command {
    stacker::maybe_grow(
        CLI_COMMAND_STACK_RED_ZONE,
        CLI_COMMAND_STACK_GROWTH_SIZE,
        Cli::command,
    )
}

fn cli_parse_from(args: Vec<OsString>) -> Cli {
    stacker::maybe_grow(
        CLI_COMMAND_STACK_RED_ZONE,
        CLI_COMMAND_STACK_GROWTH_SIZE,
        || Cli::parse_from(args),
    )
}

/// Print qpdf's sole-option version response (`QPDFJob_argv.cc:99-105`).
fn print_qpdf_version() {
    emit_logger_info(format!(
        "qpdf version {}\nRun qpdf --copyright to see copyright and license information.\n",
        flpdf::qpdf_version()
    ));
}

/// Print qpdf's sole-option copyright response (`QPDFJob_argv.cc:108-135`).
fn print_qpdf_copyright() {
    emit_logger_info(format!(
        "qpdf version {}\n\n\
Copyright (c) 2005-2024 Jay Berkenbilt\n\
QPDF is licensed under the Apache License, Version 2.0 (the \"License\");\n\
you may not use this file except in compliance with the License.\n\
You may obtain a copy of the License at\n\n  http://www.apache.org/licenses/LICENSE-2.0\n\n\
Unless required by applicable law or agreed to in writing, software\n\
distributed under the License is distributed on an \"AS IS\" BASIS,\n\
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.\n\
See the License for the specific language governing permissions and\n\
limitations under the License.\n\n\
Versions of qpdf prior to version 7 were released under the terms\n\
of version 2.0 of the Artistic License. At your option, you may\n\
continue to consider qpdf to be licensed under those terms. Please\n\
        see the manual for additional information.\n",
        flpdf::qpdf_version()
    ));
}

fn main() {
    // One private qpdf-style logger owns all document routes for this
    // invocation. It is deliberately distinct from the library process
    // default so later save/info routing can be configured as one unit.
    let _ = cli_logger();
    // Parse qpdf's argv grammar before clap parses feature values. The parser
    // preserves named segment boundaries and returns feature-neutral raw
    // tokens to the existing semantic consumers below.
    let raw_args: Vec<OsString> = std::env::args_os().collect();
    if raw_args
        .first()
        .is_some_and(|program| is_zlib_flate_program(program))
    {
        if let Err(error) = run_zlib_flate(&raw_args[1..], "zlib-flate", "zlib-flate") {
            let code = error
                .downcast_ref::<CliExitError>()
                .map_or(ExitCode::Errors, |error| error.code);
            std::process::exit(code.as_i32());
        }
        return;
    }
    if raw_args.len() == 2 {
        match raw_args[1].to_str() {
            Some("--version") | Some("-version") => {
                print_qpdf_version();
                return;
            }
            Some("--copyright") | Some("-copyright") => {
                print_qpdf_copyright();
                return;
            }
            _ => {}
        }
    }

    let PreprocessedArgs {
        residual_args,
        overlay_specs,
        attachment_segments,
        raw_overrides,
    } = match preprocess_qpdf_args(raw_args) {
        Ok(parsed) => parsed,
        Err(error) => {
            emit_logger_error(format!("flpdf: {error}\n"));
            std::process::exit(2);
        }
    };
    let mut args = cli_parse_from(residual_args);
    apply_raw_overrides(&mut args, raw_overrides);
    // qpdf keeps --verbose on QPDFJob rather than on the password parser, but
    // the reader owns the authentication retry boundary in flpdf. Carry the
    // job policy through the existing PasswordArgs copy used by every open
    // helper so the qpdf retry diagnostic is emitted before the alternate
    // candidate is attempted.
    args.password.verbose = args.verbose;
    validate_collate_values(&args.page_ops.collate);
    if let Err(error) = validate_keep_files_open_threshold(&args.page_ops) {
        emit_logger_error(format!("flpdf: {error}\n"));
        std::process::exit(2);
    }
    let top_level_version_options =
        match parse_cli_version_options(args.min_version.as_deref(), args.force_version.as_deref())
        {
            Ok(options) => options,
            Err(error) => {
                emit_logger_error(format!("flpdf: {error}\n"));
                std::process::exit(1);
            }
        };
    let top_level_compression_level =
        match parse_compression_level(args.compression_level.as_deref()) {
            Ok(level) => level,
            Err(error) => {
                emit_logger_error(format!("flpdf: {error}\n"));
                std::process::exit(2);
            }
        };
    let top_level_image_options = match image_optimization_options(
        args.keep_inline_images,
        args.oi_min_width.as_deref(),
        args.oi_min_height.as_deref(),
        args.oi_min_area.as_deref(),
        args.ii_min_bytes.as_deref(),
    ) {
        Ok(options) => options,
        Err(error) => {
            emit_logger_error(format!("flpdf: {error}\n"));
            std::process::exit(2);
        }
    };
    // QPDFWriter::doWriteSetup clears QDF before deriving QDF's implicit
    // normalization defaults for linearized output (`QPDFWriter.cc:2068-2080`).
    // Keep an explicit --normalize-content value, but do not synthesize the
    // QDF default when --linearize will clear QDF in the writer.
    let normalize_content =
        normalize_content_enabled(args.normalize_content, args.qdf && !args.linearize);

    // --static-id produces a fixed, non-unique trailer /ID. It exists only
    // for deterministic test/parity output. The native `rewrite --static-id`
    // surface warns loudly (stderr only, exit code unchanged) so it is never
    // mistaken for a production option; the top-level qpdf-shaped alias stays
    // silent to mirror qpdf. Done here, after clap parsing
    // succeeds and before any rewrite work, so the warning never precedes a
    // usage error yet is always visible.
    warn_if_static_id(&args);

    // `--overlay`/`--underlay` groups are stripped from argv before clap by
    // `preprocess_qpdf_args`, so a stripped group leaves no trace for the
    // dispatch chain. Only the rewrite paths (the `Rewrite` subcommand and the
    // top-level default/`--linearize` rewrite branches) consume `overlay_specs`;
    // every other command/mode would silently ignore it. Reject that here so an
    // overlay on, e.g., `check`/`--show-npages`/`--pages` fails loudly instead of
    // being dropped. The top-level predicate mirrors the dispatch chain below
    // (the rewrite branch is the final `else`, reached only when no inspection,
    // attachment, json, or page-op mode is selected) and must stay in sync with
    // it; page-operation output is dispatched to the page-operation writer
    // boundary, including when `--linearize` is present.
    if !overlay_specs.is_empty() {
        let target_is_rewrite = match &args.command {
            Some(Commands::Rewrite(_)) => true,
            Some(_) => false,
            None => {
                args.json.is_none()
                    && args.show_object.is_none()
                    && !args.show_npages
                    && !args.show_pages
                    && !args.show_xref
                    && !args.check_linearization
                    && !args.show_linearization
                    && !args.show_encryption
                    && !args.check
                    && !args.list_attachments
                    && args.show_attachment.is_none()
                    && args.remove_attachment.is_empty()
                    && args.add_attachment.is_empty()
                    && args.copy_attachments_from.is_empty()
            }
        };
        if !target_is_rewrite {
            emit_logger_error(
                "flpdf: --overlay/--underlay can only be used with rewrite output, \
                 not with inspection or other commands\n",
            );
            std::process::exit(2);
        }
    }

    let json_input_inspection = (args.json_input || args.update_from_json.is_some())
        && (args.check
            || args.show_npages
            || args.show_pages
            || args.show_xref
            || args.show_encryption);

    // JSON-input/update inspection is routed through the already-created job
    // document before the ordinary file-backed inspection branches. qpdf
    // creates or updates the QPDF object first, then runs read-only consumers
    // such as --check and --show-pages on that same object.
    // For ordinary JSON output, the separate --json branch remains first among
    // the non-inspection modes and retains its existing validation boundary.
    let result = if let Some(path) = args.job_json_file.as_deref() {
        run_job_json_file(
            path,
            args.input.as_deref(),
            args.output.as_deref(),
            &args.password,
            args.no_warn,
        )
    } else if json_input_inspection {
        run_json_input_inspection(&args)
    } else if args.json.is_some() || args.json_output.is_some() {
        run_json(&args, top_level_image_options)
    } else if let Some(command) = args.command {
        run_command(command, &overlay_specs)
    } else if args.is_encrypted {
        if args.page_ops.empty {
            run_empty_document_encryption_status()
        } else {
            match args.input.as_ref() {
                Some(input) => run_is_encrypted(input, args.repair, &args.password, args.no_warn),
                None => Err("--is-encrypted requires an input file".into()),
            }
        }
    } else if args.requires_password {
        if args.page_ops.empty {
            run_empty_document_encryption_status()
        } else {
            match args.input.as_ref() {
                Some(input) => {
                    run_requires_password(input, args.repair, &args.password, args.no_warn)
                }
                None => Err("--requires-password requires an input file".into()),
            }
        }
    } else if let Some(object_ref) = args.show_object.as_deref() {
        run_show_object(
            args.input,
            args.repair,
            &args.password,
            object_ref,
            args.raw_stream_data,
            args.filtered_stream_data,
            args.no_warn,
        )
    } else if args.show_npages {
        run_show_npages(args.input, args.repair, &args.password, args.no_warn)
    } else if args.show_pages {
        run_show_pages(
            args.input,
            args.repair,
            &args.password,
            args.with_images,
            args.no_warn,
        )
    } else if args.show_xref {
        run_show_xref(args.input, args.repair, &args.password, args.no_warn)
    } else if args.check_linearization {
        run_check_linearization(args.input, args.repair, &args.password, args.no_warn)
    } else if args.show_linearization {
        run_show_linearization(args.input, args.repair, &args.password, args.no_warn)
    } else if args.show_encryption {
        match args.input.as_ref() {
            Some(input) => run_show_encryption(
                input,
                args.repair,
                &args.password,
                args.no_warn,
                args.show_encryption_key,
            ),
            None => Err("--show-encryption requires an input file".into()),
        }
    } else if args.check {
        run_check(
            args.input,
            args.repair,
            &args.password,
            args.no_warn,
            args.show_encryption_key,
        )
    } else if args.list_attachments {
        run_list_attachments(
            args.input,
            args.repair,
            &args.password,
            args.verbose,
            args.no_warn,
        )
    } else if let Some(key) = args.show_attachment {
        run_show_attachment(args.input, args.repair, &args.password, &key, args.no_warn)
    } else if !args.remove_attachment.is_empty() {
        let options = top_level_writer_options(
            &args,
            normalize_content,
            top_level_compression_level,
            &top_level_version_options,
        );
        run_remove_attachment(
            args.input,
            args.output,
            args.repair,
            &args.password,
            &args.remove_attachment,
            args.verbose,
            args.no_warn,
            args.remove_restrictions,
            args.linearize,
            args.linearize_pass1.as_deref(),
            options,
        )
    } else if !args.add_attachment.is_empty() {
        let options = top_level_writer_options(
            &args,
            normalize_content,
            top_level_compression_level,
            &top_level_version_options,
        );
        run_add_attachment(
            args.input,
            args.output,
            args.repair,
            &args.password,
            attachment_segments,
            args.verbose,
            args.no_warn,
            args.remove_restrictions,
            args.linearize,
            args.linearize_pass1.as_deref(),
            options,
        )
    } else if !args.copy_attachments_from.is_empty() {
        let copy_groups = args
            .raw_copy_attachments_from
            .clone()
            .unwrap_or_else(|| vec![raw_os_args(&args.copy_attachments_from)]);
        let options = top_level_writer_options(
            &args,
            normalize_content,
            top_level_compression_level,
            &top_level_version_options,
        );
        run_copy_attachments_from(
            args.input,
            args.output,
            args.repair,
            &args.password,
            copy_groups,
            args.verbose,
            args.no_warn,
            args.remove_restrictions,
            args.linearize,
            args.linearize_pass1.as_deref(),
            options,
        )
    } else if args.linearize && !page_ops_active(&args.page_ops) {
        let options = top_level_writer_options(
            &args,
            normalize_content,
            top_level_compression_level,
            &top_level_version_options,
        );
        let result = run_rewrite(
            args.input,
            args.output.clone(),
            args.repair,
            &args.password,
            args.json_input,
            args.update_from_json.as_deref(),
            true,
            args.linearize_pass1.as_deref(),
            args.remove_restrictions,
            args.decrypt,
            normalize_content,
            args.coalesce_contents,
            args.remove_unreferenced_resources,
            args.generate_appearances,
            args.optimize_images.then_some(top_level_image_options),
            args.flatten_annotations,
            false, // flatten_rotation (not on top-level surface)
            &overlay_specs,
            args.verbose,
            args.no_warn,
            options,
        );
        result
    } else if page_ops_active(&args.page_ops) {
        // Top-level page-operation path (qpdf-shaped invocation:
        // `flpdf in.pdf --pages . 1-3 -- out.pdf`). Mirrors the `rewrite`
        // subcommand's page-op dispatch below.
        //
        // The page-op pipeline does not thread `WriterOptions.encrypt`
        // through to the page-extraction / page-rewrite paths, so
        // silently honoring `--encrypt` here would emit plaintext output
        // even though the user asked for encryption. Reject upfront with
        // the same shape `rewrite --encrypt --pages …` already uses
        // (mirrors the existing `--decrypt` / `--remove-restrictions`
        // rejection in the subcommand surface). Wiring encryption
        // through the page-op pipeline is unsupported, so reject the option
        // before any page operation runs.
        if args.encrypt.is_some() {
            emit_logger_error(
                "flpdf: --encrypt is not applied in the \
                 --pages/--rotate/--split-pages/--collate pipeline; \
                 rerun without --encrypt or without the page operation\n",
            );
            std::process::exit(1);
        }
        if args.copy_encryption.is_some() {
            emit_logger_error(
                "flpdf: --copy-encryption is not applied in the \
                 --pages/--rotate/--split-pages/--collate pipeline; \
                 rerun without --copy-encryption or without the page operation\n",
            );
            std::process::exit(1);
        }
        let mut options = WriterOptions {
            static_id: args.static_id,
            deterministic_id: args.deterministic_id,
            static_aes_iv: args.static_aes_iv,
            no_original_object_ids: args.no_original_object_ids,
            preserve_unreferenced_objects: args.preserve_unreferenced,
            progress: args.progress,
            recompress_flate: args.recompress_flate,
            compression_level: top_level_compression_level,
            object_streams: args.object_streams.into(),
            stream_data: args.stream_data.map(Into::into),
            content_normalization: normalize_content,
            content_normalization_set: args.normalize_content.is_some(),
            qdf: args.qdf,
            // qpdf applies `--newline-before-endstream` to every output
            // writer (`QPDFWriter.cc:1560`), including page-operation output.
            newline_before_endstream: args.newline_before_endstream.into(),
            password_mode: args.password.password_mode.into(),
            ..WriterOptions::default()
        };
        apply_cli_decode_level(&mut options, args.decode_level);
        apply_cli_version_options(&mut options, &top_level_version_options);
        if let Some(ref cs) = args.compress_streams {
            match cs.as_str() {
                "y" => options.compress_streams = Some(CompressStreams::Yes),
                "n" => options.compress_streams = Some(CompressStreams::No),
                other => {
                    emit_logger_error(format!(
                        "flpdf: --compress-streams must be y or n, got: {:?}\n",
                        other
                    ));
                    std::process::exit(2);
                }
            }
        }
        if args.page_ops.empty && !args.page_ops.pages.is_empty() && args.output.is_none() {
            match args.input.clone() {
                Some(output) => run_empty_page_extraction(
                    &output,
                    args.repair,
                    &args.password,
                    args.update_from_json.as_deref(),
                    &args.page_ops,
                    &overlay_specs,
                    args.remove_unreferenced_resources,
                    options,
                    args.linearize,
                    args.linearize_pass1.as_deref(),
                    args.optimize_images.then_some(top_level_image_options),
                    args.verbose,
                    args.no_warn,
                ),
                None => Err("--empty page operations require an output file".into()),
            }
        } else {
            let dispatch = |input: PathBuf, output: PathBuf| -> CliResult<()> {
                if !args.page_ops.pages.is_empty() {
                    run_page_extraction(
                        &input,
                        &output,
                        args.repair,
                        &args.password,
                        args.json_input,
                        args.update_from_json.as_deref(),
                        &args.page_ops,
                        &overlay_specs,
                        args.remove_unreferenced_resources,
                        options.clone(),
                        args.linearize,
                        args.linearize_pass1.as_deref(),
                        args.optimize_images.then_some(top_level_image_options),
                        args.verbose,
                        args.no_warn,
                    )
                } else {
                    if !overlay_specs.is_empty() {
                        emit_logger_error(
                            "flpdf: --overlay/--underlay is not applied with \
                             --rotate/--split-pages alone (no --pages); \
                             rerun with --pages or without the overlay\n",
                        );
                        std::process::exit(1);
                    }
                    run_rewrite_with_page_ops(
                        &input,
                        &output,
                        args.repair,
                        &args.password,
                        args.json_input,
                        args.update_from_json.as_deref(),
                        &args.page_ops,
                        args.remove_unreferenced_resources,
                        options.clone(),
                        args.linearize,
                        args.linearize_pass1.as_deref(),
                        args.optimize_images.then_some(top_level_image_options),
                        args.verbose,
                        args.no_warn,
                    )
                }
            };
            match (args.input.clone(), args.output.clone()) {
                (Some(i), Some(o)) => dispatch(i, o),
                _ => Err("page operations require both an input and an output file".into()),
            }
        }
    } else {
        let options = top_level_writer_options(
            &args,
            normalize_content,
            top_level_compression_level,
            &top_level_version_options,
        );
        run_rewrite(
            args.input,
            args.output,
            args.repair,
            &args.password,
            args.json_input,
            args.update_from_json.as_deref(),
            false,
            None,
            args.remove_restrictions,
            args.decrypt,
            normalize_content,
            args.coalesce_contents,
            args.remove_unreferenced_resources,
            args.generate_appearances,
            args.optimize_images.then_some(top_level_image_options),
            args.flatten_annotations,
            false, // flatten_rotation (not on top-level surface)
            &overlay_specs,
            args.verbose,
            args.no_warn,
            options,
        )
    };

    if let Err(error) = result {
        // If the error carries an explicit exit code (e.g. from run_check),
        // honour it.  Unknown/generic errors fall back to exit 2 (qpdf
        // convention for "error", unchanged from before this change).
        if let Some(exit_err) = error.downcast_ref::<CliExitError>() {
            // Only print a message when there is one; the caller may have
            // already printed its own summary (e.g. run_check prints the qpdf
            // "checking" block before returning exit 3 for warnings, and its
            // exit-2 path passes an empty message because the error diagnostics
            // were already printed in qpdf shape).
            if !exit_err.message.is_empty() {
                emit_logger_error(format!("\n{}: {}\n", progname(), exit_err.message));
            }
            std::process::exit(exit_err.code.as_i32());
        }
        if let Some(usage_error) = find_usage_error(error.as_ref()) {
            usage_exit(usage_error);
        }
        if let Some(message) = find_raw_error_message(error.as_ref()) {
            let mut line = progname().into_bytes();
            line.extend_from_slice(b": ");
            line.extend_from_slice(message);
            line.push(b'\n');
            emit_logger_error(line);
            std::process::exit(2);
        }
        if let Some(path_error) = error.downcast_ref::<CliPathError>() {
            let mut line = progname().into_bytes();
            line.extend_from_slice(b": ");
            if let Some(operation) = path_error.operation {
                line.extend_from_slice(operation.as_bytes());
                line.push(b' ');
            }
            line.extend_from_slice(&path_error.path);
            line.extend_from_slice(b": ");
            line.extend_from_slice(path_error.message.as_bytes());
            line.push(b'\n');
            emit_logger_error(line);
            std::process::exit(2);
        }
        emit_logger_error(format!("{}: {error}\n", progname()));
        std::process::exit(2);
    }
}

fn find_usage_error<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a UsageError> {
    if let Some(usage_error) = error.downcast_ref::<UsageError>() {
        return Some(usage_error);
    }
    if let Some(Error::Usage(usage_error)) = error.downcast_ref::<Error>() {
        return Some(usage_error);
    }
    error.source().and_then(find_usage_error)
}

fn find_raw_error_message<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a [u8]> {
    if let Some(error) = error.downcast_ref::<Error>() {
        if let Some(message) = error.raw_message() {
            return Some(message);
        }
    }
    error.source().and_then(find_raw_error_message)
}

fn usage_exit(error: &UsageError) -> ! {
    let who = progname();
    emit_logger_error(format!(
        "\n{who}: {error}\n\nFor help:\n  {who} --help=usage       usage information\n  \
{who} --help=topic       help on a topic\n  {who} --help=--option    help on an option\n  \
{who} --help             general help and a topic list\n\n"
    ));
    std::process::exit(2);
}

fn run_job_json_file(
    path: &Path,
    input: Option<&Path>,
    output: Option<&Path>,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<()> {
    let json = std::fs::read(path)
        .map_err(|error| error_with_file(path, Box::new(error) as Box<dyn std::error::Error>))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_suppress_warnings(suppress_warnings);

    job.initialize_from_json_partial_bytes(&json)
        .map_err(|error| {
            Box::new(CliExitError {
                code: ExitCode::Errors,
                message: format_job_json_error(path, error),
            }) as Box<dyn std::error::Error>
        })?;
    if let Some(input) = input {
        job.set_input_file(input.to_path_buf()).map_err(|error| {
            Box::new(CliExitError {
                code: ExitCode::Errors,
                message: format_job_json_error(path, error),
            }) as Box<dyn std::error::Error>
        })?;
    }
    if let Some(output) = output {
        job.set_output_file(output.to_path_buf()).map_err(|error| {
            Box::new(CliExitError {
                code: ExitCode::Errors,
                message: format_job_json_error(path, error),
            }) as Box<dyn std::error::Error>
        })?;
    }
    if let Some(password) = password.password_bytes() {
        job.set_password(password);
    }
    // The library JSON entry point uses qpdfjob's C-helper prefix while the
    // CLI's QPDFJob caller uses the ordinary qpdf prefix. Set the CLI
    // boundary after initialization, before any input warning or completion
    // summary is emitted.
    job.set_message_prefix(progname());
    finish_job_exit_status(job.run()?)
}

fn format_job_json_error(path: &Path, error: impl std::fmt::Display) -> String {
    format!(
        "error with job-json file {}: {error}\nRun {} --job-json-help for information on the file format.\n\nFor help:\n  {} --help=usage       usage information\n  {} --help=topic       help on a topic\n  {} --help=--option    help on an option\n  {} --help             general help and a topic list\n",
        path.display(),
        progname(),
        progname(),
        progname(),
        progname(),
        progname(),
    )
}

fn run_json(cli: &Cli, image_options: ImageOptimizationOptions) -> CliResult<()> {
    const QPDF_JSON_KEY_NAMES: &[&str] = &[
        "acroform",
        "attachments",
        "encrypt",
        "objectinfo",
        "objects",
        "outlines",
        "pagelabels",
        "pages",
        "qpdf",
    ];

    let json_output_mode = cli.json_output.is_some();
    let json_version = cli
        .json_output
        .as_deref()
        .or(cli.json.as_deref())
        .unwrap_or("2");
    let json_version = match json_version {
        "1" => 1,
        "2" | "latest" => 2,
        other => return Err(format!("unsupported json version {other}").into()),
    };

    // 1. Validate --json-key values before doing any I/O.
    let mut json_keys: Vec<JsonKey> = Vec::new();
    for raw in &cli.json_key {
        if json_version != 1 && matches!(raw.as_str(), "objects" | "objectinfo") {
            emit_logger_error(
                "flpdf: json keys \"objects\" and \"objectinfo\" are only valid for json version 1"
                    .to_owned()
                    + "\n",
            );
            std::process::exit(2);
        }
        if json_version == 1 && raw == "qpdf" {
            emit_logger_error("flpdf: json key \"qpdf\" is only valid for json version > 1\n");
            std::process::exit(2);
        }
        match JsonKey::from_str(raw.as_str()) {
            Some(k) => json_keys.push(k),
            None => {
                let names = QPDF_JSON_KEY_NAMES.join(",");
                emit_logger_error(format!(
                    "flpdf: --json-key must be given as --json-key={{{names}}}\n"
                ));
                std::process::exit(2);
            }
        }
    }
    if json_output_mode {
        if json_version == 1 {
            emit_logger_error("flpdf: --json-output requires JSON version 2\n");
            std::process::exit(2);
        }
        // qpdf's json-output mode always selects the qpdf key in addition to
        // any explicitly requested keys (`QPDFJob_config.cc:312-324`).
        if !json_keys.contains(&JsonKey::Qpdf) {
            json_keys.push(JsonKey::Qpdf);
        }
    }

    // 2. Validate --json-object selectors before doing any I/O.
    let mut json_objects: Vec<JsonObjectSelector> = Vec::new();
    for raw in &cli.json_object {
        match JsonObjectSelector::from_str(raw.as_str()) {
            Some(s) => json_objects.push(s),
            None => {
                emit_logger_error(format!(
                    "flpdf: --json-object selector \"{raw}\" must be 'trailer', 'N', or 'N,G'\n"
                ));
                std::process::exit(2);
            }
        }
    }

    // 3. Resolve stream-data mode.
    //
    // Ordinary `--json` follows the inspection default of no stream payloads;
    // qpdf's `--json-output` mode selects inline stream data unless the caller
    // overrides it with --json-stream-data.
    let stream_data = match cli
        .json_stream_data
        .as_deref()
        .unwrap_or(if json_output_mode { "inline" } else { "none" })
    {
        "none" => JsonStreamData::None,
        "inline" => JsonStreamData::Inline,
        "file" => JsonStreamData::File,
        other => {
            emit_logger_error(format!(
                "flpdf: --json-stream-data must be none, inline, or file; got: {other}\n"
            ));
            std::process::exit(2);
        }
    };

    // 4. Reject an output that identifies the input file before opening or
    // truncating it. qpdf performs this check in QPDFJob.cc:627-630. Path
    // spelling alone is insufficient: relative aliases, symlinks, and hard
    // links can all name the same underlying file.
    let input = cli.input.as_ref().ok_or("missing input file")?;
    if let Some(output) = cli
        .output
        .as_ref()
        .filter(|path| path.as_path() != Path::new("-"))
    {
        reject_same_json_output(input, output)?;
    }

    // qpdf reserves standard output for binary/structured save data before
    // opening the document, so warnings and later info cannot claim stdout.
    let mut standard_output = if cli
        .output
        .as_ref()
        .is_none_or(|path| path.as_path() == Path::new("-"))
    {
        Some(standard_save_writer()?)
    } else {
        None
    };

    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(cli.no_warn);

    // 5. Open the input once and retain an identity handle for the output
    // check. JSON input uses the canonical complete-document importer; a
    // partial update is applied immediately after creation/opening.
    let input_file = File::open(input).map_err(|error| {
        if cli.json_input {
            qpdf_json_input_open_error(input, error)
        } else {
            json_input_open_error(input, error)
        }
    })?;
    let input_identity = input_file
        .try_clone()
        .map_err(|error| error_with_file(input, error.into()))?;

    if cli.json_input {
        let mut pdf = job
            .create_from_json_document(input_file, path_description(input))
            .map_err(|error| json_error_with_file(input, Box::new(error)))?;
        apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
        if cli.optimize_images {
            flpdf::optimize_images(
                &mut pdf,
                &cli_logger(),
                &progname(),
                cli.verbose,
                image_options,
            )?;
        }
        let mut runtime = JsonJobRuntime {
            input_identity: &input_identity,
            standard_output: &mut standard_output,
            job: &mut job,
        };
        run_json_document(
            cli,
            &mut runtime,
            &mut pdf,
            json_version,
            cli.test_json_schema,
            cli.show_encryption_key,
            json_output_mode,
            stream_data,
            &json_keys,
            &json_objects,
        )
    } else {
        let mut pdf =
            open_pdf_from_file(input, input_file, cli.repair, &cli.password, cli.no_warn)?;
        job.record_document_warnings(&pdf);
        apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
        apply_json_page_specs(&mut job, &mut pdf, input, &cli.page_ops)?;
        if cli.optimize_images {
            flpdf::optimize_images(
                &mut pdf,
                &cli_logger(),
                &progname(),
                cli.verbose,
                image_options,
            )?;
        }
        let mut runtime = JsonJobRuntime {
            input_identity: &input_identity,
            standard_output: &mut standard_output,
            job: &mut job,
        };
        run_json_document(
            cli,
            &mut runtime,
            &mut pdf,
            json_version,
            cli.test_json_schema,
            cli.show_encryption_key,
            json_output_mode,
            stream_data,
            &json_keys,
            &json_objects,
        )
    }
}

/// Apply the single-source `--pages` operation before qpdf JSON output.
///
/// qpdf's `createQPDF` applies `handlePageSpecs` after opening and updating the
/// primary document, and only then does `writeQPDF` serialize JSON
/// (`libqpdf/QPDFJob.cc:428-480`). Keep the JSON route on the existing page-job
/// boundary so page-tree repair and inherited-attribute state are shared with
/// ordinary page operations.
fn apply_json_page_specs<R: Read + Seek + 'static>(
    job: &mut QPDFJob,
    pdf: &mut Pdf<R>,
    primary_input: &Path,
    page_ops: &PageOpArgs,
) -> CliResult<()> {
    if page_ops.pages.is_empty() {
        return Ok(());
    }

    let page_tokens = raw_page_tokens(page_ops);
    let raw_specs = parse_pages_segment(&page_tokens)?;
    let inputs = resolve_page_specs(&raw_specs, primary_input)?;
    if inputs.iter().any(|input| input.path != primary_input) {
        return Err("--pages: JSON output currently accepts only the primary input source".into());
    }
    let specs: Vec<_> = inputs
        .into_iter()
        .map(|input| PageSpecInput::new(0, input.range))
        .collect();
    let collate = parse_collate_values(&page_ops.collate)?;
    match job.handle_page_specs(
        std::slice::from_mut(pdf),
        &specs,
        collate.as_deref(),
        RemoveUnreferencedResources::Auto,
        false,
    )? {
        PageSpecJobOutput::InPlace {
            pdf,
            result,
            prune_mode,
        } => {
            QPDFJob::complete_in_place_page_selection(pdf, &result, prune_mode).map_err(Into::into)
        }
        PageSpecJobOutput::Merged(_) => {
            Err("--pages: JSON output unexpectedly selected a multi-source page job".into())
        }
    }
}

fn run_json_input_inspection(cli: &Cli) -> CliResult<()> {
    let input = cli.input.as_ref().ok_or("missing input file")?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(cli.no_warn);

    let file = File::open(input).map_err(|error| {
        if cli.json_input {
            qpdf_json_input_open_error(input, error)
        } else {
            open_error_with_file(input, error.into())
        }
    })?;

    if cli.json_input {
        let mut pdf = job
            .create_from_json_document(file, path_description(input))
            .map_err(|error| json_error_with_file(input, Box::new(error)))?;
        apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
        return run_job_inspection_on_pdf(cli, &mut job, &mut pdf);
    }

    let mut options = pdf_open_options(cli.repair, &cli.password)?;
    if cli.check {
        // `--check` is a read-only inspection and re-emits collected
        // diagnostics once in its check report rather than delivering them
        // during input creation.
        options.suppress_warnings = true;
    } else if cli.show_encryption {
        // `--show-encryption` has no deferred-replay report body (unlike
        // `--check`): open-time diagnostics are either delivered live
        // (matching qpdf's un-suppressed `--show-encryption` output) or, with
        // `--no-warn`, dropped entirely (matching qpdf, which prints no
        // WARNING lines at all in that case; see `run_show_encryption`).
        options.suppress_warnings = cli.no_warn;
    }
    let mut pdf =
        match job.open_with_description(BufReader::new(file), path_description(input), options) {
            Ok(pdf) => pdf,
            Err(error) => {
                job.report_open_failure(&error)?;
                return Err(error_with_file(input, actionable_password_error(error)));
            }
        };
    apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
    run_job_inspection_on_pdf(cli, &mut job, &mut pdf)
}

fn run_job_inspection_on_pdf<R: Read + Seek + 'static>(
    cli: &Cli,
    job: &mut QPDFJob,
    pdf: &mut Pdf<R>,
) -> CliResult<()> {
    job.set_with_images(cli.with_images);
    if cli.check {
        job.set_show_encryption_key(cli.show_encryption_key);
        return finish_check_job(job.check(pdf));
    }
    if cli.show_npages {
        return finish_job_exit_status(job.show_npages(pdf)?);
    }
    if cli.show_pages {
        return finish_job_exit_status(job.show_pages(pdf)?);
    }
    if cli.show_xref {
        return finish_job_exit_status(job.show_xref(pdf)?);
    }
    if cli.show_encryption {
        job.set_show_encryption_key(cli.show_encryption_key);
        return finish_show_encryption(job, pdf, cli.password.password_is_hex_key);
    }
    Err("JSON input/update inspection mode is missing a consumer".into())
}

/// Serialize an already-opened job document through the existing qpdf JSON
/// output consumer. Keeping this generic preserves the same output path for
/// file-backed PDF inputs and JSON-created documents.
#[allow(clippy::too_many_arguments)]
fn run_json_document<R: Read + Seek>(
    cli: &Cli,
    runtime: &mut JsonJobRuntime<'_>,
    pdf: &mut Pdf<R>,
    json_version: i32,
    test_json_schema: bool,
    show_encryption_key: bool,
    json_output_mode: bool,
    stream_data: JsonStreamData,
    json_keys: &[JsonKey],
    json_objects: &[JsonObjectSelector],
) -> CliResult<()> {
    // `decode_level` governs both inline `data` payloads and file-mode side
    // files emitted by the job-owned JSON output pipeline.
    let json_decode_level =
        cli.decode_level
            .map(DecodeLevel::from)
            .unwrap_or(if json_output_mode {
                DecodeLevel::None
            } else {
                DecodeLevel::Generalized
            });
    let output_path = cli
        .output
        .as_ref()
        .filter(|path| path.as_path() != Path::new("-"));
    let stream_prefix = cli.json_stream_prefix.as_deref().map(arg_parser::os_bytes);
    let json_result = if let Some(path) = output_path {
        let mut file = open_verified_json_output(runtime.input_identity, path)?;
        let options = JsonJobOptions {
            decode_level: json_decode_level,
            stream_data,
            stream_prefix: stream_prefix.as_deref(),
            keys: json_keys,
            objects: json_objects,
        };
        runtime.job.write_json_with_version(
            pdf,
            json_version,
            test_json_schema,
            json_output_mode,
            show_encryption_key,
            options,
            JsonJobOutput::File {
                filename: path,
                writer: &mut file,
            },
        )
    } else {
        let options = JsonJobOptions {
            decode_level: json_decode_level,
            stream_data,
            stream_prefix: stream_prefix.as_deref(),
            keys: json_keys,
            objects: json_objects,
        };
        runtime.job.write_json_with_version(
            pdf,
            json_version,
            test_json_schema,
            json_output_mode,
            show_encryption_key,
            options,
            JsonJobOutput::Stdout(
                runtime
                    .standard_output
                    .as_mut()
                    .expect("stdout writer prepared for JSON stdout"),
            ),
        )
    };
    match json_result {
        Ok(JobExitCode::Success) => {}
        Ok(JobExitCode::Error) => {
            return Err(Box::new(CliExitError {
                code: ExitCode::Errors,
                message: String::new(),
            }))
        }
        Ok(JobExitCode::Warning) => {
            return Err(Box::new(CliExitError {
                code: ExitCode::Warnings,
                message: String::new(),
            }))
        }
        Err(JsonJobError::Output(error)) => return Err(Box::new(Error::from(error))),
        Err(JsonJobError::Usage(error)) => return Err(Box::new(error)),
        Err(JsonJobError::Completion(error)) => return Err(Box::new(error)),
    }
    Ok(())
}

fn run_command(command: Commands, overlay_specs: &[OverlaySpec]) -> CliResult<()> {
    match command {
        Commands::Check(cmd) => run_check(Some(cmd.input), cmd.repair, &cmd.password, false, false),
        Commands::CheckLinearization(cmd) => {
            run_check_linearization(Some(cmd.input), false, &PasswordArgs::default(), false)
        }
        Commands::DumpObject(cmd) => run_dump_object(
            Some(cmd.input),
            cmd.repair,
            &cmd.password,
            &cmd.object_ref,
            false,
        ),
        Commands::Pages(cmd) => {
            if cmd.show_npages {
                run_show_npages(Some(cmd.input), cmd.repair, &cmd.password, false)
            } else {
                run_show_pages(Some(cmd.input), cmd.repair, &cmd.password, false, false)
            }
        }
        Commands::Qdf(cmd) => run_qdf(
            Some(cmd.input),
            Some(cmd.output),
            cmd.repair,
            &cmd.password,
            cmd.preserve_unreferenced,
        ),
        Commands::QdfFix(cmd) => run_qdf_fix(&cmd.input, &cmd.output),
        Commands::ZlibFlate(cmd) => {
            let whoami = progname();
            let usage_name = format!("{whoami} zlib-flate");
            run_zlib_flate(&cmd.modes, &whoami, &usage_name)
        }
        Commands::ShowStream(cmd) => run_show_stream(cmd),
        Commands::ShowEncryption(cmd) => {
            // The native subcommand has no `--show-encryption-key` flag of its
            // own (the dedicated `show-encryption-key` subcommand covers that
            // need); only the qpdf-argv-compatible top-level `--show-encryption`
            // flag combines with `--show-encryption-key`.
            run_show_encryption(&cmd.input, cmd.repair, &cmd.password, false, false)
        }
        Commands::IsEncrypted(cmd) => {
            let password = PasswordArgs {
                recovery: cmd.recovery,
                ..PasswordArgs::default()
            };
            run_is_encrypted(&cmd.input, cmd.repair, &password, false)
        }
        Commands::RequiresPassword(cmd) => {
            run_requires_password(&cmd.input, cmd.repair, &cmd.password, false)
        }
        Commands::ShowEncryptionKey(cmd) => {
            run_show_encryption_key(&cmd.input, cmd.repair, &cmd.password)
        }
        Commands::Rewrite(mut cmd) => {
            // qpdf keeps --verbose on QPDFJob, so the subcommand's copy of the
            // password arguments must carry the job policy to the reader's
            // retry boundary exactly like the top-level path does.
            cmd.password.verbose = cmd.verbose;
            validate_collate_values(&cmd.page_ops.collate);
            validate_keep_files_open_threshold(&cmd.page_ops)?;
            let version_options = match parse_cli_version_options(
                cmd.min_version.as_deref(),
                cmd.force_version.as_deref(),
            ) {
                Ok(options) => options,
                Err(error) => {
                    emit_logger_error(format!("flpdf: {error}\n"));
                    std::process::exit(1);
                }
            };
            let mut options = WriterOptions {
                static_id: cmd.static_id,
                deterministic_id: cmd.deterministic_id,
                static_aes_iv: cmd.static_aes_iv,
                no_original_object_ids: cmd.no_original_object_ids,
                preserve_unreferenced_objects: cmd.preserve_unreferenced,
                progress: cmd.progress,
                // `--qdf` and `--deterministic-id` configure the canonical writer's
                // output preparation directly.
                qdf: cmd.qdf,
                password_mode: cmd.password.password_mode.into(),
                object_streams: cmd.object_streams.into(),
                compress_streams: cmd.compress_streams.map(|mode| match mode {
                    CliYesNo::Yes => CompressStreams::Yes,
                    CliYesNo::No => CompressStreams::No,
                }),
                newline_before_endstream: cmd.newline_before_endstream.into(),
                // --stream-data overrides --compress-streams when set.
                stream_data: cmd.stream_data.map(Into::into),
                // Recompressing an existing lone /FlateDecode stream is a writer
                // setting and is applied by the same canonical route.
                recompress_flate: cmd.recompress_flate,
                compression_level: parse_compression_level(cmd.compression_level.as_deref())?,
                ..WriterOptions::default()
            };
            apply_cli_decode_level(&mut options, cmd.decode_level);
            apply_cli_version_options(&mut options, &version_options);
            // `rewrite --encrypt` / `--copy-encryption`: wire encryption
            // onto WriterOptions (shared with the top-level surface via
            // apply_encryption_options).
            apply_encryption_options(
                &mut options,
                cmd.raw_encrypt.as_deref(),
                cmd.copy_encryption.as_deref(),
                cmd.raw_encryption_file_password.as_deref(),
                &cmd.password,
                false,
            );
            let normalize_content = matches!(cmd.normalize_content, Some(CliYesNo::Yes));
            options.content_normalization = normalize_content;
            options.content_normalization_set = cmd.normalize_content.is_some();
            let coalesce_contents = cmd.coalesce_contents;
            let remove_unref = cmd.remove_unreferenced_resources;
            let image_options = image_optimization_options(
                cmd.keep_inline_images,
                cmd.oi_min_width.as_deref(),
                cmd.oi_min_height.as_deref(),
                cmd.oi_min_area.as_deref(),
                cmd.ii_min_bytes.as_deref(),
            )?;

            // Page-operation dispatch. When --pages is set
            // the extraction pipeline owns the write; otherwise --rotate /
            // --split-pages decorate a plain rewrite. Linearization is a
            // writer setting applied after the page-operation mutations, just
            // as qpdf applies `setWriterOptions` after `createQPDF`.
            if page_ops_active(&cmd.page_ops) {
                // The --rotate/--split-pages-only path does not run overlay
                // stacking; only --pages does (via run_page_extraction below).
                if cmd.page_ops.pages.is_empty() && !overlay_specs.is_empty() {
                    emit_logger_error(
                        "flpdf: --overlay/--underlay is not applied with \
                         --rotate/--split-pages alone (no --pages); \
                         rerun with --pages or without the overlay\n",
                    );
                    std::process::exit(1);
                }
                // The page-operation pipeline owns the write and does not run
                // the rewrite-only mutation passes. Silently dropping them
                // would make the command partially succeed; reject the
                // unsupported combinations loudly instead. Writer settings,
                // including content normalization, are applied by the final
                // PdfWriter and are therefore intentionally accepted here.
                //
                // --decrypt is rejected for the same reason: the page-ops
                // pipeline already rejects encrypted inputs (so a useful
                // --decrypt + page-ops combination is impossible), and on
                // plaintext input --decrypt is a silent no-op anyway —
                // rejecting upfront surfaces the unsupported combination
                // instead of leaving the user wondering whether decryption
                // happened.
                // --optimize-images is NOT in this list: unlike the other
                // rewrite-only mutation passes above, the page-operation
                // functions below (run_page_extraction /
                // run_rewrite_with_page_ops) already accept and apply it via
                // `cmd.optimize_images.then_some(image_options)`, mirroring
                // the top-level --pages/--rotate/--split-pages routes.
                if coalesce_contents
                    || cmd.remove_restrictions
                    || cmd.decrypt
                    || cmd.encrypt.is_some()
                    || cmd.copy_encryption.is_some()
                    || cmd.generate_appearances
                    || cmd.flatten_annotations.is_some()
                    || cmd.flatten_rotation
                {
                    emit_logger_error(
                        "flpdf: --coalesce-contents / --remove-restrictions / --decrypt / --encrypt / \
                         --copy-encryption / --flatten-annotations / \
                         --generate-appearances / --flatten-rotation are \
                         not applied in the --pages/--rotate/--split-pages/\
                         --collate pipeline; rerun without them or without \
                         the page operation\n",
                    );
                    std::process::exit(1);
                }
                // The decorate path (--rotate/--split-pages without --pages)
                // does not thread remove_unreferenced_resources; an explicit
                // Yes/No would be silently dropped, so reject it. Auto (the
                // default) is allowed: there is no extracted subset to prune.
                if cmd.page_ops.pages.is_empty()
                    && remove_unref != CliRemoveUnreferencedResources::Auto
                {
                    emit_logger_error(
                        "flpdf: --remove-unreferenced-resources is not applied \
                         with --rotate/--split-pages alone; rerun without it \
                         or add --pages\n",
                    );
                    std::process::exit(1);
                }
                return if cmd.page_ops.empty && !cmd.page_ops.pages.is_empty() {
                    // `PageOpArgs` is shared with the top-level surface, whose
                    // `--empty --pages` route dispatches here too (main
                    // dispatch, `args.page_ops.empty && ...`); without this
                    // arm `cmd.input` (unused for an empty primary) is
                    // rejected instead of routed, and the two surfaces
                    // silently diverge on the identical flag combination.
                    run_empty_page_extraction(
                        &cmd.output,
                        cmd.repair,
                        &cmd.password,
                        None,
                        &cmd.page_ops,
                        overlay_specs,
                        remove_unref,
                        options,
                        cmd.linearize,
                        None,
                        cmd.optimize_images.then_some(image_options),
                        cmd.verbose,
                        false,
                    )
                } else if !cmd.page_ops.pages.is_empty() {
                    run_page_extraction(
                        &cmd.input,
                        &cmd.output,
                        cmd.repair,
                        &cmd.password,
                        false,
                        None,
                        &cmd.page_ops,
                        overlay_specs,
                        remove_unref,
                        options,
                        cmd.linearize,
                        None,
                        cmd.optimize_images.then_some(image_options),
                        cmd.verbose,
                        false,
                    )
                } else {
                    run_rewrite_with_page_ops(
                        &cmd.input,
                        &cmd.output,
                        cmd.repair,
                        &cmd.password,
                        false,
                        None,
                        &cmd.page_ops,
                        remove_unref,
                        options,
                        cmd.linearize,
                        None,
                        cmd.optimize_images.then_some(image_options),
                        cmd.verbose,
                        false,
                    )
                };
            }

            run_rewrite(
                Some(cmd.input),
                Some(cmd.output),
                cmd.repair,
                &cmd.password,
                false,
                None,
                cmd.linearize,
                None,
                cmd.remove_restrictions,
                cmd.decrypt,
                normalize_content,
                coalesce_contents,
                remove_unref,
                cmd.generate_appearances,
                cmd.optimize_images.then_some(image_options),
                cmd.flatten_annotations,
                cmd.flatten_rotation,
                overlay_specs,
                cmd.verbose,
                false, // no_warn: the `rewrite` subcommand has no --no-warn flag
                options,
            )
        }
    }
}

fn run_check(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    no_warn: bool,
    show_encryption_key: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(&input).map_err(|error| open_error_with_file(&input, error.into()))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(no_warn);
    job.set_show_encryption_key(show_encryption_key);
    let mut options = pdf_open_options(repair, password)?;
    // The job emits the collected diagnostics once, after the qpdf check
    // banner, and owns the shared warning completion boundary.
    options.suppress_warnings = true;
    let mut pdf =
        match job.open_with_description(BufReader::new(file), path_description(&input), options) {
            Ok(pdf) => pdf,
            Err(error) => {
                job.report_open_failure(&error)?;
                return Err(error_with_file(&input, actionable_password_error(error)));
            }
        };
    finish_check_job(job.check(&mut pdf))
}

fn run_check_linearization(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    no_warn: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(&input).map_err(|error| open_error_with_file(&input, error.into()))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(no_warn);
    let options = pdf_open_options(repair, password)?;
    let mut pdf = job
        .open_with_description(BufReader::new(file), path_description(&input), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
    finish_job_exit_status(job.check_linearization(&mut pdf)?)
}

/// Wire `--encrypt` / `--copy-encryption` onto `options`, shared by the
/// top-level and `rewrite` surfaces so the two stay in lock-step. A `--encrypt`
/// parse error or a `--copy-encryption`
/// donor-open/validation error prints a `flpdf:`-prefixed diagnostic and exits
/// 2, matching the surrounding option parsers. The two options are mutually
/// exclusive at the CLI layer (clap `conflicts_with`), so at most one branch
/// fires.
fn apply_encryption_options<T: RawCliArg>(
    options: &mut WriterOptions,
    encrypt: Option<&[T]>,
    copy_encryption: Option<&std::path::Path>,
    encryption_file_password: Option<&[u8]>,
    password_args: &PasswordArgs,
    suppress_warnings: bool,
) {
    if let Some(encrypt) = encrypt {
        match parse_encrypt_segment(encrypt, password_args.allow_weak_crypto) {
            Ok(parsed) => {
                if parsed.accessibility_warning {
                    emit_logger_error(format!(
                        "{}: -accessibility=n is ignored for modern encryption formats\n",
                        progname()
                    ));
                }
                options.encrypt = Some(parsed.params);
            }
            Err(e) => {
                emit_logger_error(format!("flpdf: {e}\n"));
                std::process::exit(2);
            }
        }
    }
    if let Some(donor_path) = copy_encryption {
        match build_copy_encryption_source(
            donor_path,
            encryption_file_password,
            password_args,
            suppress_warnings,
        ) {
            Ok(src) => {
                options.copy_encryption = Some(src);
            }
            Err(e) => {
                emit_logger_error(format!("flpdf: {e}\n"));
                std::process::exit(2);
            }
        }
    }
}

/// Open a donor PDF at `path` (with optional `password`) and extract the
/// information needed to copy its encryption to a new output file
/// (`--copy-encryption`).
///
/// Returns a [`CopyEncryptionSource`] ready to be stored in
/// [`WriterOptions::copy_encryption`] or an error string suitable for printing
/// to stderr before `exit(2)`.
///
/// Only V=4 AES-128 donors are accepted.  Other encryption schemes are
/// rejected with a "not yet supported" message.
fn build_copy_encryption_source(
    path: &std::path::Path,
    password: Option<&[u8]>,
    password_args: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<CopyEncryptionSource> {
    let file =
        File::open(path).map_err(|e| format!("--copy-encryption: cannot open {:?}: {e}", path))?;
    let reader = BufReader::new(file);

    let mut donor_password = password_args.clone();
    donor_password.set_password_bytes(password.map(ToOwned::to_owned));
    donor_password.password_file = None;
    let opts = pdf_open_options(true, &donor_password)
        .map_err(|error| format!("--copy-encryption: failed to configure {:?}: {error}", path))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    let mut donor = job
        .open_with_description(reader, path_description(path), opts)
        .map_err(|e| format!("--copy-encryption: failed to open {:?}: {e}", path))?;
    donor
        .root_handle()
        .map_err(|e| format!("--copy-encryption: failed to open {:?}: {e}", path))?;

    // Validate the donor is encrypted using qpdf's individual encryption
    // projections rather than a crate-specific aggregate information object.
    let version = donor
        .encryption_version()
        .ok_or_else(|| format!("--copy-encryption: donor {:?} is not encrypted", path))?;
    let length_bits = donor.encryption_length_bits().ok_or_else(|| {
        format!(
            "--copy-encryption: donor {:?} has no encryption key length",
            path
        )
    })?;
    let (stream_method, string_method, _) = donor.encryption_methods().ok_or_else(|| {
        format!(
            "--copy-encryption: donor {:?} has no crypt-filter methods",
            path
        )
    })?;

    // Walking-skeleton scope: only V=4 AES-128 (StmF=AESV2 / StrF=AESV2).
    // The method accessors use qpdf's spelling "AESv2" (lowercase v).
    let is_v4_aes128 =
        version == 4 && length_bits == 128 && stream_method == "AESv2" && string_method == "AESv2";
    if !is_v4_aes128 {
        return Err(format!(
            "--copy-encryption: donor {:?} uses V={} length={} \
             stream={} string={} — only V=4 AES-128 donors are accepted",
            path, version, length_bits, stream_method, string_method,
        )
        .into());
    }

    // Recover the donor's file key.  The error message guides the user to
    // supply the correct password via --encryption-file-password.
    let file_key: Vec<u8> = donor.encryption_file_key().ok_or_else(|| {
        format!(
            "--copy-encryption: failed to recover donor file key for {:?} \
             (wrong --encryption-file-password?)",
            path
        )
    })?;

    // The reader owns the authenticated qpdf copy-encryption boundary. It
    // snapshots the live /Encrypt and /ID[0] handles without exposing the
    // legacy Object route to this external CLI crate.
    let mut source = donor
        .writer_copy_encryption_source()?
        .ok_or_else(|| format!("--copy-encryption: donor {:?} is not encrypted", path))?;
    source.file_key = file_key;
    source.object_key_alg = ObjectKeyAlg::Aes;
    Ok(source)
}

/// Parse the qpdf-shaped `--encrypt USER-PW OWNER-PW KEY-LEN [sub-flags]`
/// segment into an [`EncryptParams`].
///
/// `tokens` is the captured `Vec<String>` from clap's `value_terminator="--"`
/// + `num_args = 3..` segment; it does not include the trailing `--` itself
///   (clap consumes it as the terminator).
///
/// KEY-LEN → method (matching qpdf):
/// - `40` → V=1 R=2 RC4-40 (weak).
/// - `128` + `--use-aes=y` → V=4 R=4 AES-128.
/// - `128` + (`--use-aes=n` or omitted) + `--force-V4` → V=4 R=4 RC4-128 (weak).
/// - `128` + (`--use-aes=n` or omitted) without `--force-V4` → V=2 R=3 RC4-128
///   (qpdf's default, weak).
/// - `256` → V=5 R=6 AES-256 (`--allow-insecure` gates the empty-owner case).
///
/// RC4 outputs (40-bit, or 128-bit without AES) are weak and require
/// `allow_weak_crypto` (the top-level `--allow-weak-crypto` flag), mirroring
/// qpdf's write-side checkConfiguration.
///
/// Permission sub-flags (`--print`, `--modify`, `--extract`, `--annotate`,
/// `--form`, `--assemble`, `--accessibility`, and `--modify-other`) use the
/// key-length-specific qpdf option table and are applied left-to-right. R=2
/// uses its separate four-bit permission configuration; R>=3 uses
/// [`PermissionsConfig`]. `--cleartext-metadata` promotes 128-bit output to
/// V=4, while `--force-R5` selects the 256-bit R=5 path.
fn parse_perm_yn(flag: &str, val: &str) -> CliResult<bool> {
    match val {
        "y" => Ok(true),
        "n" => Ok(false),
        other => Err(format!("{flag} must be y or n (got {other:?})").into()),
    }
}
#[derive(Debug)]
struct ParsedEncryptSegment {
    params: EncryptParams,
    accessibility_warning: bool,
}

trait RawCliArg {
    fn raw_bytes(&self) -> Vec<u8>;
    fn os_string(&self) -> OsString;
}

impl RawCliArg for String {
    fn raw_bytes(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }

    fn os_string(&self) -> OsString {
        OsString::from(self)
    }
}

impl RawCliArg for OsString {
    fn raw_bytes(&self) -> Vec<u8> {
        arg_parser::os_bytes(self)
    }

    fn os_string(&self) -> OsString {
        self.clone()
    }
}

impl RawCliArg for Vec<u8> {
    fn raw_bytes(&self) -> Vec<u8> {
        self.clone()
    }

    fn os_string(&self) -> OsString {
        arg_parser::os_string_from_bytes(self)
    }
}

impl RawCliArg for arg_parser::RawArg {
    fn raw_bytes(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }

    fn os_string(&self) -> OsString {
        self.as_os_str().to_os_string()
    }
}

/// Return qpdf's active option-table name for an encryption segment.
///
/// qpdf selects the key-length-specific table as soon as it consumes the
/// third positional argument or `--bits`; `QPDFArgParser` includes that table
/// name in unknown-argument diagnostics (`QPDFArgParser.cc:496-502`).
fn encrypt_option_table_name(key_len: Option<u32>) -> &'static str {
    match key_len {
        Some(40) => "40-bit encryption",
        Some(128) => "128-bit encryption",
        Some(256) => "256-bit encryption",
        None | Some(_) => "encryption",
    }
}

fn unrecognized_encrypt_argument(token: &str, key_len: Option<u32>) -> String {
    format!(
        "unrecognized argument {token} ({} options must be terminated with --)",
        encrypt_option_table_name(key_len)
    )
}

fn parse_encrypt_segment<T: RawCliArg>(
    tokens: &[T],
    allow_weak_crypto: bool,
) -> CliResult<ParsedEncryptSegment> {
    if tokens.is_empty() {
        return Err("--encrypt requires USER-PW OWNER-PW KEY-LEN".into());
    }

    // qpdf starts in a password-argument table and switches to a
    // key-length-specific table after the third positional argument or the
    // named --bits argument. Keep the two password forms distinct so the
    // mixed-form error is raised at the same boundary as qpdf.
    let mut positional: Vec<Vec<u8>> = Vec::new();
    let mut dashed_mode = false;
    let mut positional_mode = false;
    let mut user_password = None;
    let mut owner_password = None;
    let mut key_len = None;
    let mut key_len_seen = false;
    let mut subflags: Vec<Vec<u8>> = Vec::new();

    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let token_bytes = token.raw_bytes();
        let token_text = String::from_utf8_lossy(&token_bytes);
        let equal = token_bytes.iter().position(|byte| *byte == b'=');
        let (raw_name, attached) = match equal {
            Some(position) => (&token_bytes[..position], Some(&token_bytes[position + 1..])),
            None => (token_bytes.as_slice(), None),
        };
        let name = raw_name
            .strip_prefix(b"--")
            .or_else(|| raw_name.strip_prefix(b"-"))
            .and_then(|name| std::str::from_utf8(name).ok())
            .unwrap_or("");
        if matches!(name, "user-password" | "owner-password" | "bits") {
            if positional_mode {
                return Err("positional and dashed encryption arguments may not be mixed".into());
            }
            if key_len_seen {
                return Err(unrecognized_encrypt_argument(&token_text, key_len).into());
            }
            dashed_mode = true;
            let value = if let Some(value) = attached {
                value.to_vec()
            } else {
                index += 1;
                tokens
                    .get(index)
                    .map(RawCliArg::raw_bytes)
                    .ok_or_else(|| format!("{token_text} requires a value"))?
            };
            match name {
                "user-password" => user_password = Some(value),
                "owner-password" => owner_password = Some(value),
                "bits" => {
                    key_len = Some(parse_encrypt_key_len(&String::from_utf8_lossy(&value))?);
                    key_len_seen = true;
                }
                _ => unreachable!("name was matched above"),
            }
            index += 1;
            continue;
        }

        if dashed_mode {
            if !token_bytes.starts_with(b"-") || token_bytes == b"-" {
                return Err("positional and dashed encryption arguments may not be mixed".into());
            }
            // qpdf's password-argument table has no key-specific options;
            // `--bits` must select the key-length-specific table before any
            // other named option is recognized.
            if !key_len_seen {
                return Err(unrecognized_encrypt_argument(&token_text, key_len).into());
            }
            subflags.push(token_bytes);
        } else if positional.len() < 3 {
            if token_bytes.starts_with(b"-") && token_bytes != b"-" {
                return Err(unrecognized_encrypt_argument(&token_text, None).into());
            }
            positional_mode = true;
            positional.push(token_bytes.clone());
            if positional.len() == 3 {
                key_len = Some(parse_encrypt_key_len(&token_text)?);
            }
        } else {
            if !token_bytes.starts_with(b"-") || token_bytes == b"-" {
                return Err(unrecognized_encrypt_argument(&token_text, key_len).into());
            }
            subflags.push(token_bytes);
        }
        index += 1;
    }

    let (user_password, owner_password, key_len) = if dashed_mode {
        if key_len.is_none() && !subflags.is_empty() {
            return Err("--encrypt key length is required before encryption options".into());
        }
        (
            user_password.unwrap_or_default(),
            owner_password.unwrap_or_default(),
            key_len.ok_or("--encrypt key length is required")?,
        )
    } else {
        if positional.len() < 3 {
            return Err(format!(
                "--encrypt requires USER-PW OWNER-PW KEY-LEN (got {} arg(s))",
                positional.len()
            )
            .into());
        }
        let key_len = key_len.ok_or("--encrypt key length is required")?;
        (positional[0].clone(), positional[1].clone(), key_len)
    };

    let mut use_aes = None;
    let mut force_v4 = false;
    let mut force_r5 = false;
    let mut allow_insecure = false;
    let mut perms = PermissionsConfig::default();
    let mut r2_permissions = R2PermissionsConfig::default();
    let mut cleartext_metadata = false;
    let mut accessibility_explicitly_disabled = false;

    for token in &subflags {
        let token_text = String::from_utf8_lossy(token);
        let equal = token.iter().position(|byte| *byte == b'=');
        let (raw_flag, value_bytes) = match equal {
            Some(position) => (&token[..position], &token[position + 1..]),
            None => (token.as_slice(), b"".as_slice()),
        };
        let flag = raw_flag
            .strip_prefix(b"--")
            .or_else(|| raw_flag.strip_prefix(b"-"))
            .and_then(|flag| std::str::from_utf8(flag).ok())
            .unwrap_or("");
        let value = match std::str::from_utf8(value_bytes) {
            Ok(value) => value,
            Err(_) => return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into()),
        };
        match flag {
            "use-aes" => {
                if key_len != 128 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                use_aes = Some(parse_perm_yn(flag, value)?);
            }
            "force-V4" => {
                if key_len != 128 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                force_v4 = true;
            }
            "force-R5" => {
                if key_len != 256 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                force_r5 = true;
            }
            "allow-insecure" => {
                if key_len != 256 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                allow_insecure = true;
            }
            "cleartext-metadata" => {
                if !matches!(key_len, 128 | 256) {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                cleartext_metadata = true;
            }
            "print" => {
                if key_len == 40 {
                    r2_permissions.print = parse_perm_yn(flag, value)?;
                } else {
                    perms.print = match value {
                        "full" => PrintPermission::High,
                        "low" => PrintPermission::Low,
                        "none" => PrintPermission::None,
                        other => {
                            return Err(format!(
                                "--print must be full, low, or none (got {other:?})"
                            )
                            .into());
                        }
                    };
                }
            }
            "modify" => {
                if key_len == 40 {
                    r2_permissions.modify = parse_perm_yn(flag, value)?;
                } else {
                    let (other, annotate, forms, assemble) = match value {
                        "all" => (true, true, true, true),
                        "annotate" => (false, true, true, true),
                        "form" => (false, false, true, true),
                        "assembly" => (false, false, false, true),
                        "none" => (false, false, false, false),
                        other => {
                            return Err(format!(
                                "--modify must be all, annotate, form, assembly, or none (got {other:?})"
                            )
                            .into());
                        }
                    };
                    perms.modify_contents = other;
                    perms.annotate = annotate;
                    perms.fill_forms = forms;
                    perms.assemble = assemble;
                }
            }
            "extract" => {
                let value = parse_perm_yn(flag, value)?;
                if key_len == 40 {
                    r2_permissions.extract = value;
                } else {
                    perms.extract = value;
                }
            }
            "annotate" => {
                let value = parse_perm_yn(flag, value)?;
                if key_len == 40 {
                    r2_permissions.annotate = value;
                } else {
                    perms.annotate = value;
                }
            }
            "form" => {
                if key_len == 40 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                perms.fill_forms = parse_perm_yn(flag, value)?;
            }
            "assemble" => {
                if key_len == 40 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                perms.assemble = parse_perm_yn(flag, value)?;
            }
            "accessibility" => {
                if key_len == 40 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                perms.accessibility = parse_perm_yn(flag, value)?;
                accessibility_explicitly_disabled = value == "n";
            }
            "modify-other" => {
                if key_len == 40 {
                    return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
                }
                perms.modify_contents = parse_perm_yn(flag, value)?;
            }
            _other => {
                return Err(unrecognized_encrypt_argument(&token_text, Some(key_len)).into());
            }
        }
    }

    if key_len == 40 && (force_r5 || allow_insecure || cleartext_metadata) {
        return Err("--encrypt KEY-LEN=40 does not accept this encryption option".into());
    }
    if key_len == 128 && (force_r5 || allow_insecure) {
        return Err("--encrypt KEY-LEN=128 does not accept this encryption option".into());
    }
    if key_len == 256 && (force_v4 || use_aes.is_some()) {
        return Err("--encrypt KEY-LEN=256 does not accept --force-V4 or --use-aes".into());
    }

    let guard_weak = |params: EncryptParams| -> CliResult<EncryptParams> {
        if !allow_weak_crypto && params.is_weak_rc4() {
            return Err(
                "refusing to write a file with RC4, a weak cryptographic algorithm. \
                 Please use 256-bit keys for better security. Pass --allow-weak-crypto \
                 to enable writing insecure files."
                    .into(),
            );
        }
        Ok(params)
    };

    let method = match key_len {
        40 => EncryptMethod::V1Rc440,
        128 if force_v4 || cleartext_metadata => {
            if use_aes.unwrap_or(false) {
                EncryptMethod::V4Aes128
            } else {
                EncryptMethod::V4Rc4128
            }
        }
        128 if use_aes == Some(true) => EncryptMethod::V4Aes128,
        128 => EncryptMethod::V2Rc4128,
        256 if force_r5 => EncryptMethod::V5R5Aes256,
        256 => EncryptMethod::V5R6Aes256,
        _ => unreachable!("key length was validated"),
    };

    if cleartext_metadata
        && !matches!(
            method,
            EncryptMethod::V4Aes128
                | EncryptMethod::V4Rc4128
                | EncryptMethod::V5R5Aes256
                | EncryptMethod::V5R6Aes256
        )
    {
        return Err("--cleartext-metadata requires V=4 or V=5".into());
    }

    let params = match method {
        EncryptMethod::V1Rc440 => {
            let mut params = EncryptParams::rc4(method, user_password, owner_password);
            params.r2_permissions = r2_permissions;
            params
        }
        EncryptMethod::V2Rc4128 => {
            let mut params = EncryptParams::rc4(method, user_password, owner_password);
            params.permissions = perms;
            params
        }
        EncryptMethod::V4Rc4128 | EncryptMethod::V4Aes128 => {
            let mut params = if method == EncryptMethod::V4Aes128 {
                EncryptParams::v4_aes128(user_password, owner_password)
            } else {
                EncryptParams::rc4(method, user_password, owner_password)
            };
            params.permissions = perms;
            params.permissions.accessibility = true;
            params.encrypt_metadata = !cleartext_metadata;
            params
        }
        EncryptMethod::V5R5Aes256 | EncryptMethod::V5R6Aes256 => {
            if owner_password.is_empty() && !user_password.is_empty() && !allow_insecure {
                return Err(
                    "A PDF with a non-empty user password and an empty owner password \
                     encrypted with a 256-bit key is insecure as it can be opened without \
                     a password. If you really want to do this, you must also give the \
                     --allow-insecure option before the -- that follows --encrypt."
                        .into(),
                );
            }
            let mut params = if method == EncryptMethod::V5R5Aes256 {
                EncryptParams::v5_r5(user_password, owner_password)
            } else {
                EncryptParams::v5_r6(user_password, owner_password)
            };
            params.permissions = perms;
            params.permissions.accessibility = true;
            params.encrypt_metadata = !cleartext_metadata;
            params
        }
    };

    Ok(ParsedEncryptSegment {
        params: guard_weak(params)?,
        accessibility_warning: accessibility_explicitly_disabled
            && matches!(
                method,
                EncryptMethod::V4Rc4128
                    | EncryptMethod::V4Aes128
                    | EncryptMethod::V5R5Aes256
                    | EncryptMethod::V5R6Aes256
            ),
    })
}

fn parse_encrypt_key_len(value: &str) -> CliResult<u32> {
    let key_len = value.parse().map_err(|_| {
        format!("--encrypt KEY-LEN must be a positive integer (40 / 128 / 256), got: {value:?}")
    })?;
    if !matches!(key_len, 40 | 128 | 256) {
        return Err(format!("--encrypt KEY-LEN must be 40, 128, or 256 (got {key_len})").into());
    }
    Ok(key_len)
}

#[allow(clippy::too_many_arguments)]
fn run_rewrite(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    json_input: bool,
    update_from_json: Option<&Path>,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    remove_restrictions: bool,
    decrypt: bool,
    normalize_content: bool,
    coalesce_contents: bool,
    _remove_unref: CliRemoveUnreferencedResources,
    generate_appearances: bool,
    image_options: Option<ImageOptimizationOptions>,
    flatten_annotations_mode: Option<CliFlattenMode>,
    flatten_rotation: bool,
    overlay_specs: &[OverlaySpec],
    verbose: bool,
    no_warn: bool,
    options: WriterOptions,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    reject_same_job_output(&input, &output)?;
    let opened = open_job_pdf(
        &input,
        repair,
        password,
        json_input,
        update_from_json,
        false,
        no_warn,
    )?;
    match opened {
        JobPdf::File(pdf) => run_rewrite_opened(
            pdf,
            &input,
            &output,
            repair,
            password,
            linearize,
            linearize_pass1,
            remove_restrictions,
            decrypt,
            normalize_content,
            coalesce_contents,
            _remove_unref,
            generate_appearances,
            image_options,
            flatten_annotations_mode,
            flatten_rotation,
            overlay_specs,
            verbose,
            no_warn,
            options,
        ),
        JobPdf::Json(pdf) => run_rewrite_opened(
            pdf,
            &input,
            &output,
            repair,
            password,
            linearize,
            linearize_pass1,
            remove_restrictions,
            decrypt,
            normalize_content,
            coalesce_contents,
            _remove_unref,
            generate_appearances,
            image_options,
            flatten_annotations_mode,
            flatten_rotation,
            overlay_specs,
            verbose,
            no_warn,
            options,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_rewrite_opened<R: Read + Seek + 'static>(
    mut pdf: Pdf<R>,
    input: &Path,
    output: &Path,
    repair: bool,
    password: &PasswordArgs,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    remove_restrictions: bool,
    decrypt: bool,
    normalize_content: bool,
    coalesce_contents: bool,
    _remove_unref: CliRemoveUnreferencedResources,
    generate_appearances: bool,
    image_options: Option<ImageOptimizationOptions>,
    flatten_annotations_mode: Option<CliFlattenMode>,
    flatten_rotation: bool,
    overlay_specs: &[OverlaySpec],
    verbose: bool,
    no_warn: bool,
    options: WriterOptions,
) -> CliResult<()> {
    // qpdf's `--no-warn` suppresses warning delivery for the entire job,
    // including warnings raised by transformations applied after the
    // document opens (e.g. --flatten-annotations's /NeedAppearances
    // warning), not only open-time diagnostics. Without this, a warning
    // raised mid-rewrite would still print live despite --no-warn.
    pdf.set_suppress_warnings(no_warn);
    let mut standard_output = prepare_pdf_standard_output(output)?;

    // Overlay/underlay stacking mutates page dictionaries and adds objects
    // before the canonical writer plans the output. The linearized path has a
    // separate qpdf ordering contract, so the combination is rejected upfront.
    if linearize && !overlay_specs.is_empty() {
        return Err("--overlay/--underlay cannot be combined with --linearize".into());
    }

    if linearize {
        // --remove-restrictions must strip signatures before the linearization
        // plan is computed: removing signature objects changes the reachable
        // first-page graph. qpdf applies this transformation before planning.
        if remove_restrictions {
            let _ = AcroFormDocumentHelper::new(&mut pdf)?.disable_digital_signatures()?;
        }
        let mut options = options;
        if decrypt {
            options.preserve_encryption = false;
        }
        if let Some(image_options) = image_options {
            flpdf::optimize_images(&mut pdf, &cli_logger(), &progname(), verbose, image_options)?;
        }
        if generate_appearances {
            generate_missing_appearances(&mut pdf)?;
        }
        if let Some(mode) = flatten_annotations_mode {
            let (required_flags, forbidden_flags) = mode.flags();
            PageDocumentHelper::new(&mut pdf)
                .flatten_annotations(required_flags, forbidden_flags)?;
        }
        // qpdf applies --flatten-rotation after annotation flattening and
        // before the writer plans the linearized output
        // (`QPDFJob.cc:2183-2194`). Keep the transformed page graph visible
        // to the linearization planner rather than silently dropping the
        // option on this branch.
        if flatten_rotation {
            let page_refs = pages::page_refs(&mut pdf)?;
            flatten_rotation_on_pages(&mut pdf, &page_refs)?;
        }
        // Apply content normalization before the writer plans and emits the
        // linearized document.
        let normalization_last_bad = if normalize_content {
            normalize_page_contents(&mut pdf)?
        } else {
            Vec::new()
        };
        let announce_file = standard_output.is_none();
        write_with_pdf_writer(
            &mut pdf,
            output,
            &mut standard_output,
            &options,
            true,
            linearize_pass1,
        )?;
        if verbose && announce_file {
            logger_info(wrote_file_message(&progname(), output))?;
        }
        // On an encrypted input, `--decrypt` has already disabled
        // source-encryption preservation above.
        finish_rewrite_warnings(input, &pdf, &normalization_last_bad, announce_file, no_warn)?;
    } else {
        // qpdf runs disableDigitalSignatures unconditionally under
        // --remove-restrictions: remove catalog /Perms, zero /AcroForm
        // /SigFlags, strip /FT /V /SV /Lock from /Sig form fields, and erase them
        // from the top-level /Fields array (a field still reachable from a page
        // /Annots survives as a plain annotation; orphaned signature dicts are
        // dropped by the canonical rewrite GC). The qpdf mutation itself is
        // silent; normal document warnings continue through completion.
        if remove_restrictions {
            let _ = AcroFormDocumentHelper::new(&mut pdf)?.disable_digital_signatures()?;
        }
        let mut options = options;
        if decrypt {
            options.preserve_encryption = false;
        }
        if let Some(image_options) = image_options {
            flpdf::optimize_images(&mut pdf, &cli_logger(), &progname(), verbose, image_options)?;
        }
        // ── Content mutation pass ─────────────────────────────────────────────
        //
        // The mutations below operate on the in-memory Pdf model (via set_object).
        // They are all visible in the canonical writer output.
        //
        // Application order follows QPDFJob::handleTransformations:
        //   1. generate appearances;
        //   2. flatten annotations;
        //   3. coalesce page contents;
        //   4. flatten rotation and apply page stacking;
        //   5. normalize content immediately before the writer consumes it.
        // The coalesce operation is the provider-backed PageObjectHelper
        // route; it must not materialize a legacy page byte buffer.
        //
        // NOTE: a plain `rewrite` does NOT prune unreferenced /Resources entries.
        // qpdf only prunes resource-dict entries during page-copy operations
        // (`--pages`/`--split-pages`) — a plain `qpdf IN OUT`, even with
        // `--remove-unreferenced-resources=yes`, keeps every /Resources entry
        // (verified against qpdf 11.9.0). flpdf mirrors this: resource-entry
        // pruning lives in `run_page_extraction` (the --pages path), not here.
        // Pruning on a plain rewrite would incorrectly drop an unreferenced
        // image XObject. Resource-entry pruning is distinct from unreferenced-
        // object GC: renumbering drops unreachable objects on every canonical
        // rewrite, while /Resources-entry pruning is limited to page operations.
        //
        // qpdf always creates a fresh document and defaults to
        // `--compress-streams=y`; the canonical writer applies those defaults
        // for every rewrite. Version setters therefore always affect the
        // emitted header, including with `--remove-unreferenced-resources=no`.
        // (No resource-entry pruning on the plain rewrite path — see the
        // "Content mutation pass" note above. qpdf prunes /Resources entries only
        // during page operations, which flpdf handles in run_page_extraction.)

        // Step 1: generate missing form-field appearance streams
        // (--generate-appearances). MUST run before --flatten-annotations so
        // value-only fields (e.g. a filled text field with no /AP) are baked
        // into page content instead of being dropped (acceptance ordering:
        // generate first, flatten second).
        if generate_appearances {
            generate_missing_appearances(&mut pdf)?;
        }

        // Step 2: flatten annotations into page content (--flatten-annotations).
        if let Some(mode) = flatten_annotations_mode {
            let (required_flags, forbidden_flags) = mode.flags();
            PageDocumentHelper::new(&mut pdf)
                .flatten_annotations(required_flags, forbidden_flags)?;
        }

        // Step 3: coalesce per-page /Contents arrays into provider-backed
        // streams. This intentionally follows annotation flattening, matching
        // QPDFJob.cc:2183-2187 for the combined flags.
        if coalesce_contents {
            let page_refs = pages::page_refs(&mut pdf)?;
            for page_ref in page_refs {
                PageObjectHelper::new(page_ref, &mut pdf).coalesce_content_streams()?;
            }
        }

        // Step 4: flatten page rotation into content (--flatten-rotation).
        if flatten_rotation {
            let page_refs = pages::page_refs(&mut pdf)?;
            flatten_rotation_on_pages(&mut pdf, &page_refs)?;
        }

        // Step 5: overlay/underlay page stacking (--overlay / --underlay).
        // qpdf applies this as its page-stacking step, after page selection and
        // the other content transforms and before writing; mirror that ordering
        // so the output graph (and thus the bytes) matches qpdf. Each source is
        // opened (with its --password) and imported into the in-memory document
        // here; the new objects are part of the canonical writer graph.
        // qpdf keeps a provider-backed source QPDF alive when
        // `copyForeignObject` copies a Form XObject whose data comes from a
        // `StreamDataProvider` (`libqpdf/QPDF.cc:2248-2257`). The canonical
        // overlay route has the same lifetime contract: retain the opened
        // source documents until after the destination writer has consumed
        // every copied Form stream, not just until page stacking returns.
        let _built_overlay_specs = if !overlay_specs.is_empty() {
            let mut built =
                build_overlay_specs_with_suppression(overlay_specs, repair, password, no_warn)?;

            // Propagate qpdf's max input version and Adobe
            // extension level to the writer (QPDFJob.cc:1714 and :2913),
            // while leaving the explicit raw --min-version for the writer's
            // later setter.
            update_input_version_floor(&mut options.input_version_floor, &mut pdf)?;
            for spec in built.iter_mut() {
                update_input_version_floor(&mut options.input_version_floor, &mut spec.source)?;
            }

            // --verbose: emit the per-destination-page overlay/underlay plan
            // to stderr before painting, matching qpdf's --verbose output
            // ("processing underlay/overlay" header + `  page N` +
            // `    <file> overlay|underlay <src>`). The report is computed
            // via the flpdf::overlay_verbose_report inspection API so the
            // ordering (underlays first, then overlays, in declaration
            // order across specs) is source-shared with apply_overlay_specs.
            if verbose {
                let report = flpdf::overlay_verbose_report(&mut pdf, &mut built)?;
                logger_info(overlay_verbose_message(&report, overlay_specs))?;
            }

            flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
            Some(built)
        } else {
            None
        };

        // Step 6: normalize after all page transformations. The stream
        // normalizer consumes the provider-backed coalesced route and writes
        // the normalized bytes through ObjectHandle.
        let normalization_last_bad = if normalize_content {
            normalize_page_contents(&mut pdf)?
        } else {
            Vec::new()
        };

        let announce_file = standard_output.is_none();
        write_with_pdf_writer(
            &mut pdf,
            output,
            &mut standard_output,
            &options,
            false,
            None,
        )?;

        if verbose && announce_file {
            logger_info(wrote_file_message(&progname(), output))?;
        }
        // Unencrypted input + --remove-restrictions is a no-op rewrite
        // (exit 0, valid output, no diagnostic) — nothing was restricted,
        // matching qpdf's lenient handling of --remove-restrictions on
        // unencrypted files.
        finish_rewrite_warnings(input, &pdf, &normalization_last_bad, announce_file, no_warn)?;
    }
    Ok(())
}

/// Route `--generate-appearances` through qpdf's
/// `QPDFAcroFormDocumentHelper::generateAppearancesIfNeeded` boundary.
fn generate_missing_appearances<R: Read + Seek>(pdf: &mut Pdf<R>) -> CliResult<()> {
    AcroFormDocumentHelper::new(pdf)?.generate_appearances_if_needed()?;
    Ok(())
}

// ===========================================================================
// Page operations: --pages / --rotate / --split-pages /
// --collate plumbing.
//
// qpdf observation basis (/usr/bin/qpdf 11.9.0):
//   - `qpdf --help=page-selection` documents the
//     `--pages [--file=]f [--password=p] [range] [...] -- out` segment, the
//     `.` shorthand for the primary input, and `--collate=n`.
//   - `qpdf in.pdf --pages . 2-3 -- --rotate=+90:1 out.pdf` rotates the FIRST
//     EXTRACTED page (verified: source page 2's object got /Rotate 90, source
//     page 3 stayed /Rotate 0) — so --rotate ranges index OUTPUT page numbers.
//   - `qpdf --split-pages=2 in.pdf out.pdf` writes `out-1-2.pdf`,`out-3-3.pdf`.
//   - `--collate` / `--rotate` / `--split-pages` without `--pages` exit 0 in
//     qpdf; flpdf matches (rotate applies to the source doc, collate no-op,
//     split operates on the rewritten bytes).
// ===========================================================================

/// One parsed entry from the `--pages` segment before file resolution.
struct PageSegmentSpec {
    /// File token as written (`.` = primary input, or a path).
    file_token: OsString,
    /// Per-input password (`--password=` immediately following the file).
    password: Option<OsString>,
    /// Raw per-input password bytes, retained when the OS projection is lossy.
    raw_password: Option<Vec<u8>>,
    /// Page-range string (empty = all pages).
    range: String,
}

/// Parse the raw `--pages` segment tokens into ordered specs.
///
/// Grammar (qpdf 11.9.0 `--help=page-selection`, both the modern
/// `--file=`/`--range=` form and the legacy positional form):
///
/// ```text
/// segment ::= ( file [ '--password=' pw ] [ range ] )+
/// file    ::= '--file=' PATH | PATH | '.'
/// range   ::= '--range=' R | R          (R = qpdf page-range syntax)
/// ```
///
/// Bounded, non-recursive single pass over `tokens`; no panics.
fn parse_pages_segment<T: RawCliArg>(tokens: &[T]) -> CliResult<Vec<PageSegmentSpec>> {
    let mut specs: Vec<PageSegmentSpec> = Vec::new();

    for tok in tokens {
        let token_bytes = tok.raw_bytes();
        if let Some(path) = token_bytes.strip_prefix(b"--file=") {
            specs.push(PageSegmentSpec {
                file_token: arg_parser::os_string_from_bytes(path),
                password: None,
                raw_password: None,
                range: String::new(),
            });
            continue;
        }
        if let Some(pw) = token_bytes.strip_prefix(b"--password=") {
            let cur = specs
                .last_mut()
                .ok_or("--pages: --password= must follow a file in the --pages segment")?;
            cur.password = Some(arg_parser::os_string_from_bytes(pw));
            cur.raw_password = Some(pw.to_vec());
            continue;
        }
        if let Some(r) = token_bytes.strip_prefix(b"--range=") {
            let cur = specs
                .last_mut()
                .ok_or("--pages: --range= must follow a file in the --pages segment")?;
            if !cur.range.is_empty() {
                return Err("--pages: duplicate page-range for one input file".into());
            }
            cur.range =
                String::from_utf8(r.to_vec()).map_err(|_| "--pages --range must be valid UTF-8")?;
            continue;
        }
        if token_bytes.starts_with(b"--") {
            return Err(format!(
                "--pages: unsupported token {:?} in the page-selection segment",
                tok.os_string()
            )
            .into());
        }
        // Positional token: either a NEW file, or the page-range for the
        // current file. qpdf's heuristic: the token is a page-range iff a
        // file is already open and that file has no range yet AND the token
        // parses as a page-range. Otherwise it starts a new file.
        let range = std::str::from_utf8(&token_bytes)
            .ok()
            .filter(|range| PageRange::parse(range).is_ok())
            .map(str::to_owned);
        match (specs.last_mut(), range) {
            (Some(cur), Some(range)) if cur.range.is_empty() => {
                cur.range = range;
            }
            _ => specs.push(PageSegmentSpec {
                file_token: tok.os_string(),
                password: None,
                raw_password: None,
                range: String::new(),
            }),
        }
    }

    if specs.is_empty() {
        return Err("--pages: no input files given in the page-selection segment".into());
    }
    Ok(specs)
}

fn raw_page_tokens(page_ops: &PageOpArgs) -> Vec<Vec<u8>> {
    page_ops
        .raw_pages
        .clone()
        .unwrap_or_else(|| raw_os_args(&page_ops.pages))
}

/// Resolve `--pages` specs into [`InputSpec`]s, mapping the `.` shorthand to
/// the primary input path while preserving the literal filename identity used
/// by qpdf's page-spec source heap.
fn resolve_page_specs(
    specs: &[PageSegmentSpec],
    primary_input: &std::path::Path,
) -> CliResult<Vec<InputSpec>> {
    let mut out = Vec::with_capacity(specs.len());
    for s in specs {
        let path: PathBuf = if s.file_token == OsStr::new(".") {
            primary_input.to_path_buf()
        } else {
            PathBuf::from(&s.file_token)
        };
        let range = PageRange::parse(&s.range).map_err(|e| {
            Box::<dyn std::error::Error>::from(format!(
                "--pages: invalid page range {:?}: {e}",
                s.range
            ))
        })?;
        out.push(InputSpec::new(
            path,
            s.raw_password.clone().or_else(|| {
                s.password
                    .as_ref()
                    .map(|password| arg_parser::os_bytes(password))
            }),
            range,
        ));
    }
    Ok(out)
}

// ===========================================================================
// --overlay / --underlay segment parser
//
// qpdf 11.9.0 grammar (--help=overlay-underlay):
//   {--overlay|--underlay} [--file=]FILE [--password=PW]
//       [--to=RANGE] [--from=RANGE] [--repeat=RANGE] --
//
// qpdf defaults (observed, NOT encoded here — deferral to .16.4):
//   --from=1-z, --to=1-z, --repeat="" (no repeat; surplus dest pages blank).
//   The task instruction note "default --repeat=z" contradicts qpdf observation
//   and is intentionally NOT adopted. Unspecified options stay None here; the
//   default interpretation is applied in .16.4.
// ===========================================================================

/// Whether a segment introduces overlay content (drawn on top) or underlay (below).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayKind {
    Overlay,
    Underlay,
}

/// Parsed result of a single `--overlay … --` or `--underlay … --` segment.
///
/// Range strings (`from`, `to`, `repeat`) are raw qpdf page-range syntax;
/// `None` means the option was absent. Default range semantics are applied
/// by the caller during page-mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlaySpec {
    kind: OverlayKind,
    /// Path to the overlay/underlay source PDF.
    file: OsString,
    /// Password for the source PDF, if supplied via `--password=`.
    password: Option<OsString>,
    /// Raw password bytes, retained when the OS projection is lossy.
    raw_password: Option<Vec<u8>>,
    /// Raw `--from=` page-range string (source pages to cycle through).
    from: Option<String>,
    /// Raw `--to=` page-range string (destination pages to receive content).
    to: Option<String>,
    /// Raw `--repeat=` page-range string (source pages to repeat for surplus dest).
    repeat: Option<String>,
}

struct PreprocessedArgs {
    residual_args: Vec<OsString>,
    overlay_specs: Vec<OverlaySpec>,
    attachment_segments: Vec<Vec<Vec<u8>>>,
    raw_overrides: RawCliOverrides,
}

#[derive(Debug, Default)]
struct RawCliOverrides {
    password: Option<Vec<u8>>,
    encryption_file_password: Option<Vec<u8>>,
    raw_encrypt: Option<Vec<Vec<u8>>>,
    raw_pages: Option<Vec<Vec<u8>>>,
    raw_copy_attachments_from: Option<Vec<Vec<Vec<u8>>>>,
}

/// Parse the raw token slice captured between `--overlay`/`--underlay` and `--`.
///
/// Grammar: `FILE and sub-options in any order`. `FILE` is either bare or `--file=PATH`;
/// `--password=`, `--to=`, `--from=`, `--repeat=` may appear before or after it,
/// mirroring qpdf's UO segment parser (no positional ordering constraint).
///
/// - `FILE` is mandatory (exactly one, either via `--file=PATH` or bare).
/// - `--password=`, `--to=`, `--from=`, `--repeat=` are each optional; duplicates error.
/// - Range values are validated via [`PageRange::parse`] (syntax only; defaults not applied).
/// - Unknown `--xxx` tokens, duplicate files, or an empty token list all produce an error.
///
/// # Errors
///
/// Returns an error if the token slice is empty, a file is missing or duplicated,
/// a range is syntactically invalid, a flag is duplicated, or an unknown `--` flag appears.
fn parse_overlay_segment<T: RawCliArg>(kind: OverlayKind, tokens: &[T]) -> CliResult<OverlaySpec> {
    let flag = match kind {
        OverlayKind::Overlay => "--overlay",
        OverlayKind::Underlay => "--underlay",
    };

    if tokens.is_empty() {
        return Err(format!("{flag}: no source file given in the segment").into());
    }

    let mut file: Option<OsString> = None;
    let mut password: Option<OsString> = None;
    let mut raw_password: Option<Vec<u8>> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut repeat: Option<String> = None;

    for tok in tokens {
        let token_bytes = tok.raw_bytes();
        if let Some(path) = token_bytes.strip_prefix(b"--file=") {
            if file.is_some() {
                return Err(format!("{flag}: duplicate file in segment").into());
            }
            file = Some(arg_parser::os_string_from_bytes(path));
            continue;
        }
        if let Some(pw) = token_bytes.strip_prefix(b"--password=") {
            if password.is_some() {
                return Err(format!("{flag}: duplicate --password= in segment").into());
            }
            password = Some(arg_parser::os_string_from_bytes(pw));
            raw_password = Some(pw.to_vec());
            continue;
        }
        if let Some(r) = token_bytes.strip_prefix(b"--to=") {
            if to.is_some() {
                return Err(format!("{flag}: duplicate --to= in segment").into());
            }
            let r =
                String::from_utf8(r.to_vec()).map_err(|_| "overlay --to must be valid UTF-8")?;
            PageRange::parse(&r)
                .map_err(|e| format!("{flag}: invalid --to= page range {r:?}: {e}"))?;
            to = Some(r);
            continue;
        }
        if let Some(r) = token_bytes.strip_prefix(b"--from=") {
            if from.is_some() {
                return Err(format!("{flag}: duplicate --from= in segment").into());
            }
            let r =
                String::from_utf8(r.to_vec()).map_err(|_| "overlay --from must be valid UTF-8")?;
            PageRange::parse(&r)
                .map_err(|e| format!("{flag}: invalid --from= page range {r:?}: {e}"))?;
            from = Some(r);
            continue;
        }
        if let Some(r) = token_bytes.strip_prefix(b"--repeat=") {
            if repeat.is_some() {
                return Err(format!("{flag}: duplicate --repeat= in segment").into());
            }
            let r = String::from_utf8(r.to_vec())
                .map_err(|_| "overlay --repeat must be valid UTF-8")?;
            PageRange::parse(&r)
                .map_err(|e| format!("{flag}: invalid --repeat= page range {r:?}: {e}"))?;
            repeat = Some(r);
            continue;
        }
        if token_bytes.starts_with(b"--") {
            return Err(
                format!("{flag}: unsupported token {:?} in segment", tok.os_string()).into(),
            );
        }
        // Bare (non-flag) token: must be the file path (exactly one allowed).
        if file.is_some() {
            return Err(format!("{flag}: duplicate file in segment").into());
        }
        file = Some(tok.os_string());
    }

    let file = file.ok_or_else(|| format!("{flag}: no source file given in the segment"))?;

    Ok(OverlaySpec {
        kind,
        file,
        password,
        raw_password,
        from,
        to,
        repeat,
    })
}

fn overlay_verbose_message(report: &[flpdf::OverlayVerbosePage], specs: &[OverlaySpec]) -> Vec<u8> {
    let mut message = b"flpdf: processing underlay/overlay\n".to_vec();
    for page in report {
        message.extend_from_slice(format!("  page {}\n", page.dest_page).as_bytes());
        for source in &page.sources {
            let file = &specs[source.spec_index].file;
            let kind = match source.kind {
                flpdf::OverlayKind::Underlay => "underlay",
                flpdf::OverlayKind::Overlay => "overlay",
            };
            message.extend_from_slice(b"    ");
            message.extend_from_slice(&arg_parser::os_bytes(file));
            message.push(b' ');
            message.extend_from_slice(kind.as_bytes());
            message.push(b' ');
            message.extend_from_slice(source.src_page.to_string().as_bytes());
            message.push(b'\n');
        }
    }
    message
}

#[cfg(test)]
fn parse_test_args(args: Vec<String>) -> CliResult<arg_parser::ParsedArgs> {
    let has_program = args.first().is_some_and(|arg| !arg.starts_with('-'));
    let mut parser_args = args;
    if !has_program {
        parser_args.insert(0, "flpdf".to_owned());
    }
    let mut parsed = arg_parser::ArgParser::from_command(cli_command()).parse(parser_args)?;
    if !has_program {
        parsed.residual_args.remove(0);
    }
    Ok(parsed)
}

#[cfg(test)]
fn rewrite_qpdf_single_dash(args: Vec<String>) -> Vec<String> {
    parse_test_args(args)
        .expect("test argv should parse")
        .residual_args
        .into_iter()
        .map(|arg| arg.into_string().expect("test argv must be UTF-8"))
        .collect()
}

#[cfg(test)]
fn normalize_qpdf_bare_equals(args: Vec<String>) -> Vec<String> {
    rewrite_qpdf_single_dash(args)
}

#[cfg(test)]
fn extract_overlay_groups(args: Vec<String>) -> CliResult<(Vec<String>, Vec<OverlaySpec>)> {
    let parsed = parse_test_args(args)?;
    let mut overlay_specs = Vec::new();
    for segment in parsed.named_segments {
        let kind = match segment.option.as_str() {
            "overlay" => OverlayKind::Overlay,
            "underlay" => OverlayKind::Underlay,
            _ => continue,
        };
        overlay_specs.push(parse_overlay_segment(kind, &segment.tokens)?);
    }
    let residual_args = parsed
        .residual_args
        .into_iter()
        .map(|arg| arg.into_string().expect("test argv must be UTF-8"))
        .collect();
    Ok((residual_args, overlay_specs))
}

#[cfg(test)]
fn extract_attachment_groups(args: Vec<String>) -> CliResult<(Vec<String>, Vec<Vec<String>>)> {
    let parsed = parse_test_args(args)?;
    let groups = parsed
        .named_segments
        .into_iter()
        .filter(|segment| segment.option == "add-attachment")
        .map(|segment| {
            segment
                .tokens
                .into_iter()
                .map(|token| token.into_string().expect("test argv must be UTF-8"))
                .collect()
        })
        .collect();
    let residual_args = parsed
        .residual_args
        .into_iter()
        .map(|arg| arg.into_string().expect("test argv must be UTF-8"))
        .collect();
    Ok((residual_args, groups))
}

/// Build the library [`flpdf::OverlaySpec`]s from the parsed CLI segments,
/// opening each source PDF (with its per-segment `--password`).
///
/// Source files are opened read-only; an authentication failure or unreadable
/// file is surfaced as a CLI error. Weak-crypto sources (RC4, R=5) are accepted
/// unconditionally: `--allow-weak-crypto` only gates weak-crypto *writes*, and
/// an overlay source is a read-only inspection open. This mirrors qpdf's
/// observable behavior and the same treatment the `--check` inspection open
/// applies.
///
/// Page-range defaults match qpdf: an **absent** `--from`/`--to` defaults to all
/// source/destination pages. An **explicit empty** range is distinct from an
/// absent one: empty `--from=` selects an empty source set (so `--repeat` cycles
/// from the first destination page), empty `--to=` selects an empty destination
/// set (the overlay becomes a no-op), and empty `--repeat=` means no repetition
/// (identical to an absent `--repeat`). `--repeat` is `None` by default.
///
/// # Errors
///
/// Returns an error if a source PDF cannot be opened/authenticated or if a
/// stored page-range string fails to parse (already validated by
/// [`parse_overlay_segment`], so a parse failure here would be an internal
/// inconsistency).
#[cfg(test)]
fn build_overlay_specs(
    specs: &[OverlaySpec],
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Vec<flpdf::OverlaySpec<BufReader<File>>>> {
    build_overlay_specs_with_suppression(specs, repair, password, false)
}

fn build_overlay_specs_with_suppression(
    specs: &[OverlaySpec],
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<Vec<flpdf::OverlaySpec<BufReader<File>>>> {
    let mut built = Vec::with_capacity(specs.len());
    for spec in specs {
        let path = PathBuf::from(spec.file.as_os_str());
        let file = File::open(&path).map_err(|error| open_error_with_file(&path, error.into()))?;
        // Overlay sources are read-only; qpdf accepts weak-crypto opens
        // unconditionally because its flag only gates writes. Retain the
        // command-wide open policy (including recovery and xref handling),
        // replacing only the source-local password.
        let mut source_password = password.clone();
        source_password.set_password_bytes(spec.raw_password.clone().or_else(|| {
            spec.password
                .as_ref()
                .map(|password| arg_parser::os_bytes(password))
        }));
        source_password.password_file = None;
        let options = pdf_open_options(repair, &source_password)?;
        let mut source_job = QPDFJob::new();
        source_job.set_logger(cli_logger());
        source_job.set_message_prefix(progname());
        source_job.set_suppress_warnings(suppress_warnings);
        let mut source = source_job
            .open_with_description(BufReader::new(file), path_description(&path), options)
            .map_err(|error| error_with_file(&path, actionable_password_error(error)))?;
        source
            .root_handle()
            .map_err(|error| error_with_file(&path, actionable_password_error(error)))?;

        let kind = match spec.kind {
            OverlayKind::Overlay => flpdf::OverlayKind::Overlay,
            OverlayKind::Underlay => flpdf::OverlayKind::Underlay,
        };
        // Distinguish an absent `--from` (default: all source pages) from an
        // explicit empty `--from=` (empty source set). qpdf treats the latter as
        // "no from pages", so `--repeat` cycles from the first destination page.
        let from = match spec.from.as_deref() {
            None => PageRange::parse("")?,
            Some("") => PageRange::empty(),
            Some(r) => PageRange::parse(r)?,
        };
        // Distinguish an absent `--to` (default: all destination pages) from an
        // explicit empty `--to=` (empty destination set). qpdf treats the latter
        // as selecting no destination pages, so the overlay is a no-op (observed:
        // byte-identical to a plain rewrite of the destination).
        let to = match spec.to.as_deref() {
            None => PageRange::parse("")?,
            Some("") => PageRange::empty(),
            Some(r) => PageRange::parse(r)?,
        };
        // An explicit empty `--repeat=` means "no repeat", identical to an absent
        // `--repeat` (qpdf-observed: byte-identical to the default overlay). Both
        // map to `None`, the canonical no-repeat representation; mapping to
        // `Some(PageRange::empty())` would resolve to the same empty set in
        // `spec_page_sources`, so `None` is preferred.
        let repeat = match spec.repeat.as_deref() {
            None | Some("") => None,
            Some(r) => Some(PageRange::parse(r)?),
        };
        built.push(flpdf::OverlaySpec {
            source,
            kind,
            from,
            to,
            repeat,
        });
    }
    Ok(built)
}

/// Parse each raw CLI `--collate` occurrence through the shared qpdf job
/// configuration parser. qpdf appends values when the option is repeated, so
/// preserve occurrence order in the returned vector.
fn parse_collate_values(raw_values: &[String]) -> CliResult<Option<Vec<usize>>> {
    if raw_values.is_empty() {
        return Ok(None);
    }
    let mut values = Vec::new();
    for raw in raw_values {
        values.extend(QPDFJob::parse_collate(raw)?);
    }
    Ok(Some(values))
}

fn validate_collate_values(raw_values: &[String]) {
    if let Err(error) = parse_collate_values(raw_values) {
        if let Some(usage_error) = find_usage_error(error.as_ref()) {
            usage_exit(usage_error);
        }
        emit_logger_error(format!("{}: {error}\n", progname()));
        std::process::exit(2);
    }
}

/// Apply the page-job-owned keep-open configuration from the CLI surface.
fn configure_keep_files_open(job: &mut QPDFJob, page_ops: &PageOpArgs) -> CliResult<()> {
    if let Some(value) = page_ops.keep_files_open {
        job.set_keep_files_open(matches!(value, CliYesNo::Yes));
    }
    if let Some(threshold) = page_ops.keep_files_open_threshold.as_deref() {
        job.set_keep_files_open_threshold(QPDFJob::parse_keep_files_open_threshold(threshold)?);
    }
    Ok(())
}

/// Validate the qpdf unsigned threshold even when no page operation is
/// selected. qpdf parses all options before dispatch, so a malformed value
/// must not be silently ignored by an ordinary rewrite route.
fn validate_keep_files_open_threshold(page_ops: &PageOpArgs) -> CliResult<()> {
    if let Some(value) = page_ops.keep_files_open_threshold.as_deref() {
        QPDFJob::parse_keep_files_open_threshold(value)?;
    }
    Ok(())
}

/// Run the `--pages` extraction pipeline.
///
/// Processing order is fixed as follows:
///   1. `QPDFJob::handle_page_specs` resolves and copies every `--pages`
///      specification, including repeated occurrences of one source
///   2. apply the shared post-copy page-operation consumers
///   3. write (or split_pages when --split-pages is set)
///
/// qpdf 11.9.0 always enters `QPDFJob::handlePageSpecs` when page
/// specifications exist (`libqpdf/QPDFJob.cc:466-470`). Both the single-source
/// and multi-source CLI paths therefore use the same fresh primary-based job
/// document before the shared rotate, navigation, annotation, and writer
/// completion boundary.
#[allow(clippy::too_many_arguments)]
fn run_page_extraction(
    primary_input: &std::path::Path,
    output: &std::path::Path,
    repair: bool,
    password: &PasswordArgs,
    json_input: bool,
    update_from_json: Option<&Path>,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    image_options: Option<ImageOptimizationOptions>,
    verbose: bool,
    no_warn: bool,
) -> CliResult<()> {
    // `--split-pages` writes one numbered file per output page rather than a
    // single `output` path, so `output` is a naming template here, not a
    // literal file to compare against `primary_input` — matching qpdf's own
    // `(!m->split_pages) && QUtil::same_file(...)` exclusion in
    // `checkConfiguration()` (`QPDFJob.cc:627`).
    if !split_pages_active(page_ops.split_pages.as_deref()) {
        reject_same_job_output(primary_input, output)?;
    }
    let standard_output = prepare_page_operation_standard_output(output, page_ops)?;
    let creates_output = standard_output.is_none();
    if page_ops.empty {
        // qpdf accepts `--empty`; ignoring it would silently change which
        // document supplies the catalog/outlines. Fail loudly instead.
        return Err(
            "--empty is accepted by qpdf but not implemented in flpdf at this layer \
             (tracked separately); rerun without --empty"
                .into(),
        );
    }

    let page_tokens = raw_page_tokens(page_ops);
    let specs = parse_pages_segment(&page_tokens)?;
    let mut inputs = resolve_page_specs(&specs, primary_input)?;
    let has_external_source = inputs.iter().any(|spec| spec.path != primary_input);

    // The in-place single-document planner must use the top-level password
    // for the already-authenticated primary when `--pages . ...` carries no
    // segment password. The multi-source QPDFJob route opens the primary
    // separately and must leave secondary credentials segment-local: qpdf does
    // not fall back to the primary password for a distinct source
    // (QPDFJob.cc:2400-2412).
    if !has_external_source {
        if let Some(top_pw) = password.password_bytes() {
            for spec in &mut inputs {
                if spec.password.is_none() {
                    spec.password = Some(top_pw.clone());
                }
            }
        }
    }

    // Keep the canonicalized path set for the JSON-input guard and the
    // single-source verbose route. Ordinary page operations below use the
    // literal qpdf filename identity: two spellings of the same file may be
    // distinct QPDF sources, as documented by qpdf's page-spec API.
    let mut distinct: Vec<std::path::PathBuf> = Vec::new();
    for spec in &inputs {
        // Source inputs must exist to be opened; if canonicalization fails
        // fall back to the literal path (the open will surface a clear error).
        let key = std::fs::canonicalize(&spec.path).unwrap_or_else(|_| spec.path.clone());
        if !distinct.contains(&key) {
            distinct.push(key);
        }
    }
    if json_input || update_from_json.is_some() {
        // `run_page_extraction_from_single_source` below applies every spec's
        // range to the single already-opened job document; it has no way to
        // honor a `spec.path` that names a genuinely different file. The
        // `distinct.len() > 1` check above only catches this
        // when two *explicit* paths disagree with each other -- a lone
        // explicit source (e.g. `--pages other.pdf 1`, no `.` segment) never
        // puts `primary_input` itself into `distinct`, so it silently
        // resolves to a single-element `distinct` and slips past. Comparing
        // every resolved spec path against `primary_input`'s own canonical
        // path here closes that gap without touching the ordinary branch's
        // (already correct) handling of a genuinely different single source.
        let primary_canonical =
            std::fs::canonicalize(primary_input).unwrap_or_else(|_| primary_input.to_path_buf());
        if distinct.iter().any(|path| *path != primary_canonical) {
            return Err(
                "--pages: cross-document page merge is not supported at this layer \
                 (an explicit --pages source differs from the --json-input/\
                 --update-from-json primary input). Single-document extraction \
                 ('.' or the primary input's own path) is supported; cross-doc \
                 merge with a JSON-created/updated primary is tracked in a \
                 separate issue."
                    .into(),
            );
        }
        let opened = open_job_pdf(
            primary_input,
            repair,
            password,
            json_input,
            update_from_json,
            false,
            no_warn,
        )?;
        return match opened {
            JobPdf::File(pdf) => run_page_extraction_from_single_source(
                pdf,
                primary_input,
                output,
                repair,
                password,
                page_ops,
                overlay_specs,
                remove_unref,
                options,
                linearize,
                linearize_pass1,
                image_options,
                verbose,
                standard_output,
                creates_output,
                &inputs,
                no_warn,
            ),
            JobPdf::Json(pdf) => run_page_extraction_from_single_source(
                pdf,
                primary_input,
                output,
                repair,
                password,
                page_ops,
                overlay_specs,
                remove_unref,
                options,
                linearize,
                linearize_pass1,
                image_options,
                verbose,
                standard_output,
                creates_output,
                &inputs,
                no_warn,
            ),
        };
    }

    // qpdf's ordinary page-spec job owns every page-spec selection, whether
    // the segment names one source or several. Distinct input documents are
    // copied into the primary output by the same library QPDFJob facade as
    // the single-source route below.
    if has_external_source {
        return run_page_extraction_from_multiple_sources(
            primary_input,
            output,
            repair,
            password,
            page_ops,
            overlay_specs,
            remove_unref,
            options,
            linearize,
            linearize_pass1,
            image_options,
            verbose,
            no_warn,
            standard_output,
            creates_output,
            inputs,
        );
    }

    run_page_extraction_from_single_source(
        open_pdf_with_suppression(&primary_input.to_path_buf(), repair, password, no_warn)?,
        primary_input,
        output,
        repair,
        password,
        page_ops,
        overlay_specs,
        remove_unref,
        options,
        linearize,
        linearize_pass1,
        image_options,
        verbose,
        standard_output,
        creates_output,
        &inputs,
        no_warn,
    )
}

/// Run qpdf's `--empty --pages` route with an empty primary document.
///
/// qpdf's empty primary has no input filename and all page specifications are
/// therefore secondary sources. Keep this as the same `QPDFJob::handle_page_specs`
/// boundary used by ordinary multi-source extraction so source-count policy,
/// collate order, copying, and final writing cannot drift between the two
/// command shapes.
#[allow(clippy::too_many_arguments)]
fn run_empty_page_extraction(
    output: &Path,
    repair: bool,
    password: &PasswordArgs,
    update_from_json: Option<&Path>,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    image_options: Option<ImageOptimizationOptions>,
    verbose: bool,
    no_warn: bool,
) -> CliResult<()> {
    let standard_output = prepare_page_operation_standard_output(output, page_ops)?;
    let creates_output = standard_output.is_none();
    let page_tokens = raw_page_tokens(page_ops);
    let raw_specs = parse_pages_segment(&page_tokens)?;
    if raw_specs
        .iter()
        .any(|spec| spec.file_token == OsStr::new("."))
    {
        return Err("--pages: '.' cannot refer to a primary input with --empty".into());
    }
    let inputs = resolve_page_specs(&raw_specs, Path::new("<empty>"))?;

    let mut source_paths = Vec::new();
    let mut source_passwords = Vec::new();
    let mut specs = Vec::with_capacity(inputs.len());
    for input in inputs {
        let source_index =
            if let Some(index) = source_paths.iter().position(|path| *path == input.path) {
                index + 1
            } else {
                source_paths.push(input.path.clone());
                source_passwords.push(input.password.clone());
                source_paths.len()
            };
        specs.push(PageSpecInput::new(source_index, input.range));
    }

    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_verbose(verbose);
    job.set_suppress_warnings(no_warn);
    configure_keep_files_open(&mut job, page_ops)?;
    let keep_files_open = job.keep_files_open_for_page_specs(&specs);
    job.report_page_spec_selection(&specs)?;

    let mut sources = Vec::with_capacity(source_paths.len() + 1);
    sources.push(job.create_empty_document()?);
    // qpdf's createQPDF applies --update-from-json to the primary
    // immediately after creating it (empty or otherwise), before any page
    // specification is processed (QPDFJob.cc:459-462). Live-probed:
    // `qpdf --update-from-json=<file> --empty --pages ...` actually applies
    // the update against the empty primary and surfaces JSON validation
    // errors (exit 2 on malformed JSON), it is not a silent no-op.
    apply_json_update_with_job(&mut job, &mut sources[0], update_from_json)?;
    for (source_index, path) in source_paths.iter().enumerate() {
        job.report_page_source_processing(path_description(path))?;
        let mut source_password = password.clone();
        source_password.set_password_bytes(source_passwords[source_index].clone());
        source_password.password_file = None;
        sources.push(open_page_source(
            path,
            repair,
            &source_password,
            keep_files_open,
            no_warn,
        )?);
    }

    // qpdf raises the output version floor from every source participating in
    // a page job, including the empty primary's source heap
    // (`QPDFJob.cc:1714-1715,2847-2918`). Keep that floor separate from the
    // explicit raw minimum so the writer can apply qpdf's setter order.
    let mut options = options;
    for source in &mut sources {
        update_input_version_floor(&mut options.input_version_floor, source)?;
    }

    let collate = parse_collate_values(&page_ops.collate)?;
    let source_warnings = job.has_warnings();
    let page_output = job.handle_page_specs(
        &mut sources,
        &specs,
        collate.as_deref(),
        remove_unref.into(),
        options.preserve_unreferenced_objects,
    )?;
    let source_warnings = source_warnings || job.has_warnings();
    let PageSpecJobOutput::Merged(mut merged) = page_output else {
        return Err("--empty --pages unexpectedly returned an in-place document".into());
    };
    let selected = pages::page_refs(&mut merged)?;
    let combined_pages = selected
        .iter()
        .enumerate()
        .map(|(index, &page_ref)| {
            Ok(CombinedPage {
                source_index: 0,
                page: flpdf::SelectedPage {
                    index_1based: u32::try_from(index + 1)
                        .map_err(|_| "--pages: too many output pages")?,
                    page_ref,
                },
            })
        })
        .collect::<CliResult<Vec<_>>>()?;

    run_page_extraction_after_plan(
        &mut merged,
        output,
        Path::new("<empty>"),
        repair,
        password,
        page_ops,
        overlay_specs,
        remove_unref,
        options,
        linearize,
        linearize_pass1,
        verbose,
        standard_output,
        creates_output,
        false,
        None,
        source_warnings,
        None,
        combined_pages,
        image_options,
        no_warn,
    )
}

/// Run qpdf's ordinary multi-source page-spec path.
///
/// The primary document is retained at source index zero even when no page
/// spec selects it. Every other literal filename is opened once and reused by
/// the job-level page-spec planner, matching qpdf's page heap keyed by the
/// filename token. The resulting fresh merged document is then passed through
/// the same rotate/structure/overlay/write completion boundary as a
/// single-document extraction.
#[allow(clippy::too_many_arguments)]
fn run_page_extraction_from_multiple_sources(
    primary_input: &Path,
    output: &Path,
    repair: bool,
    password: &PasswordArgs,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    image_options: Option<ImageOptimizationOptions>,
    verbose: bool,
    no_warn: bool,
    standard_output: Option<PipelineWriter>,
    creates_output: bool,
    inputs: Vec<InputSpec>,
) -> CliResult<()> {
    // qpdf inherits output encryption from the primary input for page
    // operations. Keep this probe separate from the mutable source vector so
    // source opening below can use the same top-level password policy.
    let primary_encrypted = open_page_source(
        &primary_input.to_path_buf(),
        repair,
        password,
        true,
        no_warn,
    )?
    .is_encrypted();

    // Build literal-path source identity and one qpdf page specification per
    // occurrence. `.` was already normalized to primary_input by
    // resolve_page_specs; path equality therefore preserves qpdf's documented
    // distinction between two different spellings of the same file.
    let mut source_paths = vec![primary_input.to_path_buf()];
    let mut source_passwords: Vec<Option<Vec<u8>>> = vec![None];
    let mut specs = Vec::with_capacity(inputs.len());
    for input in inputs {
        let source_index = if input.path == primary_input {
            0
        } else if let Some(index) = source_paths.iter().position(|path| path == &input.path) {
            index
        } else {
            let index = source_paths.len();
            source_paths.push(input.path.clone());
            source_passwords.push(input.password.clone());
            index
        };
        specs.push(PageSpecInput::new(source_index, input.range));
    }

    let mut sources = Vec::with_capacity(source_paths.len());
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_verbose(verbose);
    job.set_suppress_warnings(no_warn);
    configure_keep_files_open(&mut job, page_ops)?;
    let keep_files_open = job.keep_files_open_for_page_specs(&specs);
    job.report_page_spec_selection(&specs)?;
    sources.push(open_page_source(
        &primary_input.to_path_buf(),
        repair,
        password,
        true,
        no_warn,
    )?);
    for (source_index, path) in source_paths.iter().enumerate().skip(1) {
        let mut source_password = password.clone();
        // qpdf opens each secondary with only the password attached to its
        // page specification (QPDFJob.cc:2400-2412). The primary password is
        // not a fallback for a secondary with no segment password; retain the
        // global interpretation/policy flags, but replace both credential
        // fields with the per-source value, including an explicit empty value.
        source_password.set_password_bytes(source_passwords[source_index].clone());
        source_password.password_file = None;
        job.report_page_source_processing(path_description(path))?;
        sources.push(open_page_source(
            path,
            repair,
            &source_password,
            keep_files_open,
            no_warn,
        )?);
    }

    // qpdf raises the writer floor from every input processed by the job
    // (`QPDFJob.cc:1714-1715`) and applies that floor before explicit
    // --min-version/--force-version settings
    // (`QPDFJob.cc:2847-2918`). The merged fresh document starts at its
    // baseline version, so carry the source floor explicitly through the
    // multi-source consumer boundary without rewriting the raw minimum.
    let mut options = options;
    for source in &mut sources {
        update_input_version_floor(&mut options.input_version_floor, source)?;
    }

    let primary_copy_encryption = sources
        .first_mut()
        .ok_or("--pages: primary input was not opened")?
        .writer_copy_encryption_source()?;

    let collate = parse_collate_values(&page_ops.collate)?;
    let source_warnings = job.has_warnings();
    let page_output = job.handle_page_specs(
        &mut sources,
        &specs,
        collate.as_deref(),
        remove_unref.into(),
        options.preserve_unreferenced_objects,
    )?;
    let source_warnings = source_warnings || job.has_warnings();

    let mut merged = match page_output {
        PageSpecJobOutput::Merged(merged) => merged,
        PageSpecJobOutput::InPlace { .. } => {
            return Err("--pages: multiple-source job returned an in-place result".into());
        }
    };

    // The merge job has already rebuilt the target page tree and copied the
    // primary document-level structures. Represent its current output pages
    // as a local selection so the shared post-selection consumer can apply
    // rotate, cleanup, overlays, split naming, and writer options without
    // reintroducing source-document ObjectRefs.
    let selected = pages::page_refs(&mut merged)?;
    let combined_pages: Vec<CombinedPage> = selected
        .iter()
        .enumerate()
        .map(|(index, &page_ref)| {
            Ok(CombinedPage {
                source_index: 0,
                page: flpdf::SelectedPage {
                    index_1based: u32::try_from(index + 1)
                        .map_err(|_| "--pages: too many output pages")?,
                    page_ref,
                },
            })
        })
        .collect::<CliResult<Vec<_>>>()?;

    run_page_extraction_after_plan(
        &mut merged,
        output,
        primary_input,
        repair,
        password,
        page_ops,
        overlay_specs,
        // QPDFJob has already applied the page-copy resource policy to each
        // The page-copy job has already applied its source-side resource
        // policy. Retain the original mode for the later doSplitPages
        // preflight; the post-copy completion boundary itself remains a
        // no-op for resource pruning.
        remove_unref,
        options,
        linearize,
        linearize_pass1,
        verbose,
        standard_output,
        creates_output,
        primary_encrypted,
        primary_copy_encryption,
        source_warnings,
        None,
        combined_pages,
        image_options,
        no_warn,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_page_extraction_from_single_source<R: Read + Seek + 'static>(
    mut pdf: Pdf<R>,
    primary_input: &Path,
    output: &Path,
    repair: bool,
    password: &PasswordArgs,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    image_options: Option<ImageOptimizationOptions>,
    verbose: bool,
    standard_output: Option<PipelineWriter>,
    creates_output: bool,
    inputs: &[InputSpec],
    no_warn: bool,
) -> CliResult<()> {
    let primary_encrypted = pdf.is_encrypted();
    let primary_copy_encryption = pdf.writer_copy_encryption_source()?;
    let specs: Vec<PageSpecInput> = inputs
        .iter()
        .map(|input| PageSpecInput::new(0, input.range.clone()))
        .collect();
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_verbose(verbose);
    job.set_suppress_warnings(no_warn);
    configure_keep_files_open(&mut job, page_ops)?;
    job.report_page_spec_selection(&specs)?;

    let collate = parse_collate_values(&page_ops.collate)?;
    let mut sources = vec![pdf];
    let before_warnings = job.has_warnings();
    let page_output = job.handle_page_specs(
        &mut sources,
        &specs,
        collate.as_deref(),
        remove_unref.into(),
        options.preserve_unreferenced_objects,
    )?;
    let source_warnings = before_warnings || job.has_warnings();

    match page_output {
        PageSpecJobOutput::InPlace {
            pdf,
            result,
            prune_mode,
        } => {
            let combined_pages: Vec<CombinedPage> = result
                .new_kids
                .iter()
                .enumerate()
                .map(|(index, &page_ref)| {
                    Ok(CombinedPage {
                        source_index: 0,
                        page: flpdf::SelectedPage {
                            index_1based: u32::try_from(index + 1)
                                .map_err(|_| "--pages: too many output pages")?,
                            page_ref,
                        },
                    })
                })
                .collect::<CliResult<Vec<_>>>()?;

            run_page_extraction_after_plan(
                pdf,
                output,
                primary_input,
                repair,
                password,
                page_ops,
                overlay_specs,
                remove_unref,
                options,
                linearize,
                linearize_pass1,
                verbose,
                standard_output,
                creates_output,
                primary_encrypted,
                primary_copy_encryption,
                source_warnings,
                Some((result, prune_mode)),
                combined_pages,
                image_options,
                no_warn,
            )
        }
        PageSpecJobOutput::Merged(mut merged) => {
            let selected = pages::page_refs(&mut merged)?;
            let combined_pages: Vec<CombinedPage> = selected
                .iter()
                .enumerate()
                .map(|(index, &page_ref)| {
                    Ok(CombinedPage {
                        source_index: 0,
                        page: flpdf::SelectedPage {
                            index_1based: u32::try_from(index + 1)
                                .map_err(|_| "--pages: too many output pages")?,
                            page_ref,
                        },
                    })
                })
                .collect::<CliResult<Vec<_>>>()?;

            run_page_extraction_after_plan(
                &mut merged,
                output,
                primary_input,
                repair,
                password,
                page_ops,
                overlay_specs,
                // QPDFJob has already applied the page-copy resource policy
                // to each source page. Retain the original mode only for the
                // later doSplitPages preflight; post-copy completion remains
                // a no-op for resource pruning.
                remove_unref,
                options,
                linearize,
                linearize_pass1,
                verbose,
                standard_output,
                creates_output,
                primary_encrypted,
                primary_copy_encryption,
                source_warnings,
                None,
                combined_pages,
                image_options,
                no_warn,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_page_extraction_after_plan<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    output: &Path,
    input_path: &Path,
    repair: bool,
    password: &PasswordArgs,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    verbose: bool,
    mut standard_output: Option<PipelineWriter>,
    creates_output: bool,
    primary_encrypted: bool,
    primary_copy_encryption: Option<CopyEncryptionSource>,
    prior_warnings: bool,
    page_job_result: Option<(RebuildResult, RemoveUnreferencedResources)>,
    combined_pages: Vec<CombinedPage>,
    image_options: Option<ImageOptimizationOptions>,
    no_warn: bool,
) -> CliResult<()> {
    pdf.set_suppress_warnings(no_warn);
    let selected: Vec<ObjectRef> = combined_pages.iter().map(|cp| cp.page.page_ref).collect();

    let (result, prune_mode) = if let Some((result, prune_mode)) = page_job_result {
        (result, prune_mode)
    } else {
        // qpdf's --pages Auto mode scans the source page tree before it
        // removes the old pages. A page-local indirect /Resources that appears
        // only once does not trigger the expensive page-helper pruning route;
        // inherited or shared resources do (QPDFJob.cc:2251-2337). Preserve
        // that decision before rebuild_page_tree flattens the original
        // inheritance structure.
        let prune_mode = if remove_unref == CliRemoveUnreferencedResources::Auto
            && !should_remove_unreferenced_resources(pdf)?
        {
            CliRemoveUnreferencedResources::No
        } else {
            remove_unref
        };
        let result = rebuild_page_tree(pdf, &selected)?;
        copy_duplicate_page_annotations(pdf, &result)?;
        (result, prune_mode.into())
    };
    QPDFJob::complete_in_place_page_selection(pdf, &result, prune_mode)?;
    apply_rotate_specs(pdf, &page_ops.rotate, &result.new_kids)?;

    // qpdf runs image externalization/optimization after page selection and
    // before the final writer (`QPDFJob.cc:2151-2174`). Keep the same order so
    // selected pages, including copied pages from secondary sources, are the
    // only images considered by this job.
    if let Some(image_options) = image_options {
        flpdf::optimize_images(pdf, &cli_logger(), &progname(), verbose, image_options)?;
    }

    let mut options = options;
    let split_pages = page_ops
        .split_pages
        .as_deref()
        .map(parse_split_n)
        .transpose()?;
    let split_pages_active = split_pages.is_some_and(|size| size > 0);
    options.preserve_encryption = primary_encrypted && !split_pages_active;
    // qpdf keeps the authenticated primary input as the output/base document
    // for `--pages` (libqpdf/QPDFJob.cc:2360-2633). The multi-source job has
    // already copied selected pages into a fresh plaintext Pdf, so its writer
    // cannot rediscover the primary's encryption from the merged document.
    // Carry the authenticated donor explicitly to the final writer; split
    // chunks remain cleartext, matching qpdf's fresh chunk writers. Gate on
    // the same conditions as `PdfWriter::prepared_write_options`'s implicit
    // `can_preserve` (`writer.rs:645-652`) so an explicit source
    // doesn't bypass qpdf's QDF-is-always-cleartext contract
    // (`cell_a_encrypted_input_is_transparently_decrypted_by_qdf`) or its
    // `decode_level == DecodeLevel::None` requirement: `--stream-data`
    // `Uncompress`/`Compress` raise the writer's decode level above `None`
    // (`WriterConfiguration::set_stream_data_mode`, `writer.rs:127-142`),
    // and an explicit non-`none` `--decode-level` does the same directly,
    // both of which `can_preserve` would likewise refuse to auto-preserve
    // through.
    if !split_pages_active
        && options.copy_encryption.is_none()
        && !options.qdf
        && !options.content_normalization
        && !matches!(
            options.stream_data,
            Some(StreamDataMode::Uncompress) | Some(StreamDataMode::Compress)
        )
        && !(options.decode_level_set && options.decode_level != StreamDecodeLevel::None)
    {
        options.copy_encryption = primary_copy_encryption;
    }
    // qpdf keeps a provider-backed source QPDF alive when
    // `copyForeignObject` copies a Form XObject whose data comes from a
    // `StreamDataProvider` (`libqpdf/QPDF.cc:2248-2257`). Retain the opened
    // source documents through the in-memory writer for the same reason.
    let _built_overlay_specs = if !overlay_specs.is_empty() {
        let mut built =
            build_overlay_specs_with_suppression(overlay_specs, repair, password, no_warn)?;
        update_input_version_floor(&mut options.input_version_floor, pdf)?;
        for spec in built.iter_mut() {
            update_input_version_floor(&mut options.input_version_floor, &mut spec.source)?;
        }

        if verbose {
            let report = flpdf::overlay_verbose_report(pdf, &mut built)?;
            logger_info(overlay_verbose_message(&report, overlay_specs))?;
        }

        flpdf::apply_overlay_specs(pdf, &mut built)?;
        Some(built)
    } else {
        None
    };

    let split_progress = split_pages_active && options.progress;
    if split_progress {
        // qpdf creates a fresh writer for each split output. The memory
        // rewrite is flpdf's internal preparation step and is not an
        // observable qpdf writer, so it must not consume the progress stream.
        options.progress = false;
    }
    if let Some(n) = split_pages.filter(|size| *size > 0) {
        let bytes = write_qpdf_to_memory(pdf, output, &options, linearize)?;
        let (_, mut split_job) = split_rewritten_pdf(
            bytes,
            n,
            output,
            input_path,
            options.deterministic_id,
            split_progress,
            verbose,
            no_warn,
            remove_unref.into(),
            writer_configuration(&options, linearize, linearize_pass1)?,
        )?;
        // The intermediate rewrite may already have repaired the condition
        // that produced a warning in the original source (e.g. --repair's
        // xref reconstruction) or the source document `pdf` itself, so the
        // freshly re-opened split source can look clean even though the
        // original input was not. Fold both signals into the split job
        // before completing it, matching what the non-split branch's
        // `finish_operation_warnings_with_prior` checks below.
        split_job.record_document_warnings(pdf);
        if prior_warnings {
            split_job.record_warnings();
        }
        return finish_job_exit_status(split_job.complete(true)?);
    } else {
        let announce_file = standard_output.is_none();
        write_with_pdf_writer(
            pdf,
            output,
            &mut standard_output,
            &options,
            linearize,
            linearize_pass1,
        )?;
        if verbose && announce_file {
            logger_info(wrote_file_message(&progname(), output))?;
        }
    }
    finish_operation_warnings_with_prior(pdf, creates_output, prior_warnings)
}

/// Apply each `--rotate` spec (in order) to `target_pages`, resolving each
/// spec's page-range against the number of target pages.
fn apply_rotate_specs<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    rotate_args: &[String],
    target_pages: &[ObjectRef],
) -> CliResult<()> {
    if rotate_args.is_empty() {
        return Ok(());
    }
    let total = u32::try_from(target_pages.len())
        .map_err(|_| "too many pages to apply --rotate".to_string())?;
    for raw in rotate_args {
        let spec =
            RotateSpec::parse(raw).map_err(|e| format!("--rotate: invalid spec {raw:?}: {e}"))?;
        // qpdf's handleRotations resolves each range against the real page
        // count and then filters `0 <= pageno < npages` before touching
        // `pages`, so an empty document rotates nothing without erroring
        // (confirmed live: `--collate=0 --rotate=90` exits 0). A resolved
        // range's own out-of-bounds check requires page_count >= 1, so this
        // document-empty case is handled up front instead of via resolve().
        let pages: Vec<ObjectRef> = if total == 0 {
            Vec::new()
        } else {
            let indices = spec
                .range
                .resolve(total)
                .map_err(|e| format!("--rotate: page range out of bounds in {raw:?}: {e}"))?;
            indices
                .iter()
                .filter_map(|&i| target_pages.get((i - 1) as usize).copied())
                .collect()
        };
        apply_rotate_to_pages(pdf, &pages, &spec.op)?;
    }
    Ok(())
}

/// Parse `--split-pages[=n]` (default 1; qpdf-compatible).
fn parse_split_n(raw: &str) -> CliResult<usize> {
    let n: usize = raw
        .parse()
        .map_err(|_| format!("--split-pages: expected a non-negative integer, got {raw:?}"))?;
    Ok(n)
}

/// Whether `--split-pages` selects qpdf's chunk-writing path.
///
/// qpdf stores the parsed value in a signed field and dispatches only when it
/// is truthy. Invalid values remain active here so `parse_split_n` can report a
/// usage error instead of silently selecting the ordinary rewrite path.
fn split_pages_active(raw: Option<&str>) -> bool {
    raw.is_some_and(|value| value.parse::<usize>().map_or(true, |size| size > 0))
}

/// Run qpdf's fresh-document split job on a rewritten in-memory source.
///
/// The page-operation pipeline has already applied its transforms to `bytes`;
/// the job-level split owns the subsequent per-chunk page copy, naming,
/// annotation, label, and output-file lifecycle. `input_path` remains the
/// original command input so the qpdf same-file guard can reject a generated
/// chunk that would truncate it.
#[allow(clippy::too_many_arguments)]
fn split_rewritten_pdf(
    bytes: Vec<u8>,
    chunk_size: usize,
    output: &Path,
    input_path: &Path,
    deterministic_id: bool,
    progress: bool,
    verbose: bool,
    suppress_warnings: bool,
    remove_unreferenced_resources: RemoveUnreferencedResources,
    writer_configuration: WriterConfiguration,
) -> CliResult<(Vec<PathBuf>, QPDFJob)> {
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    // qpdf never creates a second job for `--split-pages`: `writeQPDF`
    // (`QPDFJob.cc:483-503`) calls `doSplitPages` on `this` and gates
    // "operation succeeded with warnings" on the same `m->suppress_warnings`
    // used everywhere else. flpdf's split path uses a fresh `QPDFJob`
    // instead, so that job's own suppression must be set explicitly, and
    // before `split_pages` runs so warnings raised during the split itself
    // are suppressed too, not only the final summary line.
    job.set_suppress_warnings(suppress_warnings);
    let input_name = input_path.to_string_lossy().into_owned();
    let mut pdf = job.open(Cursor::new(bytes), input_name, PdfOpenOptions::default())?;
    if progress {
        job.set_progress(true);
        job.set_output_file(output.to_path_buf())?;
    }
    let options = SplitPageOptions::new(chunk_size, output)
        .with_input_path(input_path)
        .with_deterministic_id(deterministic_id)
        .with_verbose(verbose)
        .with_remove_unreferenced_resources(remove_unreferenced_resources)
        .with_writer_configuration(writer_configuration);
    let written = job.split_pages(&mut pdf, options)?;
    Ok((written, job))
}

/// Apply `--rotate` / `--split-pages` to a plain (no `--pages`) rewrite.
///
/// qpdf accepts these without `--pages` (exit 0). `--rotate` mutates the
/// source document's pages directly (no page-tree rebuild); `--split-pages`
/// chunks the rewritten output. `--collate` without `--pages` is a no-op,
/// matching qpdf.
#[allow(clippy::too_many_arguments)]
fn run_rewrite_with_page_ops(
    input: &std::path::Path,
    output: &std::path::Path,
    repair: bool,
    password: &PasswordArgs,
    json_input: bool,
    update_from_json: Option<&Path>,
    page_ops: &PageOpArgs,
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    image_options: Option<ImageOptimizationOptions>,
    verbose: bool,
    no_warn: bool,
) -> CliResult<()> {
    let opened = open_job_pdf(
        input,
        repair,
        password,
        json_input,
        update_from_json,
        false,
        no_warn,
    )?;
    match opened {
        JobPdf::File(pdf) => run_rewrite_with_page_ops_opened(
            pdf,
            input,
            output,
            page_ops,
            remove_unref,
            options,
            linearize,
            linearize_pass1,
            image_options,
            verbose,
        ),
        JobPdf::Json(pdf) => run_rewrite_with_page_ops_opened(
            pdf,
            input,
            output,
            page_ops,
            remove_unref,
            options,
            linearize,
            linearize_pass1,
            image_options,
            verbose,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_rewrite_with_page_ops_opened<R: Read + Seek + 'static>(
    mut pdf: Pdf<R>,
    input: &Path,
    output: &std::path::Path,
    page_ops: &PageOpArgs,
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    image_options: Option<ImageOptimizationOptions>,
    verbose: bool,
) -> CliResult<()> {
    let mut standard_output = prepare_page_operation_standard_output(output, page_ops)?;
    let creates_output = standard_output.is_none();
    if page_ops.empty {
        return Err(
            "--empty is accepted by qpdf but not implemented in flpdf at this layer \
             (tracked separately); rerun without --empty"
                .into(),
        );
    }
    if !page_ops.rotate.is_empty() {
        let page_refs = pages::page_refs(&mut pdf)?;
        apply_rotate_specs(&mut pdf, &page_ops.rotate, &page_refs)?;
    }
    if let Some(image_options) = image_options {
        flpdf::optimize_images(&mut pdf, &cli_logger(), &progname(), verbose, image_options)?;
    }

    // Page operations emit a fresh document and preserve encryption only when
    // the primary input itself was encrypted, matching qpdf's page copier.
    // `--split-pages` is the exception: qpdf's doSplitPages path makes a fresh
    // empty output document per chunk, so its intermediate and final chunks
    // are cleartext unless explicit encryption options are configured. Keep
    // the memory intermediate decryptable before split_pages re-opens it.
    let mut options = options;
    let split_pages = page_ops
        .split_pages
        .as_deref()
        .map(parse_split_n)
        .transpose()?;
    let split_pages_active = split_pages.is_some_and(|size| size > 0);
    options.preserve_encryption = !split_pages_active && pdf.is_encrypted();
    let split_progress = split_pages_active && options.progress;
    if split_progress {
        // qpdf creates a fresh writer for each split output. The memory
        // rewrite is flpdf's internal preparation step and is not an
        // observable qpdf writer, so it must not consume the progress stream.
        options.progress = false;
    }

    if let Some(n) = split_pages.filter(|size| *size > 0) {
        let suppress_warnings = pdf.suppress_warnings();
        let bytes = write_qpdf_to_memory(&mut pdf, output, &options, linearize)?;
        let (_, mut split_job) = split_rewritten_pdf(
            bytes,
            n,
            output,
            input,
            options.deterministic_id,
            split_progress,
            verbose,
            suppress_warnings,
            remove_unref.into(),
            writer_configuration(&options, linearize, linearize_pass1)?,
        )?;
        // The intermediate rewrite may already have repaired the condition
        // that produced a warning on the original `pdf` (e.g. --repair's
        // xref reconstruction), so the freshly re-opened split source can
        // look clean even though the original input was not.
        split_job.record_document_warnings(&pdf);
        return finish_job_exit_status(split_job.complete(true)?);
    } else {
        let announce_file = standard_output.is_none();
        write_with_pdf_writer(
            &mut pdf,
            output,
            &mut standard_output,
            &options,
            linearize,
            linearize_pass1,
        )?;
        if verbose && announce_file {
            logger_info(wrote_file_message(&progname(), output))?;
        }
    }
    finish_operation_warnings(&pdf, creates_output)
}

/// True when any page-operation flag that requires the page-op code paths is
/// set. `--collate` alone (no `--pages`) is a documented no-op and does NOT
/// trigger this on its own.
fn page_ops_active(p: &PageOpArgs) -> bool {
    !p.pages.is_empty()
        || !p.rotate.is_empty()
        || split_pages_active(p.split_pages.as_deref())
        || p.empty
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentNormalizationWarning {
    parsed_offset: Option<u64>,
    last_token_was_bad: bool,
}

/// Normalize all page content streams in an in-memory PDF graph.
///
/// Shared by the plain and linearized rewrite paths so both use the same page
/// traversal, indirect `/Contents` handling, alias deduplication, and warning
/// order.
fn normalize_page_contents<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> CliResult<Vec<ContentNormalizationWarning>> {
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    let page_refs = pages::page_refs(pdf)?;
    for page_ref in page_refs {
        warnings.extend(apply_normalize_content(pdf, page_ref, &mut seen)?);
    }
    Ok(warnings)
}

/// Normalize the content stream(s) for a single page.
///
/// Reads each `/Contents` stream referenced by the page, applies
/// [`normalize_content_stream`] to the decoded bytes, and writes the result
/// back into the in-memory [`Pdf`] model through live `ObjectHandle` mutation.
///
/// The `/Length` entry in each stream's dictionary is updated to the new
/// (normalized) byte count. No filter is applied here — the canonical writer
/// emits the already-normalized bytes through qpdf's normalization branch,
/// which takes precedence over ordinary stream compression.
fn apply_normalize_content<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    seen: &mut HashSet<ObjectRef>,
) -> CliResult<Vec<ContentNormalizationWarning>> {
    let mut warnings = Vec::new();
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve(&page)?;
    let contents = page.get_key(b"/Contents");
    let contents_ref = contents.object_ref();
    pdf.resolve(&contents)?;

    let mut streams = Vec::new();
    if contents.as_stream_dict().is_some() {
        if let Some(stream_ref) = contents_ref {
            streams.push((stream_ref, contents));
        }
    } else if let Some(items) = contents.as_array() {
        for item in items {
            let item_ref = item.object_ref();
            pdf.resolve(&item)?;
            if item.as_stream_dict().is_some() {
                if let Some(item_ref) = item_ref {
                    streams.push((item_ref, item));
                }
            }
        }
    }

    for (stream_ref, stream) in streams {
        if let Some(last_bad) = normalize_and_store_stream_handle(pdf, stream_ref, stream, seen)? {
            warnings.push(last_bad);
        }
    }
    Ok(warnings)
}

/// Normalize the decoded bytes of the indirect stream at `stream_ref` through
/// the live ObjectHandle stream pipeline and mutate that same qpdf-style
/// stream in place. Keeping the stream handle live is important for malformed
/// content holders: the writer must observe one canonical resolution and not
/// parse the legacy raw Object a second time.
fn normalize_and_store_stream_handle<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    stream_ref: ObjectRef,
    stream: ObjectHandle,
    seen: &mut HashSet<ObjectRef>,
) -> CliResult<Option<ContentNormalizationWarning>> {
    if !seen.insert(stream_ref) {
        return Ok(None);
    }

    // Decode the stored bytes through qpdf's canonical stream pipeline. This
    // resolves indirect filters/parameters and preserves source recovery
    // diagnostics on the owning document.
    let decoded = stream.get_stream_data(StreamDecodeLevel::All)?;

    // Normalize the decoded content stream bytes.
    let normalized = normalize_content_stream(decoded.as_ref());
    let warning = normalized
        .any_bad_tokens()
        .then(|| ContentNormalizationWarning {
            parsed_offset: u64::try_from(stream.get_parsed_offset()).ok(),
            last_token_was_bad: normalized.last_token_was_bad(),
        });
    let normalized = normalized.into_bytes();

    // Remove filter / encode-form keys and install the fresh direct length;
    // the normalized payload is raw. This is qpdf's in-place stream mutation
    // boundary, so mark the canonical indirect owner dirty for the writer.
    let normalized = std::rc::Rc::new(normalized);
    stream.replace_stream_data(
        std::rc::Rc::clone(&normalized),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );
    if let Some(dict) = stream.as_stream_dict() {
        dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(normalized.len())?),
        )?;
    }
    stream.mark_content_normalization_applied();
    pdf.mark_object_handle_dirty(&stream)?;
    Ok(warning)
}

fn run_qdf(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    preserve_unreferenced: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let mut standard_output = prepare_pdf_standard_output(&output)?;
    let creates_output = standard_output.is_none();
    let mut pdf = open_pdf(&input, repair, password)?;

    // The `qdf` subcommand is the canonical PdfWriter QDF mode.
    let options = WriterOptions {
        qdf: true,
        preserve_unreferenced_objects: preserve_unreferenced,
        ..WriterOptions::default()
    };
    write_with_pdf_writer(
        &mut pdf,
        &output,
        &mut standard_output,
        &options,
        false,
        None,
    )?;
    finish_operation_warnings(&pdf, creates_output)
}

/// `qdf-fix` (qpdf `fix-qdf` equivalent): repair stream `/Length`, xref
/// offsets, `/Size` and `startxref` in a hand-edited QDF file.
///
/// fix_qdf is byte-level: it must operate on the raw file bytes and must
/// NOT reparse/reformat the document, so this reads with `std::fs::read`
/// (not `open_pdf`) and writes the repaired bytes verbatim. No password /
/// no `Pdf` open.
fn run_qdf_fix(input: &std::path::Path, output: &std::path::Path) -> CliResult<()> {
    let bytes = std::fs::read(input)?;
    let fixed = fix_qdf(&bytes)?;
    std::fs::write(output, fixed)?;
    Ok(())
}

fn is_zlib_flate_program(program: &OsStr) -> bool {
    Path::new(program).file_name().is_some_and(|name| {
        name == OsStr::new("zlib-flate") || name == OsStr::new("zlib-flate.exe")
    })
}

fn zlib_flate_usage(usage_name: &str) -> CliResult<()> {
    emit_logger_error(format!(
        "Usage: {usage_name} {{ -uncompress | -compress[=n] }}\n\
If n is specified with -compress, it is a zlib compression level from\n\
1 to 9 where lower numbers are faster and less compressed and higher\n\
numbers are slower and more compressed\n"
    ));
    Err(Box::new(CliExitError {
        code: ExitCode::Errors,
        message: String::new(),
    }))
}

fn zlib_flate_failure(whoami: &str, error: impl std::fmt::Display) -> CliResult<()> {
    emit_logger_error(format!("{whoami}: {error}\n"));
    Err(Box::new(CliExitError {
        code: ExitCode::Errors,
        message: String::new(),
    }))
}

/// Run qpdf's raw zlib stdin/stdout utility over the canonical Flate pipeline.
fn run_zlib_flate(args: &[OsString], whoami: &str, usage_name: &str) -> CliResult<()> {
    if args.len() == 1 && args[0] == OsStr::new("--version") {
        emit_logger_info(format!(
            "{whoami} from qpdf version {}\n",
            flpdf::qpdf_version()
        ));
        return Ok(());
    }
    if args.len() != 1 {
        return zlib_flate_usage(usage_name);
    }

    let Some(mode) = args[0].to_str() else {
        return zlib_flate_usage(usage_name);
    };
    let (action, compression_level) = match mode {
        "-uncompress" => (FlateAction::Inflate, None),
        "-compress" => (FlateAction::Deflate, None),
        value => match value.strip_prefix("-compress=") {
            Some(level) => {
                let level = match qpdf_selector_integer(level) {
                    Ok(level) => level,
                    Err(error) => return zlib_flate_failure(whoami, error),
                };
                (FlateAction::Deflate, Some(level))
            }
            None => return zlib_flate_usage(usage_name),
        },
    };

    let input = std::io::stdin();
    let mut input = input.lock();
    let output = std::io::stdout();
    let mut output = output.lock();
    let mut sink = PlStdioFile::new("stdout", &mut output);
    let flate_result = match compression_level {
        Some(level) => PlFlate::new_with_compression_level("flate", &mut sink, action, level),
        None => PlFlate::new("flate", &mut sink, action),
    };
    let mut flate = match flate_result {
        Ok(flate) => flate,
        // cov:ignore-start: both constructors use a fixed valid output buffer size
        Err(error) => return zlib_flate_failure(whoami, error),
        // cov:ignore-end
    };
    let warned = std::rc::Rc::new(std::cell::Cell::new(false));
    let warned_for_callback = std::rc::Rc::clone(&warned);
    let warning_whoami = whoami.to_owned();
    flate.set_warn_callback(move |message, code| {
        warned_for_callback.set(true);
        emit_logger_error(format!(
            "{warning_whoami}: WARNING: zlib code {code}, msg = {message}\n"
        ));
        Ok(())
    });

    let mut buffer = [0u8; 10_000];
    loop {
        let length = match input.read(&mut buffer) {
            Ok(length) => length,
            // cov:ignore-start: integration processes cannot inject a stdin read error
            Err(error) => {
                drop(flate);
                return zlib_flate_failure(whoami, error);
                // cov:ignore-end
            }
        };
        if length == 0 {
            break;
        }
        if let Err(error) = flate.write(&buffer[..length]) {
            drop(flate);
            return zlib_flate_failure(whoami, error);
        }
    }

    let finish = flate.finish();
    drop(flate);
    // cov:ignore-start: closed stdout/flush failure is owned by the host process boundary
    if let Err(error) = finish {
        return zlib_flate_failure(whoami, error);
    }
    // cov:ignore-end
    if warned.get() {
        return Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        }));
    }
    Ok(())
}

fn reject_same_json_output(input: &Path, output: &Path) -> CliResult<()> {
    reject_same_file(
        input,
        output,
        "input file and output file are the same; choose a different --json-output path",
        "--json-output",
    )
}

/// Reject a job whose main input and output resolve to the same file
/// (qpdf's `QUtil::same_file` guard in `checkConfiguration()`,
/// `QPDFJob.cc:627-630`). qpdf's own message references `--replace-input`,
/// a dedicated escape hatch flpdf does not implement; this instead follows
/// the existing `--json-output` guard's wording (a different output path is
/// the only way out today), the same way that guard already departs from
/// qpdf's exact text for the same reason.
///
/// This is a job-wide guard, not one scoped to `--json-input`/
/// `--update-from-json`: qpdf's check is unconditional, and an ordinary
/// `flpdf in.pdf in.pdf` rewrite is just as destructive (the canonical
/// writer reads from `input` while truncating `output`) as the JSON case
/// that surfaced the gap.
fn reject_same_job_output(input: &Path, output: &Path) -> CliResult<()> {
    reject_same_file(
        input,
        output,
        "input file and output file are the same; use a different output path",
        "output",
    )
}

fn reject_same_file(
    input: &Path,
    output: &Path,
    same_file_message: &str,
    inspect_label: &str,
) -> CliResult<()> {
    match std::fs::metadata(output) {
        Ok(_) => {
            // This is only a non-destructive hint: if inspecting the input
            // fails, the real input open below owns its path-specific error.
            // Output metadata failures remain fail-closed in the next arm.
            if qpdf_same_file(input, output) {
                return Err(same_file_message.into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "unable to inspect {inspect_label} file {}: {error}",
                output.display()
            )
            .into())
        }
    }
    Ok(())
}

fn json_input_open_error(input: &Path, error: std::io::Error) -> Box<dyn std::error::Error> {
    let rendered = error.to_string();
    let message = error
        .raw_os_error()
        .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&rendered);
    format!("open {}: {message}", input.display()).into()
}

fn qpdf_json_input_open_error(input: &Path, error: std::io::Error) -> Box<dyn std::error::Error> {
    let rendered = error.to_string();
    let message = if error.kind() == std::io::ErrorKind::NotFound {
        // qpdf uses its portable POSIX wording for a missing JSON input on
        // every host; Rust exposes the native Windows wording instead.
        "No such file or directory"
    } else {
        error
            .raw_os_error()
            .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
            .unwrap_or(&rendered)
    };
    format!("open {}: {message}", input.display()).into()
}

fn open_verified_json_output(input: &File, output: &Path) -> CliResult<File> {
    let mut output_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(output)?;
    let input_handle = same_file::Handle::from_file(input.try_clone()?)?;
    let output_handle = same_file::Handle::from_file(output_file.try_clone()?)?;
    if input_handle == output_handle {
        return Err(
            "input file and output file are the same; choose a different --json-output path".into(),
        );
    }
    if output_file.metadata()?.file_type().is_file() {
        output_file.set_len(0)?;
        output_file.seek(SeekFrom::Start(0))?;
    }
    Ok(output_file)
}

fn run_dump_object(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    object_ref: &str,
    suppress_warnings: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let object_ref = ObjectRef::parse(object_ref)?;

    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    finish_job_exit_status(job.dump_object(&mut pdf, object_ref)?)
}

#[derive(Debug, Clone, Copy)]
enum ShowObjectSelector {
    Trailer,
    Object(ObjectRef),
    Null,
    NoObject,
}

/// Parse qpdf's `--show-object` selector without changing the shared
/// `ObjectRef::parse` syntax used by the legacy `dump-object` command.
fn parse_show_object_selector(value: &str) -> CliResult<ShowObjectSelector> {
    if value == "trailer" {
        return Ok(ShowObjectSelector::Trailer);
    }

    let (number, generation) = value.split_once(',').unwrap_or((value, "0"));
    let number = qpdf_selector_integer(number)?;
    let generation = if generation.is_empty() {
        0
    } else {
        qpdf_selector_integer(generation)?
    };
    if number <= 0 {
        return Ok(ShowObjectSelector::NoObject);
    }
    if !(0..=i32::from(u16::MAX)).contains(&generation) {
        return Ok(ShowObjectSelector::Null);
    }
    Ok(ShowObjectSelector::Object(ObjectRef::new(
        u32::try_from(number).expect("positive i32 fits u32"),
        u16::try_from(generation).expect("validated u16 generation"),
    )))
}

/// qpdf's `QUtil::string_to_int` uses `strtoll`: it accepts a signed decimal
/// prefix and returns zero when no digits are present. `--show-object` treats
/// object number zero as a no-output selector, so retain that observable
/// leniency instead of routing this option through the stricter shared parser.
fn qpdf_selector_integer(value: &str) -> CliResult<i32> {
    let original = value;
    let value = value.trim_start_matches(|character| {
        matches!(
            character,
            ' ' | '\n' | '\r' | '\t' | '\u{000c}' | '\u{000b}'
        )
    });
    let digits_start = usize::from(matches!(value.as_bytes().first(), Some(b'+') | Some(b'-')));
    let digits_end = digits_start
        + value[digits_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
    if digits_end == digits_start {
        return Ok(0);
    }
    let prefix = &value[..digits_end];
    let parsed = prefix.parse::<i128>().map_err(|_| {
        UsageError::new(format!(
            "overflow/underflow converting {original} to 64-bit integer"
        ))
    })?;
    if !(i128::from(i64::MIN)..=i128::from(i64::MAX)).contains(&parsed) {
        return Err(UsageError::new(format!(
            "overflow/underflow converting {original} to 64-bit integer"
        ))
        .into());
    }
    let parsed = parsed as i64;
    Ok(i32::try_from(parsed).map_err(|_| {
        UsageError::new(format!(
            "integer out of range converting {parsed} from a 8-byte signed type to a 4-byte signed type"
        ))
    })?)
}

/// Show one object through qpdf's canonical object/stream inspection split.
fn run_show_object(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    selector: &str,
    raw_stream_data: bool,
    filtered_stream_data: bool,
    suppress_warnings: bool,
) -> CliResult<()> {
    // qpdf's Config::showObject callback parses the selector during argv
    // parsing, before QPDFJob::run() ever opens the input file, so a usage
    // error in the selector must surface even when no input file is given.
    let selector = parse_show_object_selector(selector)?;
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    let object = match selector {
        ShowObjectSelector::Trailer => pdf.trailer(),
        ShowObjectSelector::Object(object_ref) => pdf.get_object_handle(object_ref),
        ShowObjectSelector::NoObject => {
            return finish_job_exit_status(
                job.inspect(&mut pdf, |_pdf| Ok::<(), flpdf::Error>(()))?,
            );
        }
        ShowObjectSelector::Null => {
            logger_info(b"null\n")?;
            return finish_job_exit_status(
                job.inspect(&mut pdf, |_pdf| Ok::<(), flpdf::Error>(()))?,
            );
        }
    };
    finish_job_exit_status(job.show_object(
        &mut pdf,
        object,
        raw_stream_data,
        filtered_stream_data,
    )?)
}

fn run_show_stream(cmd: ShowStreamCommand) -> CliResult<()> {
    let object_ref = ObjectRef::parse(&cmd.object_ref)?;
    let mut pdf = open_pdf(&cmd.input, cmd.repair, &cmd.password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    finish_job_exit_status(job.show_stream(&mut pdf, object_ref, cmd.raw_stream_data)?)
}

fn run_show_npages(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    finish_job_exit_status(job.show_npages(&mut pdf)?)
}

fn run_show_pages(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    with_images: bool,
    suppress_warnings: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    job.set_with_images(with_images);
    finish_job_exit_status(job.show_pages(&mut pdf)?)
}

fn run_show_xref(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    finish_job_exit_status(job.show_xref(&mut pdf)?)
}

fn run_show_linearization(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    no_warn: bool,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(&input).map_err(|error| open_error_with_file(&input, error.into()))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(no_warn);
    let mut options = pdf_open_options(repair, password)?;
    options.suppress_warnings = no_warn;
    let mut pdf =
        match job.open_with_description(BufReader::new(file), path_description(&input), options) {
            Ok(pdf) => pdf,
            Err(error) => return Err(error_with_file(&input, actionable_password_error(error))),
        };
    finish_job_exit_status(job.show_linearization(&mut pdf)?)
}

// ---------------------------------------------------------------------------
// Encryption inspection subcommands
//
// qpdf exit-code semantics for these subcommands, from
// qpdf/include/qpdf/Constants.h `enum qpdf_exit_code_e`:
//   qpdf_exit_success           = 0
//   qpdf_exit_error             = 2
//   qpdf_exit_is_not_encrypted  = 2   (--is-encrypted / --requires-password)
//   qpdf_exit_correct_password  = 3   (--requires-password)
// and the qpdf manual "Exit Status" / option tables:
//   https://qpdf.readthedocs.io/en/stable/cli.html
//
// The layer-1 `ExitCode` enum is generic (Ok=0, Errors=2, Warnings=3); these
// subcommands reuse the numeric values 2 and 3 with subcommand-specific
// MEANINGS (not "errors"/"warnings"), documented at each construction site.
// ---------------------------------------------------------------------------

/// Outcome of attempting to open a possibly-encrypted document for an
/// inspection subcommand, where (unlike normal processing) a failed
/// password attempt is informative rather than fatal.
enum EncryptionProbe {
    /// Opened successfully. The bool is `Pdf::is_encrypted()`.
    Opened { encrypted: bool },
    /// The file is encrypted but the supplied/empty password did not
    /// authenticate (`BadPassword`). qpdf can still report "encrypted" /
    /// "password required" without authenticating, so this is a normal
    /// classification here, not an error.
    EncryptedAuthFailed,
}

/// Open `input` for a read-only encryption inspection (`is-encrypted` /
/// `requires-password`), treating a wrong/empty password (`BadPassword`) as
/// "the file is encrypted but we could not authenticate" rather than a hard
/// error. This mirrors qpdf's ability to answer these queries for
/// password-protected files without the password.
///
/// qpdf applies its weak-crypto refusal to write/transform operations, not to
/// these read-only inspections. Authentication still runs first, so a wrong
/// password yields `BadPassword` exactly as before.
fn probe_encryption(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<EncryptionProbe> {
    let file = File::open(input).map_err(|error| open_error_with_file(input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    match job.open_with_description(BufReader::new(file), path_description(input), options) {
        Ok(mut pdf) => {
            pdf.root_handle()
                .map_err(|error| error_with_file(input, actionable_password_error(error)))?;
            Ok(EncryptionProbe::Opened {
                encrypted: pdf.is_encrypted(),
            })
        }
        // A wrong/empty password: the document is definitely encrypted, we
        // just have not authenticated it. qpdf treats this as "encrypted,
        // password required".
        Err(error) if is_bad_password_error(&error) => Ok(EncryptionProbe::EncryptedAuthFailed),
        Err(other) => Err(error_with_file(input, actionable_password_error(other))),
    }
}

fn is_bad_password_error(error: &flpdf::Error) -> bool {
    let source = error.open_failure().map_or(error, |(source, _)| source);
    matches!(
        source,
        flpdf::Error::Encrypted(flpdf::EncryptedError::BadPassword)
    )
}

/// `--empty --is-encrypted`/`--empty --requires-password`: silently exit 2.
///
/// qpdf's `createQPDF` still builds an empty document for `--empty` before
/// the encryption-status early return (`QPDFJob.cc:429-456,535-557`); an
/// empty document is necessarily unencrypted, so both flags exit 2 without
/// opening any file.
fn run_empty_document_encryption_status() -> CliResult<()> {
    Err(Box::new(CliExitError {
        code: ExitCode::Errors,
        message: String::new(),
    }))
}

/// `is-encrypted FILE`: exit 0 if encrypted, exit 2 if not.
///
/// qpdf `--is-encrypted` (qpdf manual): exit 0 = encrypted, exit 2 = not
/// encrypted (`qpdf_exit_is_not_encrypted = 2`). No required stdout. A
/// supplied password is still forwarded because qpdf passes its configured
/// password to `processFile`; it does not change the classification of an
/// encrypted input, but password-file parsing and diagnostics remain visible.
fn run_is_encrypted(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<()> {
    let encrypted = match probe_encryption(input, repair, password, suppress_warnings)? {
        EncryptionProbe::Opened { encrypted } => encrypted,
        EncryptionProbe::EncryptedAuthFailed => true,
    };
    if encrypted {
        Ok(()) // exit 0 — file is encrypted.
    } else {
        // Exit 2 — NOT an error here: qpdf_exit_is_not_encrypted = 2 means
        // "file is not encrypted" for --is-encrypted specifically.
        Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: String::new(),
        }))
    }
}

/// `requires-password FILE [--password ...]`: qpdf `--requires-password`.
///
/// Exit codes (qpdf manual + Constants.h):
///   2 = not encrypted              (qpdf_exit_is_not_encrypted)
///   3 = encrypted, supplied/empty password opens it
///       (qpdf_exit_correct_password — no further password required)
///   0 = encrypted, a password other than the one supplied is required
///
/// Weak-crypto (RC4 / R=5) files are answered purely on the password, matching
/// qpdf: a correct password yields 3 and a wrong/absent one yields 0, with no
/// `--allow-weak-crypto` opt-in required (see `probe_encryption`).
fn run_requires_password(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<()> {
    match probe_encryption(input, repair, password, suppress_warnings)? {
        EncryptionProbe::Opened { encrypted: false } => {
            // Exit 2 — qpdf_exit_is_not_encrypted: file is not encrypted.
            Err(Box::new(CliExitError {
                code: ExitCode::Errors,
                message: String::new(),
            }))
        }
        EncryptionProbe::Opened { encrypted: true } => {
            // Exit 3 — qpdf_exit_correct_password: encrypted, but the
            // supplied/empty password opened it, so no other password is
            // required. Reuses ExitCode::Warnings's numeric 3 with this
            // subcommand-specific meaning.
            Err(Box::new(CliExitError {
                code: ExitCode::Warnings,
                message: String::new(),
            }))
        }
        EncryptionProbe::EncryptedAuthFailed => {
            // Exit 0 — encrypted and a password OTHER than the one supplied
            // is required (qpdf manual: "a password, other than as
            // supplied, is required").
            Ok(())
        }
    }
}

/// `show-encryption-key FILE [--password ...]`: qpdf `--show-encryption-key`.
///
/// Authenticate, then print the derived file encryption key as lowercase
/// hex. Not encrypted or wrong password → error (exit 2), matching qpdf
/// (which errors when it cannot derive the key). Weak-crypto (RC4 / R=5) files
/// are inspectable with the correct password and no `--allow-weak-crypto`,
/// matching qpdf's read-only treatment (see [`open_pdf_for_inspection`]).
fn run_show_encryption_key(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let pdf = open_pdf_for_inspection(input, repair, password)?;
    match pdf.encryption_file_key() {
        Some(key) => {
            logger_info(format!("{}\n", hex_lower(&key)))?;
            finish_operation_warnings(&pdf, false)
        }
        None if pdf.is_encrypted() => Err("invalid password".into()),
        None => Err("file is not encrypted; no encryption key to show".into()),
    }
}

/// `show-encryption FILE [--password ...]`: qpdf `--show-encryption`.
///
/// The report is the qpdf `QPDFJob::showEncryption` format. Weak-crypto
/// (RC4 / R=5) files are inspectable with the correct password and no
/// `--allow-weak-crypto`, matching qpdf's read-only treatment.
///
/// Opens through its own job (rather than the shared
/// [`open_pdf_for_inspection`] helper) so `--no-warn` reaches
/// `QPDFJob::set_suppress_warnings`, matching `run_check`/
/// `run_check_linearization`'s pattern.
fn run_show_encryption(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    no_warn: bool,
    show_encryption_key: bool,
) -> CliResult<()> {
    let file = File::open(input).map_err(|error| open_error_with_file(input, error.into()))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(no_warn);
    job.set_show_encryption_key(show_encryption_key);
    let mut options = pdf_open_options(repair, password)?;
    // qpdf's `--no-warn` drops open-time repair diagnostics entirely for
    // `--show-encryption` (no deferred replay, unlike `--check`'s report
    // body); verified against `qpdf --no-warn --show-encryption` on a
    // damaged fixture, which prints no WARNING lines at all. Without this,
    // `job.set_suppress_warnings(no_warn)` above only gates the trailing
    // "operation succeeded with warnings" summary while the live
    // `WARNING: ...` lines from `Pdf::open_for_encryption_inspection` still
    // print unconditionally.
    options.suppress_warnings = no_warn;
    let mut pdf = job
        .open_for_encryption_inspection_with_description(
            BufReader::new(file),
            path_description(input),
            options,
        )
        .map_err(|error| error_with_file(input, actionable_password_error(error)))?;
    finish_show_encryption(&mut job, &mut pdf, password.password_is_hex_key)
}

/// Emit the qpdf-verbatim encryption report through the job-owned renderer,
/// then complete the same warning/exit-status boundary as other inspections.
/// The document may have come from either a file-backed input or a JSON update,
/// so the already-open document must be passed through unchanged.
fn finish_show_encryption<R: Read + Seek>(
    job: &mut QPDFJob,
    pdf: &mut Pdf<R>,
    password_is_hex_key: bool,
) -> CliResult<()> {
    job.show_encryption(pdf, password_is_hex_key)?;
    job.record_document_warnings(pdf);
    finish_job_exit_status(job.complete(false)?)
}

/// Lowercase hex encoding (qpdf `--show-encryption-key` format).
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Main-input variants accepted by the qpdf-shaped job boundary.
///
/// The JSON importer owns a `Pdf<Cursor<Vec<u8>>>` because its rootless seed
/// is an in-memory PDF. Ordinary files keep the existing buffered-file reader.
/// The enum is intentionally confined to this CLI boundary; every downstream
/// consumer still receives its normal generic `Pdf<R>` and therefore uses the
/// canonical resolver/object-handle route.
enum JobPdf {
    File(Pdf<BufReader<File>>),
    Json(Pdf<Cursor<Vec<u8>>>),
}

struct JsonJobRuntime<'a> {
    input_identity: &'a File,
    standard_output: &'a mut Option<PipelineWriter>,
    job: &'a mut QPDFJob,
}

fn apply_json_update<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    update_from_json: Option<&Path>,
) -> CliResult<()> {
    if let Some(path) = update_from_json {
        let source = File::open(path).map_err(|error| qpdf_json_input_open_error(path, error))?;
        pdf.update_from_json(source, path_description(path))
            .map_err(|error| json_error_with_file(path, Box::new(error)))?;
    }
    Ok(())
}

fn apply_json_update_with_job<R: Read + Seek + 'static>(
    job: &mut QPDFJob,
    pdf: &mut Pdf<R>,
    update_from_json: Option<&Path>,
) -> CliResult<()> {
    if let Some(path) = update_from_json {
        let source = File::open(path).map_err(|error| qpdf_json_input_open_error(path, error))?;
        job.update_from_json(pdf, source, path_description(path))
            .map_err(|error| json_error_with_file(path, Box::new(error)))?;
    }
    Ok(())
}

/// Open the main qpdf job input and apply `--update-from-json` at the same
/// point qpdf's `QPDFJob::createQPDF` does: immediately after input creation,
/// before page specifications, rotations, overlays, or serialization.
/// `check_inspection` applies `run_check`'s warning-aggregation policy (see
/// [`open_pdf_for_check_inspection`]) to the
/// non-`--json-input` (`--update-from-json` only) branch. It has no effect
/// on the `--json-input` branch: [`Pdf::create_from_json`] always seeds from
/// the fixed, never-encrypted rootless bootstrap document, so this policy
/// only matters for a real encrypted PDF opened through
/// `--update-from-json`.
fn open_job_pdf(
    input: &Path,
    repair: bool,
    password: &PasswordArgs,
    json_input: bool,
    update_from_json: Option<&Path>,
    check_inspection: bool,
    suppress_warnings: bool,
) -> CliResult<JobPdf> {
    if json_input {
        Ok(JobPdf::Json(open_json_pdf(
            input,
            update_from_json,
            suppress_warnings,
        )?))
    } else {
        let mut pdf = if check_inspection {
            open_pdf_for_check_inspection(&input.to_path_buf(), repair, password)?
        } else {
            open_pdf_with_suppression(&input.to_path_buf(), repair, password, suppress_warnings)?
        };
        apply_json_update(&mut pdf, update_from_json)?;
        Ok(JobPdf::File(pdf))
    }
}

fn open_json_pdf(
    input: &Path,
    update_from_json: Option<&Path>,
    suppress_warnings: bool,
) -> CliResult<Pdf<Cursor<Vec<u8>>>> {
    let source = File::open(input).map_err(|error| qpdf_json_input_open_error(input, error))?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    let mut pdf = job
        .create_from_json(source, path_description(input))
        .map_err(|error| json_error_with_file(input, Box::new(error)))?;
    apply_json_update(&mut pdf, update_from_json)?;
    Ok(pdf)
}

fn open_pdf(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, false)
}

fn open_pdf_with_suppression(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, suppress_warnings)
}

/// Open a secondary page-spec source through the same file-backed reader
/// boundary as qpdf's `QPDFJob::handlePageSpecs`.
fn open_page_source(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    stay_open: bool,
    suppress_warnings: bool,
) -> CliResult<Pdf<Box<dyn flpdf::ReadSeek>>> {
    let mut options = pdf_open_options(repair, password)?;
    configure_document_logger(&mut options, input);
    options.suppress_warnings = suppress_warnings;
    let mut pdf = Pdf::<Box<dyn flpdf::ReadSeek>>::open_file_with_options(input, options)
        .map_err(|error| open_error_with_file(input, actionable_password_error(error)))?;
    pdf.root_handle()
        .map_err(|error| error_with_file(input, actionable_password_error(error)))?;
    pdf.set_input_source_stay_open(stay_open);
    Ok(pdf)
}

fn open_pdf_from_file(
    input: &Path,
    file: File,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_file_impl(input, file, repair, password, suppress_warnings, false)
}

/// Open for the read-only encryption inspections (`show-encryption`,
/// `show-encryption-key`).
///
/// qpdf treats these as read-only inspections rather than a write policy: it
/// derives and prints the key / encryption block for a weak file with the
/// correct password and emits no weak-crypto warning (verified qpdf 11.9.0).
/// A wrong password retains qpdf's parsed encryption state so show-encryption
/// can report it rather than failing before the report.
fn open_pdf_for_inspection(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    let file = File::open(input).map_err(|error| open_error_with_file(input, error.into()))?;
    open_pdf_file_impl(input, file, repair, password, false, true)
}

/// Open for `--update-from-json --check`'s generic job-inspection route.
///
/// Mirrors `run_check`'s own inspection policy, plus `suppress_warnings` so
/// open/update-time repair diagnostics are collected
/// rather than delivered live, since the qpdf-shaped job check re-emits the
/// same diagnostics from the document after its check banner -- without
/// this, a `--repair`-triggered warning prints twice). `--show-npages`/
/// `--show-pages` do not need either policy: like their non-JSON siblings
/// `run_show_npages`/`run_show_pages`, they use the plain [`open_pdf`]
/// path via [`open_job_pdf`]'s `check_inspection` parameter.
fn open_pdf_for_check_inspection(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, true)
}

fn open_pdf_impl(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    let file = File::open(input).map_err(|error| open_error_with_file(input, error.into()))?;
    open_pdf_file_impl(input, file, repair, password, suppress_warnings, false)
}

fn open_pdf_file_impl(
    input: &Path,
    file: File,
    repair: bool,
    password: &PasswordArgs,
    suppress_warnings: bool,
    encryption_inspection: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    let mut options = pdf_open_options(repair, password)?;
    if suppress_warnings {
        options.suppress_warnings = true;
    }
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    let pdf = if encryption_inspection {
        job.open_for_encryption_inspection_with_description(
            BufReader::new(file),
            path_description(input),
            options,
        )
    } else {
        job.open_with_description(BufReader::new(file), path_description(input), options)
    }
    .map_err(|error| error_with_file(input, actionable_password_error(error)))?;
    Ok(pdf)
}

fn pdf_open_options(repair: bool, password: &PasswordArgs) -> CliResult<PdfOpenOptions> {
    let password_bytes = if let Some(password) = password.password_bytes() {
        password
    } else if let Some(path) = &password.password_file {
        read_password_file(path)?
    } else {
        Vec::new()
    };

    Ok(pdf_open_options_with_password_bytes(
        repair,
        password,
        password_bytes,
    ))
}

/// Read qpdf's `--password-file` value as raw bytes.
///
/// qpdf 11.9.0 calls `QUtil::read_lines_from_file` and uses only `lines.front()`
/// (`QUtil.cc:1231-1286`, `QPDFJob_config.cc:661-679`). The line reader splits
/// on `\n`, removes only a preceding `\r`, retains arbitrary non-UTF-8 bytes,
/// and does not create an extra line for a final newline. qpdf also treats
/// `-` as stdin and warns about every line after the first; that warning is a
/// configuration diagnostic, so it is emitted even for `--no-warn`.
fn read_password_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let bytes = if path == Path::new("-") {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(path)?
    };

    let first_newline = bytes.iter().position(|&byte| byte == b'\n');
    let first_line_len = first_newline.unwrap_or(bytes.len());
    let mut first_line = bytes[..first_line_len].to_vec();
    if first_line.ends_with(b"\r") {
        first_line.pop();
    }
    if first_newline.is_some_and(|index| index + 1 < bytes.len()) {
        emit_logger_error(format!(
            "{}: WARNING: all but the first line of the password file are ignored\n",
            progname()
        ));
    }
    Ok(first_line)
}

fn pdf_open_options_with_password_bytes(
    repair: bool,
    password: &PasswordArgs,
    password_bytes: Vec<u8>,
) -> PdfOpenOptions {
    let recovery = password.recovery;
    PdfOpenOptions {
        // qpdf's recovery permission is enabled on the document by default;
        // --suppress-recovery is the explicit opt-out.
        repair: !recovery.suppress_recovery && (repair || PdfOpenOptions::default().repair),
        ignore_xref_streams: recovery.ignore_xref_streams,
        password: password_bytes,
        password_mode: password.password_mode.into(),
        suppress_password_recovery: password.suppress_password_recovery,
        password_is_hex_key: password.password_is_hex_key,
        verbose: password.verbose,
        message_prefix: progname().into_bytes(),
        ..PdfOpenOptions::default()
    }
}

fn cli_logger() -> QPDFLogger {
    static LOGGER: OnceLock<QPDFLogger> = OnceLock::new();
    LOGGER.get_or_init(QPDFLogger::create).clone()
}

fn standard_save_writer() -> CliResult<PipelineWriter> {
    standard_save_writer_for(&cli_logger())
}

fn standard_save_writer_for(logger: &QPDFLogger) -> CliResult<PipelineWriter> {
    logger.save_to_standard_output(true)?;
    Ok(PipelineWriter {
        pipeline: logger.get_save()?,
    })
}

fn prepare_pdf_standard_output(output: &Path) -> CliResult<Option<PipelineWriter>> {
    if output.as_os_str() == "-" {
        Ok(Some(standard_save_writer()?))
    } else {
        Ok(None)
    }
}

// cov:ignore-start: exercised by page-operation stdout subprocess integration tests
fn prepare_page_operation_standard_output(
    output: &Path,
    page_ops: &PageOpArgs,
) -> CliResult<Option<PipelineWriter>> {
    if output.as_os_str() == "-" && split_pages_active(page_ops.split_pages.as_deref()) {
        return Err("--split-pages may not be used when writing to standard output".into());
    }
    prepare_pdf_standard_output(output)
}
// cov:ignore-end

fn logger_info(data: impl AsRef<[u8]>) -> CliResult<()> {
    cli_logger().info(data)?;
    Ok(())
}

fn logger_warn(data: impl AsRef<[u8]>) -> CliResult<()> {
    cli_logger().warn(data)?;
    Ok(())
}

fn emit_logger_info(data: impl AsRef<[u8]>) {
    let _ = logger_info(data);
}

fn emit_logger_error(data: impl AsRef<[u8]>) {
    if let Err(error) = cli_logger().error(data) {
        eprintln!("flpdf: unable to write diagnostic: {error}"); // cov:ignore: last-resort path after the standard error sink itself fails
    }
}

fn configure_document_logger(options: &mut PdfOpenOptions, input: &Path) {
    options.logger = Some(cli_logger());
    options.description = path_description(input);
}

#[cfg(unix)]
fn path_description(input: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    input.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_description(input: &Path) -> Vec<u8> {
    input.to_string_lossy().into_owned().into_bytes()
}

/// Build qpdf's `<prefix>: wrote file <output>` line with the output name's
/// raw bytes (`QPDFJob.cc:3059-3062`); `Path::display()` would replace
/// non-UTF-8 bytes with U+FFFD.
fn wrote_file_message(prefix: &str, output: &Path) -> Vec<u8> {
    let mut message = format!("{prefix}: wrote file ").into_bytes();
    message.extend_from_slice(&path_description(output));
    message.push(b'\n');
    message
}

/// Program name used in qpdf-parity diagnostic prefixes.
///
/// `FLPDF_PROGNAME` overrides the default so the qpdf qtest harness shim can
/// present flpdf as `qpdf`; unset or empty, the prefix is always `flpdf`.
fn progname() -> String {
    std::env::var("FLPDF_PROGNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "flpdf".to_string())
}

/// Render the `<file>` / `<file> (offset N)` location part shared by the
/// qpdf-shaped diagnostic lines (qpdf 11.9.0 observed format; qpdf
/// suppresses the offset display when it is unknown).
fn diagnostic_location(input: &Path, offset: Option<u64>) -> String {
    match offset {
        Some(offset) => format!("{} (offset {offset})", input.display()),
        None => input.display().to_string(),
    }
}

/// Finish a successful operation after all requested output has been emitted.
/// qpdf aggregates warnings from both open-time and lazy object resolution;
/// the summary shape depends on whether this route created a PDF output.
fn finish_operation_warnings<R: Read + Seek>(pdf: &Pdf<R>, creates_output: bool) -> CliResult<()> {
    finish_operation_warnings_with_prior(pdf, creates_output, false)
}

/// Complete the warning boundary while retaining warnings observed in source
/// documents that were merged into a fresh output PDF.
fn finish_operation_warnings_with_prior<R: Read + Seek>(
    pdf: &Pdf<R>,
    creates_output: bool,
    prior_warnings: bool,
) -> CliResult<()> {
    finish_warning_state(
        prior_warnings || !pdf.repair_diagnostics().entries().is_empty(),
        creates_output,
        pdf.suppress_warnings(),
    )
}

fn finish_job_exit_status(status: JobExitCode) -> CliResult<()> {
    match status {
        JobExitCode::Success => Ok(()),
        JobExitCode::Error => Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: String::new(),
        })),
        JobExitCode::Warning => Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        })),
    }
}

/// Map the qpdf-shaped check consumer's result to the CLI exit contract.
/// Diagnostics have already been emitted by [`QPDFJob::check`]; this adapter
/// only selects qpdf's error (2), warning (3), or success (0) process status.
fn finish_check_job(result: std::result::Result<JobExitCode, CheckError>) -> CliResult<()> {
    match result {
        Ok(status) => finish_job_exit_status(status),
        Err(CheckError::ErrorsDetected) => Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: String::new(),
        })),
        Err(CheckError::Operation(error)) => Err(Box::new(error)),
    }
}

fn finish_warning_state(has_warnings: bool, creates_output: bool, no_warn: bool) -> CliResult<()> {
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(no_warn);
    if has_warnings {
        job.record_warnings();
    }

    match job.complete(creates_output)? {
        JobExitCode::Success => Ok(()),
        JobExitCode::Error => Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: String::new(),
        })),
        JobExitCode::Warning => Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        })),
    }
}

fn emit_content_normalization_warnings(
    input: &Path,
    warning: ContentNormalizationWarning,
) -> CliResult<()> {
    let location = diagnostic_location(input, warning.parsed_offset);
    let mut message =
        format!("WARNING: {location}: content normalization encountered bad tokens\n");
    if warning.last_token_was_bad {
        message.push_str(&format!(
            "WARNING: {location}: normalized content ended with a bad token; \
             you may be able to resolve this by coalescing content streams in \
             combination with normalizing content. From the command line, \
             specify --coalesce-contents\n"
        ));
    }
    message.push_str(&format!(
        "WARNING: {location}: Resulting stream data may be corrupted but is may \
         still useful for manual inspection. For more information on this \
         warning, search for content normalization in the manual.\n"
    ));
    logger_warn(message)
}

fn finish_rewrite_warnings<R: Read + Seek>(
    input: &Path,
    pdf: &Pdf<R>,
    normalization_warnings: &[ContentNormalizationWarning],
    creates_output: bool,
    no_warn: bool,
) -> CliResult<()> {
    // qpdf retains open-time warnings in the document warning collection and
    // emits the final summary after the output writer completes. Include the
    // full collection here, not only warnings added after this route opened
    // the document.
    let has_repair_warnings = !pdf.repair_diagnostics().entries().is_empty();
    // qpdf reports these through `QPDF::warn`, which records the warning but
    // skips the text under `--no-warn` (`QPDF_Stream.cc:625`, `QPDF.cc:491`);
    // the exit status still reflects it.
    if !no_warn {
        for &warning in normalization_warnings {
            emit_content_normalization_warnings(input, warning)?;
        }
    }
    if normalization_warnings.is_empty() && !has_repair_warnings {
        return Ok(());
    }
    finish_warning_state(true, creates_output, no_warn)
}

/// Prefix a fatal post-open error with the input path so main() renders the
/// qpdf shape `<progname>: <file>: <msg>` for path-scoped failures.
///
/// This type-erases the error; do not downcast the result.
fn error_with_file(input: &Path, error: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    Box::new(CliPathError {
        path: path_description(input),
        operation: None,
        message: error.to_string(),
        source: error,
    })
}

/// Prefix a fatal input-open error with qpdf's `open ` operation and normalize
/// Rust's platform-specific I/O rendering. This mirrors `QUtil::safe_fopen`
/// (`libqpdf/QUtil.cc:512-515`) and `QPDFSystemError::createWhat`
/// (`libqpdf/QPDFSystemError.cc:13-29`). The path is kept as raw bytes so the
/// CLI preserves non-UTF-8 Unix arguments just as qpdf's `std::string`
/// boundary does.
fn open_error_with_file(
    input: &Path,
    error: Box<dyn std::error::Error>,
) -> Box<dyn std::error::Error> {
    let io_error = error.downcast_ref::<std::io::Error>().or_else(|| {
        match error.downcast_ref::<flpdf::Error>() {
            Some(flpdf::Error::FileIo {
                operation: "open",
                source,
                ..
            }) => Some(source),
            _ => None,
        }
    });
    let Some(io_error) = io_error else {
        return error_with_file(input, error);
    };
    let message = qpdf_open_io_error_message(io_error);
    Box::new(CliPathError {
        path: path_description(input),
        operation: Some("open"),
        message,
        source: error,
    })
}

/// Match qpdf's `QPDFSystemError::createWhat`: use the C-runtime wording and
/// omit Rust's numeric `(os error N)` suffix. qpdf uses the portable
/// not-found wording on every supported host.
fn qpdf_open_io_error_message(error: &std::io::Error) -> String {
    let message = match error.kind() {
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
    let rendered = error.to_string();
    error
        .raw_os_error()
        .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&rendered)
        .to_owned()
}

fn json_error_with_file(
    input: &Path,
    error: Box<dyn std::error::Error>,
) -> Box<dyn std::error::Error> {
    let path = path_description(input);
    let lossy_path = String::from_utf8_lossy(&path);
    let message = error
        .to_string()
        .strip_prefix(&format!("{lossy_path}: "))
        .map_or_else(|| error.to_string(), str::to_owned);
    Box::new(CliPathError {
        path,
        operation: None,
        message,
        source: error,
    })
}

fn actionable_password_error(error: flpdf::Error) -> Box<dyn std::error::Error> {
    if is_bad_password_error(&error) {
        return "invalid password".into();
    }
    error.into()
}

// ── Attachment helpers ──────────────────────────────────────────

/// Parse and retain the PDF timestamp syntax accepted by qpdf's
/// `QUtil::pdf_time_to_qpdf_time`: `D:YYYYMMDDHHmmSS`, optionally followed by
/// `Z` or a signed `HH'MM'` offset. The raw value is retained so qpdf's exact
/// timezone spelling is written to `/Params`.
fn parse_pdf_date_arg(s: &str) -> CliResult<Vec<u8>> {
    let s = s
        .strip_prefix("D:")
        .ok_or_else(|| format!("invalid PDF date {s:?}: must start with D:"))?;
    // Validate the required 14-character body is ASCII digits BEFORE slicing
    // by byte offsets: a multibyte value (e.g. fullwidth digits
    // `D:２０２４…`) would otherwise panic on a non-char-boundary slice.
    if s.len() < 14 || !s.as_bytes()[..14].iter().all(u8::is_ascii_digit) {
        return Err(format!(
            "invalid PDF date D:{s:?}: need at least 14 ASCII digits (YYYYMMDDHHmmSS)"
        )
        .into());
    }
    let suffix = &s[14..];
    let valid_suffix = match suffix.as_bytes() {
        b"" | b"Z" => true,
        [sign, hour_tens, hour_ones, b'\'', minute_tens, minute_ones, b'\''] => {
            matches!(sign, b'+' | b'-')
                && hour_tens.is_ascii_digit()
                && hour_ones.is_ascii_digit()
                && minute_tens.is_ascii_digit()
                && minute_ones.is_ascii_digit()
        }
        _ => false,
    };
    if !valid_suffix {
        return Err(format!("invalid PDF date D:{s:?}: timezone must be Z or [+|-]HH'MM'").into());
    }
    Ok(format!("D:{s}").into_bytes())
}

/// Parsed sub-flags for the `--add-attachment FILE [sub-flags] --` segment.
struct AddAttachmentArgs {
    /// Path to the file whose bytes will be embedded.
    file: PathBuf,
    /// Name-tree key (default: basename of `file`).
    key: Option<Vec<u8>>,
    /// Filename stored in both `/F` and `/UF` by qpdf's `createFileSpec` path.
    filename: Option<Vec<u8>>,
    /// MIME type for `/EmbeddedFile /Subtype`.
    mimetype: Option<Vec<u8>>,
    /// Human-readable description for `/Filespec /Desc`.
    description: Option<Vec<u8>>,
    /// `/Params /CreationDate` as the raw PDF date string.
    creation_date: Option<Vec<u8>>,
    /// `/Params /ModDate` as the raw PDF date string.
    mod_date: Option<Vec<u8>>,
    /// When true, replace an existing attachment with the same key.
    replace: bool,
}

/// Parse the raw token Vec captured by `--add-attachment … --` into
/// [`AddAttachmentArgs`].
///
/// Expected token order: FILE [--key=K] [--filename=F] [--mimetype=M]
/// [--description=D] [--creationdate=D] [--moddate=D] [--replace]
fn parse_add_attachment_segment<T: RawCliArg>(tokens: Vec<T>) -> CliResult<AddAttachmentArgs> {
    let mut iter = tokens.into_iter();
    let file: PathBuf = iter
        .next()
        .ok_or("--add-attachment: missing FILE argument")?
        .os_string()
        .into();

    let mut key: Option<Vec<u8>> = None;
    let mut filename: Option<Vec<u8>> = None;
    let mut mimetype: Option<Vec<u8>> = None;
    let mut description: Option<Vec<u8>> = None;
    let mut creation_date: Option<Vec<u8>> = None;
    let mut mod_date: Option<Vec<u8>> = None;
    let mut replace = false;

    for token in iter {
        let token_bytes = token.raw_bytes();
        if let Some(v) = token_bytes.strip_prefix(b"--key=") {
            key = Some(v.to_vec());
        } else if let Some(v) = token_bytes.strip_prefix(b"--filename=") {
            filename = Some(v.to_vec());
        } else if let Some(v) = token_bytes.strip_prefix(b"--mimetype=") {
            let bytes = v.to_vec();
            if !bytes.contains(&b'/') {
                return Err("mime type should be specified as type/subtype".into());
            }
            mimetype = Some(bytes);
        } else if let Some(v) = token_bytes.strip_prefix(b"--description=") {
            description = Some(v.to_vec());
        } else if let Some(v) = token_bytes.strip_prefix(b"--creationdate=") {
            let value = std::str::from_utf8(v)
                .map_err(|_| "--add-attachment --creationdate must be valid UTF-8")?;
            creation_date = Some(parse_pdf_date_arg(value)?);
        } else if let Some(v) = token_bytes.strip_prefix(b"--moddate=") {
            let value = std::str::from_utf8(v)
                .map_err(|_| "--add-attachment --moddate must be valid UTF-8")?;
            mod_date = Some(parse_pdf_date_arg(value)?);
        } else if token_bytes == b"--replace" {
            replace = true;
        } else {
            return Err(format!(
                "--add-attachment: unknown sub-flag or unexpected token {:?}",
                token.os_string()
            )
            .into());
        }
    }

    Ok(AddAttachmentArgs {
        file,
        key,
        filename,
        mimetype,
        description,
        creation_date,
        mod_date,
        replace,
    })
}

/// Parsed sub-flags for the `--copy-attachments-from FILE [sub-flags] --` segment.
struct CopyAttachmentsArgs {
    /// Source PDF path.
    file: PathBuf,
    /// Password for the source PDF (empty = no password).
    password: Vec<u8>,
    /// Prefix prepended to each copied key.
    prefix: Option<Vec<u8>>,
}

/// Parse the raw token Vec captured by `--copy-attachments-from … --` into
/// [`CopyAttachmentsArgs`].
///
/// Expected token order: FILE [--password=P] [--prefix=X]
fn parse_copy_attachments_segment<T: RawCliArg>(tokens: Vec<T>) -> CliResult<CopyAttachmentsArgs> {
    let mut iter = tokens.into_iter();
    let file: PathBuf = iter
        .next()
        .ok_or("--copy-attachments-from: missing FILE argument")?
        .os_string()
        .into();

    let mut password: Vec<u8> = Vec::new();
    let mut prefix: Option<Vec<u8>> = None;

    for token in iter {
        let token_bytes = token.raw_bytes();
        if let Some(v) = token_bytes.strip_prefix(b"--password=") {
            password = v.to_vec();
        } else if let Some(v) = token_bytes.strip_prefix(b"--prefix=") {
            prefix = Some(v.to_vec());
        } else {
            return Err(format!(
                "--copy-attachments-from: unknown sub-flag or unexpected token {:?}",
                token.os_string()
            )
            .into());
        }
    }

    Ok(CopyAttachmentsArgs {
        file,
        password,
        prefix,
    })
}

/// Return the basename of `path` as raw bytes, or error if the path has no
/// valid file name component.
fn path_basename(path: &std::path::Path) -> CliResult<Vec<u8>> {
    path.file_name()
        .ok_or_else(|| format!("cannot determine filename from path {:?}", path).into())
        .map(arg_parser::os_bytes)
}

/// Append the existing attachment diagnostic's quoted key without converting
/// arbitrary argv bytes through UTF-8. Printable non-UTF-8 bytes stay raw;
/// quotes, backslashes, and ASCII control bytes retain the old debug-style
/// quoting convention in an unambiguous form.
fn append_debug_quoted_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.push(b'"');
    for &byte in bytes {
        match byte {
            b'"' => output.extend_from_slice(b"\\\""),
            b'\\' => output.extend_from_slice(b"\\\\"),
            b'\n' => output.extend_from_slice(b"\\n"),
            b'\r' => output.extend_from_slice(b"\\r"),
            b'\t' => output.extend_from_slice(b"\\t"),
            byte if byte.is_ascii_control() => {
                output.extend_from_slice(format!("\\x{byte:02x}").as_bytes())
            }
            byte => output.push(byte),
        }
    }
    output.push(b'"');
}

/// `--add-attachment FILE [sub-flags] -- output.pdf`
#[allow(clippy::too_many_arguments)]
fn run_add_attachment(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    segments: Vec<Vec<Vec<u8>>>,
    verbose: bool,
    suppress_warnings: bool,
    remove_restrictions: bool,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    writer_options: WriterOptions,
) -> CliResult<()> {
    let input = input.ok_or("--add-attachment: missing input PDF")?;
    let output = output.ok_or("--add-attachment: missing output PDF")?;
    let attachment_options = segments
        .into_iter()
        .map(|tokens| {
            let args = parse_add_attachment_segment(tokens)?;
            let basename = path_basename(&args.file)?;
            let key = args.key.unwrap_or_else(|| basename.clone());
            let filename = args.filename.unwrap_or_else(|| basename.clone());
            Ok(AttachmentAddOptions {
                path: args.file,
                key,
                filename,
                mimetype: args.mimetype,
                description: args.description,
                creation_date: args.creation_date,
                modification_date: args.mod_date,
                replace: args.replace,
                verbose,
            })
        })
        .collect::<CliResult<Vec<_>>>()?;

    // Reserve standard output before opening the input, like qpdf's
    // `saveToStandardOutput` (`QPDFJob.cc:625`), so open-time `--verbose`
    // info lines go to stderr when the PDF goes to stdout.
    let mut standard_output = prepare_pdf_standard_output(&output)?;

    let file = File::open(&input).map_err(|error| open_error_with_file(&input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    let mut pdf = job
        .open_with_description(BufReader::new(file), path_description(&input), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
    pdf.set_suppress_warnings(suppress_warnings);

    if remove_restrictions {
        AcroFormDocumentHelper::new(&mut pdf)?.disable_digital_signatures()?;
    }
    job.add_attachments(&mut pdf, &attachment_options)?;

    // qpdf's writer applies content normalization after all transformations
    // have updated the document graph. The direct writer API requires the
    // equivalent page-content prepass before it emits the final PDF.
    let normalization_warnings = if writer_options.content_normalization {
        normalize_page_contents(&mut pdf)?
    } else {
        Vec::new()
    };
    write_with_pdf_writer(
        &mut pdf,
        &output,
        &mut standard_output,
        &writer_options,
        linearize,
        linearize_pass1,
    )?;
    if verbose && output.as_os_str() != "-" {
        job.logger()
            .info(wrote_file_message(&progname(), &output))?;
    }
    if !suppress_warnings {
        for &warning in &normalization_warnings {
            emit_content_normalization_warnings(&input, warning)?;
        }
    }
    if !normalization_warnings.is_empty() {
        job.record_warnings();
    }
    job.record_document_warnings(&pdf);
    finish_job_exit_status(job.complete(true)?)
}

/// `--remove-attachment KEY [input] [output]`
#[allow(clippy::too_many_arguments)]
fn run_remove_attachment(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    keys: &[OsString],
    verbose: bool,
    suppress_warnings: bool,
    remove_restrictions: bool,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    writer_options: WriterOptions,
) -> CliResult<()> {
    let input = input.ok_or("--remove-attachment: missing input PDF")?;
    let output = output.ok_or("--remove-attachment: missing output PDF")?;

    // qpdf switches the logger to "save to standard output" before it opens
    // the input (`QPDFJob.cc:625`), so every `--verbose` info line — the
    // password-encoding recovery notice emitted while opening as well as the
    // removal report below — lands on stderr when the PDF goes to stdout.
    let mut standard_output = prepare_pdf_standard_output(&output)?;
    let creates_output = standard_output.is_none();

    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;

    if remove_restrictions {
        AcroFormDocumentHelper::new(&mut pdf)?.disable_digital_signatures()?;
    }
    for key in keys {
        let key = arg_parser::os_bytes(key);
        let found = pdf.embedded_files().remove_embedded_file(&key)?;
        if !found {
            let mut message = b"attachment ".to_vec();
            message.extend_from_slice(&key);
            message.extend_from_slice(b" not found");
            return Err(Error::SystemBytes(message).into());
        }

        if verbose {
            let mut message = format!("{}: removed attachment ", progname()).into_bytes();
            message.extend_from_slice(&key);
            message.push(b'\n');
            logger_info(message)?;
        }
    }

    let normalization_warnings = if writer_options.content_normalization {
        normalize_page_contents(&mut pdf)?
    } else {
        Vec::new()
    };
    write_with_pdf_writer(
        &mut pdf,
        &output,
        &mut standard_output,
        &writer_options,
        linearize,
        linearize_pass1,
    )?;
    if verbose && output.as_os_str() != "-" {
        logger_info(wrote_file_message(&progname(), &output))?;
    }
    finish_rewrite_warnings(
        &input,
        &pdf,
        &normalization_warnings,
        creates_output,
        suppress_warnings,
    )
}

/// `--list-attachments [--verbose] input`
fn run_list_attachments(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    verbose: bool,
    suppress_warnings: bool,
) -> CliResult<()> {
    let input = input.ok_or("--list-attachments: missing input PDF")?;
    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    job.set_input_name_bytes(path_description(&input));
    let status = job.list_attachments(&mut pdf, verbose)?;
    finish_job_exit_status(status)
}

/// `--show-attachment KEY [-o PATH] input`
fn run_show_attachment(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    key: &OsStr,
    suppress_warnings: bool,
) -> CliResult<()> {
    let input = input.ok_or("--show-attachment: missing input PDF")?;
    let mut pdf = open_pdf_with_suppression(&input, repair, password, suppress_warnings)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    job.set_input_name_bytes(path_description(&input));
    let key = arg_parser::os_bytes(key);
    let status = job.show_attachment(&mut pdf, &key).map_err(|error| {
        let detail = error
            .raw_message()
            .map_or_else(|| error.to_string().into_bytes(), ToOwned::to_owned);
        let mut message = b"--show-attachment: key ".to_vec();
        append_debug_quoted_bytes(&mut message, &key);
        message.extend_from_slice(b" not found or unreadable: ");
        message.extend_from_slice(&detail);
        Error::SystemBytes(message)
    })?;
    finish_job_exit_status(status)
}

/// `--copy-attachments-from FILE [--password=P] [--prefix=X] -- ...`
///
/// qpdf accepts this group repeatedly and copies from every donor in order.
#[allow(clippy::too_many_arguments)]
fn run_copy_attachments_from(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    groups: Vec<Vec<Vec<u8>>>,
    verbose: bool,
    suppress_warnings: bool,
    remove_restrictions: bool,
    linearize: bool,
    linearize_pass1: Option<&Path>,
    writer_options: WriterOptions,
) -> CliResult<()> {
    let input = input.ok_or("--copy-attachments-from: missing input PDF")?;
    let output = output.ok_or("--copy-attachments-from: missing output PDF")?;
    let donor_args = groups
        .into_iter()
        .map(parse_copy_attachments_segment)
        .collect::<CliResult<Vec<_>>>()?;

    // Reserve standard output before opening the target, like qpdf's
    // `saveToStandardOutput` (`QPDFJob.cc:625`), so open-time `--verbose`
    // info lines go to stderr when the PDF goes to stdout.
    let mut standard_output = prepare_pdf_standard_output(&output)?;

    let file = File::open(&input).map_err(|error| open_error_with_file(&input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_suppress_warnings(suppress_warnings);
    let mut pdf = job
        .open_with_description(BufReader::new(file), path_description(&input), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
    pdf.set_suppress_warnings(suppress_warnings);
    if remove_restrictions {
        let _ = AcroFormDocumentHelper::new(&mut pdf)?.disable_digital_signatures()?;
    }

    // Open each source with its own password (independent of the target's).
    // Retain the command-wide open policy so qpdf's recovery/xref controls
    // apply to every secondary input exactly as they do to the target. Each
    // source uses a standalone Pdf rather than job.open_with_description,
    // since qpdf's doCopyAttachments (`QPDFJob.cc:2100`) opens each donor as
    // its own local QPDF. Keeping all donors alive lets the canonical job
    // batch method aggregate duplicate keys across the complete list.
    let mut donor_sources = Vec::with_capacity(donor_args.len());
    for args in donor_args {
        let mut source_password = password.clone();
        source_password.password = None;
        source_password.raw_password = None;
        source_password.password_file = None;
        let mut src_options =
            pdf_open_options_with_password_bytes(repair, &source_password, args.password);
        configure_document_logger(&mut src_options, &args.file);
        src_options.suppress_warnings |= suppress_warnings;
        let src_file = File::open(&args.file)
            .map_err(|error| open_error_with_file(&args.file, error.into()))?;
        let mut src = Pdf::open_with_options(BufReader::new(src_file), src_options)
            .map_err(|error| error_with_file(&args.file, actionable_password_error(error)))?;
        src.root_handle()
            .map_err(|error| error_with_file(&args.file, actionable_password_error(error)))?;
        donor_sources.push((
            src,
            AttachmentCopyOptions {
                path: args.file,
                prefix: args.prefix.unwrap_or_default(),
                verbose,
            },
        ));
    }
    let mut sources = donor_sources
        .iter_mut()
        .map(|(source, options)| AttachmentCopySource {
            source,
            options: options.clone(),
        })
        .collect::<Vec<_>>();
    job.copy_attachments_many(&mut pdf, &mut sources)?;

    // Content normalization is a writer option in qpdf, but the CLI's shared
    // prepass also owns its diagnostic collection. Run it after attachments
    // have been copied so the target page graph is the one normalized by the
    // final writer.
    let normalization_warnings = if writer_options.content_normalization {
        normalize_page_contents(&mut pdf)?
    } else {
        Vec::new()
    };
    write_with_pdf_writer(
        &mut pdf,
        &output,
        &mut standard_output,
        &writer_options,
        linearize,
        linearize_pass1,
    )?;
    if verbose && output.as_os_str() != "-" {
        job.logger()
            .info(wrote_file_message(&progname(), &output))?;
    }
    // Same `--no-warn` boundary as `finish_rewrite_warnings`: the warning is
    // recorded (exit status 3) but its text is suppressed like `QPDF::warn`.
    if !suppress_warnings {
        for &warning in &normalization_warnings {
            emit_content_normalization_warnings(&input, warning)?;
        }
    }
    if !normalization_warnings.is_empty() {
        job.record_warnings();
    }
    job.record_document_warnings(&pdf);
    finish_job_exit_status(job.complete(true)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::pipeline::{Pipeline, PipelineResult};
    use std::sync::{Arc, Mutex};

    struct ChunkRecordingSink {
        chunks: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl Pipeline for ChunkRecordingSink {
        fn identifier(&self) -> &str {
            "chunk recording sink"
        }

        fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
            self.chunks.lock().unwrap().push(data.to_vec());
            Ok(())
        }

        fn finish(&mut self) -> PipelineResult<()> {
            Ok(())
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn os_strs(v: &[&str]) -> Vec<OsString> {
        v.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn cli_flatten_modes_use_the_canonical_job_masks() {
        assert_eq!(
            CliFlattenMode::All.flags(),
            FlattenAnnotationsMode::All.qpdf_flags()
        );
        assert_eq!(
            CliFlattenMode::Screen.flags(),
            FlattenAnnotationsMode::Screen.qpdf_flags()
        );
        assert_eq!(
            CliFlattenMode::Print.flags(),
            FlattenAnnotationsMode::Print.qpdf_flags()
        );
    }

    #[test]
    fn preprocess_qpdf_args_routes_raw_segments_through_arg_parser() {
        let PreprocessedArgs {
            residual_args: residual,
            overlay_specs,
            attachment_segments,
            ..
        } = preprocess_qpdf_args(strs(&["flpdf", "--overlay", "source.pdf", "--to=1", "--"]))
            .expect("qpdf preprocessing should succeed");

        assert_eq!(residual, os_strs(&["flpdf"]));
        assert_eq!(overlay_specs.len(), 1);
        assert_eq!(overlay_specs[0].file, "source.pdf");
        assert_eq!(overlay_specs[0].to.as_deref(), Some("1"));
        assert!(attachment_segments.is_empty());
    }

    #[test]
    fn preprocess_qpdf_args_keeps_raw_top_level_password_for_clap_override() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("args");
        std::fs::write(&path, b"--password=top-\xff\ninput.pdf\n").expect("write argument file");
        let preprocessed = preprocess_qpdf_args(vec![
            OsString::from("flpdf"),
            OsString::from(format!("@{}", path.display())),
        ])
        .expect("qpdf preprocessing should preserve raw password bytes");
        let expected = b"top-\xff".to_vec();
        assert_eq!(
            preprocessed.raw_overrides.password.as_deref(),
            Some(expected.as_slice())
        );

        let mut args = cli_parse_from(preprocessed.residual_args);
        apply_raw_overrides(&mut args, preprocessed.raw_overrides);
        assert_eq!(args.password.password_bytes(), Some(expected));
    }

    #[test]
    fn raw_password_override_reaches_pdf_open_options_without_projection() {
        let mut password = PasswordArgs {
            password: Some(OsString::from("replacement")),
            ..PasswordArgs::default()
        };
        password.raw_password = Some(b"raw-\xff".to_vec());

        let options = pdf_open_options(false, &password).expect("password options should build");
        assert_eq!(options.password, b"raw-\xff");
    }

    #[test]
    fn preprocess_qpdf_args_keeps_raw_encrypt_and_donor_passwords() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let encrypt_path = directory.path().join("encrypt-args");
        std::fs::write(
            &encrypt_path,
            b"--encrypt\nuser-\xff\nowner\n128\n--\ninput.pdf\noutput.pdf\n",
        )
        .expect("write encryption argument file");
        let preprocessed = preprocess_qpdf_args(vec![
            OsString::from("flpdf"),
            OsString::from(format!("@{}", encrypt_path.display())),
        ])
        .expect("qpdf encryption segment should be expanded");
        assert_eq!(
            preprocessed.raw_overrides.raw_encrypt.as_ref().unwrap()[0],
            b"user-\xff"
        );
        let parsed_encrypt = parse_encrypt_segment(
            preprocessed.raw_overrides.raw_encrypt.as_ref().unwrap(),
            true,
        )
        .expect("raw encrypt passwords should reach the byte parser");
        assert_eq!(parsed_encrypt.params.user_password, b"user-\xff");

        let donor_path = directory.path().join("donor-args");
        std::fs::write(
            &donor_path,
            b"--encryption-file-password=donor-\xff\ninput.pdf\noutput.pdf\n",
        )
        .expect("write donor argument file");
        let donor = preprocess_qpdf_args(vec![
            OsString::from("flpdf"),
            OsString::from(format!("@{}", donor_path.display())),
        ])
        .expect("qpdf donor password should be expanded");
        assert_eq!(
            donor.raw_overrides.encryption_file_password.as_deref(),
            Some(b"donor-\xff".as_slice())
        );
    }

    #[test]
    fn preprocess_qpdf_args_rejects_a_second_pages_group_like_qpdf() {
        let error = match preprocess_qpdf_args(strs(&[
            "flpdf", "--pages", "a.pdf", "1", "--", "--pages", "a.pdf", "2", "--", "in.pdf",
            "out.pdf",
        ])) {
            Ok(_) => panic!("qpdf rejects a second --pages group"),
            Err(error) => error,
        };
        let usage = find_usage_error(error.as_ref()).expect("a qpdf usage error");
        assert_eq!(usage.to_string(), "--pages may only be specified one time");
    }

    #[test]
    fn preprocess_qpdf_args_preserves_every_copy_attachments_group() {
        let preprocessed = preprocess_qpdf_args(strs(&[
            "flpdf",
            "--copy-attachments-from",
            "don0.pdf",
            "--",
            "--copy-attachments-from",
            "don1.pdf",
            "--",
            "in.pdf",
            "out.pdf",
        ]))
        .expect("qpdf accepts repeated donor groups");
        assert_eq!(
            preprocessed.raw_overrides.raw_copy_attachments_from,
            Some(vec![vec![b"don0.pdf".to_vec()], vec![b"don1.pdf".to_vec()],])
        );
    }

    #[test]
    fn preprocess_qpdf_args_does_not_promote_segment_password_to_top_level() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("pages-args");
        std::fs::write(
            &path,
            b"--pages\nsource.pdf\n--password=page-\xff\n--\ninput.pdf\noutput.pdf\n",
        )
        .expect("write page argument file");

        let preprocessed = preprocess_qpdf_args(vec![
            OsString::from("flpdf"),
            OsString::from(format!("@{}", path.display())),
        ])
        .expect("qpdf page segment should be expanded");
        assert!(preprocessed.raw_overrides.password.is_none());
        assert_eq!(
            preprocessed.raw_overrides.raw_pages.as_ref().unwrap()[1],
            b"--password=page-\xff"
        );
    }

    #[test]
    fn raw_passwords_reach_pages_overlay_and_copy_segment_parsers() {
        let page_tokens = vec![b"source.pdf".to_vec(), b"--password=page-\xff".to_vec()];
        let pages = parse_pages_segment(&page_tokens).expect("page segment should parse");
        assert_eq!(pages[0].raw_password, Some(b"page-\xff".to_vec()));

        let overlay = parse_overlay_segment(
            OverlayKind::Overlay,
            &[b"source.pdf".to_vec(), b"--password=overlay-\xff".to_vec()],
        )
        .expect("overlay segment should parse");
        assert_eq!(overlay.raw_password, Some(b"overlay-\xff".to_vec()));

        let copy = parse_copy_attachments_segment(vec![
            b"source.pdf".to_vec(),
            b"--password=copy-\xff".to_vec(),
        ])
        .expect("copy-attachments segment should parse");
        assert_eq!(copy.password, b"copy-\xff".to_vec());
    }

    #[test]
    fn standard_save_writer_rejects_stdout_after_info_use() {
        let logger = QPDFLogger::create();
        logger.info([]).unwrap();

        let error = standard_save_writer_for(&logger).err().unwrap();

        assert_eq!(
            error.to_string(),
            "QPDFLogger: called setSave on standard output after standard output has already been used"
        );
    }

    #[test]
    fn json_input_open_error_uses_qpdf_not_found_wording() {
        let error = qpdf_json_input_open_error(
            Path::new("missing.json"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );

        assert_eq!(
            error.to_string(),
            "open missing.json: No such file or directory"
        );
    }

    #[test]
    fn open_error_with_file_keeps_non_io_errors_outside_open_prefix() {
        let error = open_error_with_file(
            Path::new("bad.pdf"),
            flpdf::Error::parse(0, "malformed PDF").into(),
        );

        assert_eq!(
            error.to_string(),
            "bad.pdf: parse error at byte 0: malformed PDF"
        );
    }

    #[test]
    fn qpdf_open_io_error_uses_portable_permission_text() {
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);

        assert_eq!(qpdf_open_io_error_message(&error), "Permission denied");
        let error = std::io::Error::from(std::io::ErrorKind::AlreadyExists);
        assert_eq!(qpdf_open_io_error_message(&error), "File exists");
        let error = std::io::Error::other("native fallback");
        assert_eq!(qpdf_open_io_error_message(&error), "native fallback");
    }

    #[test]
    fn parse_add_attachment_rejects_mimetype_without_type_subtype_separator() {
        let Err(error) =
            parse_add_attachment_segment(strs(&["payload.txt", "--mimetype=textplain"]))
        else {
            panic!("the CLI parser must reject a mimetype without a slash"); // cov:ignore: parser regression guard
        };

        assert_eq!(
            error.to_string(),
            "mime type should be specified as type/subtype"
        );
    }

    #[cfg(unix)]
    #[test]
    fn attachment_segment_paths_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let add =
            parse_add_attachment_segment(vec![OsString::from_vec(b"payload-\xff.bin".to_vec())])
                .expect("raw attachment path should parse");
        assert_eq!(add.file.as_os_str().as_bytes(), b"payload-\xff.bin");

        let copy =
            parse_copy_attachments_segment(vec![OsString::from_vec(b"source-\xff.pdf".to_vec())])
                .expect("raw copy source path should parse");
        assert_eq!(copy.file.as_os_str().as_bytes(), b"source-\xff.pdf");
    }

    #[cfg(unix)]
    #[test]
    fn overlay_file_option_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let token = OsString::from_vec(b"--file=overlay-\xff.pdf".to_vec());
        let spec = parse_overlay_segment(OverlayKind::Overlay, &[token])
            .expect("raw overlay path should parse");
        assert_eq!(spec.file.as_bytes(), b"overlay-\xff.pdf");
    }

    #[test]
    fn cli_command_builds_on_a_small_stack() {
        let command = std::thread::Builder::new()
            .name("small-stack-cli-command".to_owned())
            .stack_size(512 * 1024)
            .spawn(cli_command)
            .expect("small-stack thread should start")
            .join()
            .expect("Cli::command must not overflow a small stack");

        assert_eq!(command.get_name(), "flpdf");
    }

    #[test]
    fn cli_parse_from_builds_on_a_small_stack() {
        let args = std::thread::Builder::new()
            .name("small-stack-cli-parse".to_owned())
            .stack_size(512 * 1024)
            .spawn(|| cli_parse_from(vec![OsString::from("flpdf")]))
            .expect("small-stack thread should start")
            .join()
            .expect("Cli::parse_from must not overflow a small stack");

        assert!(args.command.is_none());
    }

    #[test]
    fn show_pages_writes_each_logical_line_incrementally() {
        let chunks = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let logger = QPDFLogger::create();
        let mut sink = ChunkRecordingSink {
            chunks: Arc::clone(&chunks),
        };
        assert_eq!(sink.identifier(), "chunk recording sink");
        sink.finish().unwrap();
        logger.set_info(Some(PipelineHandle::new(sink)));
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();

        let mut job = QPDFJob::new();
        job.set_logger(logger);
        assert_eq!(job.show_pages(&mut pdf).unwrap(), JobExitCode::Success);

        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), b"page 1: 3 0 R\n  content:\n    7 0 R\n");
    }

    #[test]
    fn probe_encryption_classifies_bad_password_after_repair_warnings() {
        let mut input =
            include_bytes!("../../../tests/fixtures/compat/encrypted-r4-three-page.pdf").to_vec();
        let xref = input
            .windows(4)
            .position(|window| window == b"xref")
            .expect("encrypted fixture should contain an xref keyword");
        input[xref + 2] = b'X';

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("damaged-encrypted.pdf");
        std::fs::write(&path, input).expect("write damaged encrypted fixture");
        let outcome = probe_encryption(
            &path,
            true,
            &PasswordArgs {
                password: Some("wrong".to_owned().into()),
                ..PasswordArgs::default()
            },
            false,
        );

        assert!(matches!(outcome, Ok(EncryptionProbe::EncryptedAuthFailed)));
    }

    #[test]
    fn probe_encryption_prefixes_a_dangling_root_error_with_the_input_path() {
        let input = b"%PDF-1.4\nxref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 1 >>\nstartxref\n9\n%%EOF\n";
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("missing-root.pdf");
        std::fs::write(&path, input).expect("write missing-root fixture");

        let Err(error) = probe_encryption(&path, false, &PasswordArgs::default(), false) else {
            panic!("a missing /Root must be a hard error");
        };

        assert!(
            error.to_string().contains(&path.display().to_string()),
            "error should carry the input path like other open boundaries: {error}"
        );
        assert!(error
            .to_string()
            .contains("unable to find /Root dictionary"));
    }

    #[cfg(unix)]
    #[test]
    fn verified_json_output_rejects_hardlink_swapped_after_path_check() {
        let temp = tempfile::tempdir().unwrap();
        let input_path = temp.path().join("input.pdf");
        let output_path = temp.path().join("output.json");
        let original = b"input bytes must survive".to_vec();
        std::fs::write(&input_path, &original).unwrap();
        std::fs::write(&output_path, b"distinct output").unwrap();
        reject_same_json_output(&input_path, &output_path).unwrap();

        std::fs::remove_file(&output_path).unwrap();
        std::fs::hard_link(&input_path, &output_path).unwrap();
        let input_file = File::open(&input_path).unwrap();

        let error = open_verified_json_output(&input_file, &output_path).unwrap_err();

        assert!(error
            .to_string()
            .contains("input file and output file are the same"));
        assert_eq!(std::fs::read(&input_path).unwrap(), original);
    }

    // --- parse_overlay_segment ------------------------------------------

    #[test]
    fn overlay_bare_file() {
        let spec = parse_overlay_segment(OverlayKind::Overlay, &strs(&["over.pdf"])).unwrap();
        assert_eq!(
            spec,
            OverlaySpec {
                kind: OverlayKind::Overlay,
                file: "over.pdf".into(),
                password: None,
                raw_password: None,
                from: None,
                to: None,
                repeat: None,
            }
        );
    }

    #[test]
    fn underlay_bare_file() {
        let spec = parse_overlay_segment(OverlayKind::Underlay, &strs(&["under.pdf"])).unwrap();
        assert_eq!(spec.kind, OverlayKind::Underlay);
        assert_eq!(spec.file, "under.pdf");
    }

    #[test]
    fn overlay_file_flag_form() {
        let spec =
            parse_overlay_segment(OverlayKind::Overlay, &strs(&["--file=over.pdf"])).unwrap();
        assert_eq!(spec.file, "over.pdf");
        assert_eq!(spec.password, None);
    }

    #[test]
    fn overlay_password() {
        let spec = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["over.pdf", "--password=secret"]),
        )
        .unwrap();
        assert_eq!(spec.file, "over.pdf");
        assert_eq!(spec.password, Some("secret".into()));
    }

    #[test]
    fn overlay_from_to_repeat() {
        let spec = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["src.pdf", "--from=1-3", "--to=2-4", "--repeat=1"]),
        )
        .unwrap();
        assert_eq!(spec.from, Some("1-3".into()));
        assert_eq!(spec.to, Some("2-4".into()));
        assert_eq!(spec.repeat, Some("1".into()));
    }

    #[test]
    fn overlay_all_flags_via_file_flag() {
        let spec = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&[
                "--file=src.pdf",
                "--password=pw",
                "--to=1",
                "--from=1-z",
                "--repeat=z",
            ]),
        )
        .unwrap();
        assert_eq!(spec.file, "src.pdf");
        assert_eq!(spec.password, Some("pw".into()));
        assert_eq!(spec.to, Some("1".into()));
        assert_eq!(spec.from, Some("1-z".into()));
        assert_eq!(spec.repeat, Some("z".into()));
    }

    #[test]
    fn overlay_empty_tokens_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &[] as &[String])
            .unwrap_err()
            .to_string();
        assert!(err.contains("--overlay"), "got: {err}");
        assert!(err.contains("no source file"), "got: {err}");
    }

    #[test]
    fn overlay_missing_file_with_only_sub_options_error() {
        // Sub-options present but no file token at all: fails with a missing-file
        // error (order-independent parsing means --password= alone no longer
        // triggers a "must follow" positional check).
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["--password=pw"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--overlay"), "got: {err}");
        assert!(err.contains("no source file"), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_file_bare_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["a.pdf", "b.pdf"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate file"), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_file_flag_error() {
        let err = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["--file=a.pdf", "--file=b.pdf"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate file"), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_file_mixed_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["a.pdf", "--file=b.pdf"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate file"), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_to_error() {
        let err = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["src.pdf", "--to=1", "--to=2"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate --to="), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_from_error() {
        let err = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["src.pdf", "--from=1", "--from=2"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate --from="), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_repeat_error() {
        let err = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["src.pdf", "--repeat=1", "--repeat=z"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate --repeat="), "got: {err}");
    }

    #[test]
    fn overlay_unknown_flag_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["src.pdf", "--bogus=x"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--overlay"), "got: {err}");
        assert!(err.contains("unsupported token"), "got: {err}");
    }

    #[test]
    fn underlay_unknown_flag_error_prefix() {
        let err = parse_overlay_segment(OverlayKind::Underlay, &strs(&["src.pdf", "--unknown"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--underlay"), "got: {err}");
    }

    #[test]
    fn overlay_invalid_range_to_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["src.pdf", "--to=abc!"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid --to="), "got: {err}");
    }

    #[test]
    fn overlay_invalid_range_from_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["src.pdf", "--from=abc!"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid --from="), "got: {err}");
    }

    #[test]
    fn overlay_invalid_range_repeat_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["src.pdf", "--repeat=abc!"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid --repeat="), "got: {err}");
    }

    #[test]
    fn overlay_option_before_file_to_ok() {
        // qpdf accepts sub-options before the file token within a UO segment.
        let spec =
            parse_overlay_segment(OverlayKind::Overlay, &strs(&["--to=1", "src.pdf"])).unwrap();
        assert_eq!(spec.file, "src.pdf");
        assert_eq!(spec.to, Some("1".into()));
        assert_eq!(spec.from, None);
        assert_eq!(spec.repeat, None);
        assert_eq!(spec.password, None);
    }

    #[test]
    fn overlay_option_before_file_from_ok() {
        let spec =
            parse_overlay_segment(OverlayKind::Overlay, &strs(&["--from=1", "src.pdf"])).unwrap();
        assert_eq!(spec.file, "src.pdf");
        assert_eq!(spec.from, Some("1".into()));
    }

    #[test]
    fn overlay_option_before_file_repeat_ok() {
        let spec =
            parse_overlay_segment(OverlayKind::Overlay, &strs(&["--repeat=1", "src.pdf"])).unwrap();
        assert_eq!(spec.file, "src.pdf");
        assert_eq!(spec.repeat, Some("1".into()));
    }

    #[test]
    fn overlay_option_before_file_password_ok() {
        let spec =
            parse_overlay_segment(OverlayKind::Overlay, &strs(&["--password=pw", "src.pdf"]))
                .unwrap();
        assert_eq!(spec.file, "src.pdf");
        assert_eq!(spec.password, Some("pw".into()));
    }

    #[test]
    fn overlay_options_mixed_around_file_ok() {
        // Sub-options both before and after the file token, mirroring qpdf's
        // free-order UO segment parsing.
        let spec = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&[
                "--password=pw",
                "--from=1",
                "src.pdf",
                "--to=2",
                "--repeat=z",
            ]),
        )
        .unwrap();
        assert_eq!(spec.file, "src.pdf");
        assert_eq!(spec.password, Some("pw".into()));
        assert_eq!(spec.from, Some("1".into()));
        assert_eq!(spec.to, Some("2".into()));
        assert_eq!(spec.repeat, Some("z".into()));
    }

    #[test]
    fn underlay_option_before_file_ok() {
        let spec = parse_overlay_segment(
            OverlayKind::Underlay,
            &strs(&["--to=1", "--from=2", "under.pdf"]),
        )
        .unwrap();
        assert_eq!(spec.kind, OverlayKind::Underlay);
        assert_eq!(spec.file, "under.pdf");
        assert_eq!(spec.to, Some("1".into()));
        assert_eq!(spec.from, Some("2".into()));
    }

    #[test]
    fn overlay_duplicate_to_before_file_error() {
        // Duplicate detection stays effective even when duplicates straddle
        // the file token.
        let err = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["--to=1", "src.pdf", "--to=2"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate --to="), "got: {err}");
    }

    #[test]
    fn overlay_invalid_range_before_file_error() {
        // Range validation runs regardless of where the sub-option appears.
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["--to=abc!", "src.pdf"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid --to="), "got: {err}");
    }

    #[test]
    fn overlay_duplicate_password_error() {
        let err = parse_overlay_segment(
            OverlayKind::Overlay,
            &strs(&["src.pdf", "--password=a", "--password=b"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("duplicate --password="), "got: {err}");
    }

    // --- rewrite_qpdf_single_dash ---------------------------------------

    #[test]
    fn single_dash_long_becomes_double_dash() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-qdf", "-static-id", "in.pdf"]));
        assert_eq!(out, strs(&["flpdf", "--qdf", "--static-id", "in.pdf"]));
    }

    #[test]
    fn single_dash_long_with_equals_becomes_double_dash() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-object-streams=generate"]));
        assert_eq!(out, strs(&["flpdf", "--object-streams=generate"]));
    }

    #[test]
    fn single_dash_bare_equals_value_is_discarded_after_rewriting() {
        let out =
            normalize_qpdf_bare_equals(rewrite_qpdf_single_dash(strs(&["flpdf", "-qdf=ignored"])));
        assert_eq!(out, strs(&["flpdf", "--qdf"]));
    }

    #[test]
    fn qpdf_bare_boolean_equals_values_are_discarded() {
        let out = normalize_qpdf_bare_equals(strs(&[
            "flpdf",
            "--check=ignored",
            "--qdf=ignored",
            "--static-id=ignored",
            "--verbose=ignored",
            "--preserve-unreferenced=ignored",
        ]));
        assert_eq!(
            out,
            strs(&[
                "flpdf",
                "--check",
                "--qdf",
                "--static-id",
                "--verbose",
                "--preserve-unreferenced",
            ])
        );
    }

    #[test]
    fn qpdf_bare_normalization_preserves_value_options() {
        let args = strs(&[
            "flpdf",
            "--json=2",
            "--object-streams=generate",
            "--normalize-content=y",
            "--newline-before-endstream=never",
        ]);
        assert_eq!(normalize_qpdf_bare_equals(args.clone()), args);
    }

    #[test]
    fn newline_before_endstream_discards_an_unrecognized_equals_value() {
        let args = strs(&["flpdf", "--newline-before-endstream=garbage"]);

        assert_eq!(
            normalize_qpdf_bare_equals(args),
            strs(&["flpdf", "--newline-before-endstream"])
        );
    }

    #[test]
    fn qpdf_bare_normalization_preserves_value_terminated_segments() {
        let args = strs(&[
            "flpdf",
            "--encrypt",
            "user",
            "owner",
            "256",
            "--qdf=an-encrypt-suboption-value",
            "--",
            "--check=after-segment",
        ]);
        assert_eq!(
            normalize_qpdf_bare_equals(args),
            strs(&[
                "flpdf",
                "--encrypt",
                "user",
                "owner",
                "256",
                "--qdf=an-encrypt-suboption-value",
                "--",
                "--check",
            ])
        );
    }

    #[test]
    fn double_dash_long_is_untouched() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "--qdf", "--static-id"]));
        assert_eq!(out, strs(&["flpdf", "--qdf", "--static-id"]));
    }

    #[test]
    fn known_short_flags_untouched() {
        // -o and -h are the only shorts flpdf declares; they must not be
        // rewritten to `--o`/`--h`.
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-o", "path", "-h"]));
        assert_eq!(out, strs(&["flpdf", "-o", "path", "-h"]));
    }

    #[test]
    fn stdin_sentinel_and_options_terminator_untouched() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-", "--"]));
        assert_eq!(out, strs(&["flpdf", "-", "--"]));
    }

    #[test]
    fn single_dash_long_after_section_terminator_is_rewritten() {
        let out = rewrite_qpdf_single_dash(strs(&[
            "flpdf",
            "-pages",
            ".",
            "--",
            "-qdf",
            "-not-an-option",
        ]));
        assert_eq!(
            out,
            strs(&["flpdf", "--pages", ".", "--", "--qdf", "-not-an-option",])
        );
    }

    #[test]
    fn top_level_terminator_preserves_remaining_tokens() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "--", "-in.pdf", "-qdf"]));
        assert_eq!(out, strs(&["flpdf", "--", "-in.pdf", "-qdf"]));
    }

    #[test]
    fn attached_short_output_is_untouched() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-o/tmp/out"]));
        assert_eq!(out, strs(&["flpdf", "-o/tmp/out"]));
    }

    #[test]
    fn hyphenated_segment_operand_is_untouched() {
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-add-attachment", "-note.txt", "--"]));
        assert_eq!(out, strs(&["flpdf", "--add-attachment", "-note.txt", "--"]));
    }

    #[test]
    fn single_dash_segment_sub_option_is_rewritten() {
        let parsed = arg_parser::ArgParser::from_command(cli_command())
            .parse(strs(&["flpdf", "-overlay", "stamp.pdf", "-to=1", "--"]))
            .unwrap();
        assert_eq!(parsed.residual_args, os_strs(&["flpdf"]));
        assert_eq!(parsed.named_segments[0].option, "overlay");
        assert_eq!(
            parsed.named_segments[0].tokens,
            os_strs(&["stamp.pdf", "--to=1"])
        );
    }

    #[test]
    fn each_segment_kind_recognizes_its_sub_options() {
        let parsed = arg_parser::ArgParser::from_command(cli_command())
            .parse(strs(&[
                "flpdf",
                "--encrypt",
                "-use-aes",
                "--",
                "--pages",
                "-range=1",
                "--",
                "--add-attachment",
                "-replace",
                "--",
                "--copy-attachments-from",
                "-prefix=copy-",
                "--",
            ]))
            .unwrap();

        assert_eq!(parsed.named_segments[0].tokens, ["--use-aes"]);
        assert_eq!(parsed.named_segments[1].tokens, ["--range=1"]);
        assert_eq!(parsed.named_segments[2].tokens, ["--replace"]);
        assert_eq!(parsed.named_segments[3].tokens, ["--prefix=copy-"]);
    }

    #[test]
    fn collect_clap_long_options_includes_aliases() {
        let command = clap::Command::new("test")
            .arg(clap::Arg::new("mode").long("mode").alias("legacy-mode"));
        let parsed = arg_parser::ArgParser::from_command(command)
            .parse(strs(&["test", "-legacy-mode"]))
            .unwrap();

        assert_eq!(parsed.residual_args, os_strs(&["test", "--legacy-mode"]));
    }

    #[test]
    fn legacy_encrypt_password_is_not_rewritten() {
        let out =
            rewrite_qpdf_single_dash(strs(&["flpdf", "-encrypt", "-user", "owner", "128", "--"]));
        assert_eq!(
            out,
            strs(&["flpdf", "--encrypt", "-user", "owner", "128", "--",])
        );
    }

    #[test]
    fn non_bare_hyphen_encrypt_password_is_rejected() {
        let err = parse_encrypt_segment(&strs(&["-user", "owner", "128"]), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unrecognized argument -user"), "got: {err}");
    }

    #[test]
    fn bare_hyphen_encrypt_password_is_accepted() {
        let params = parse_encrypt_segment(&strs(&["-", "-", "128"]), true)
            .unwrap()
            .params;
        assert_eq!(params.user_password, b"-");
        assert_eq!(params.owner_password, b"-");
    }

    #[test]
    fn encrypt_parser_accepts_dashed_passwords_and_bits_before_the_terminator() {
        let params = parse_encrypt_segment(
            &strs(&["--user-password=u", "--bits=256", "--allow-insecure"]),
            false,
        )
        .expect("qpdf dashed encryption form")
        .params;
        assert_eq!(params.method, EncryptMethod::V5R6Aes256);
        assert_eq!(params.user_password, b"u");
        assert!(params.owner_password.is_empty());
    }

    #[test]
    fn encrypt_parser_rejects_mixed_positional_and_dashed_passwords() {
        let error = parse_encrypt_segment(&strs(&["user", "--owner-password=owner", "128"]), true)
            .expect_err("mixed encryption form");
        assert!(error
            .to_string()
            .contains("positional and dashed encryption arguments may not be mixed"));
    }

    #[test]
    fn encrypt_parser_keeps_r2_permissions_separate_from_r3_permissions() {
        let parsed = parse_encrypt_segment(
            &strs(&[
                "user",
                "owner",
                "40",
                "-print=n",
                "-modify=y",
                "-extract=n",
                "-annotate=y",
            ]),
            true,
        )
        .expect("R=2 encryption options");
        assert_eq!(parsed.params.method, EncryptMethod::V1Rc440);
        assert!(!parsed.params.r2_permissions.print);
        assert!(parsed.params.r2_permissions.modify);
        assert!(!parsed.params.r2_permissions.extract);
        assert!(parsed.params.r2_permissions.annotate);
        assert_eq!(parsed.params.permissions, PermissionsConfig::default());
    }

    #[test]
    fn encrypt_parser_reports_ignored_accessibility_for_modern_revisions() {
        let parsed = parse_encrypt_segment(
            &strs(&["user", "owner", "128", "--force-V4", "--accessibility=n"]),
            true,
        )
        .expect("modern encryption options");
        assert!(parsed.accessibility_warning);
        assert_eq!(parsed.params.method, EncryptMethod::V4Rc4128);
        assert!(parsed.params.permissions.accessibility);
    }

    #[test]
    fn negative_number_positional_untouched() {
        // -1 / -0.5 are numeric positionals, never options.
        let out = rewrite_qpdf_single_dash(strs(&["flpdf", "-1", "-0.5"]));
        assert_eq!(out, strs(&["flpdf", "-1", "-0.5"]));
    }

    // --- extract_overlay_groups -----------------------------------------

    #[test]
    fn extract_no_overlay_leaves_args_untouched() {
        let argv = strs(&["flpdf", "rewrite", "--static-id", "in.pdf", "out.pdf"]);
        let (residual, specs) = extract_overlay_groups(argv.clone()).unwrap();
        assert_eq!(residual, argv);
        assert!(specs.is_empty());
    }

    #[test]
    fn extract_single_attachment_group_leaves_a_clap_dispatch_marker() {
        let argv = strs(&[
            "flpdf",
            "in.pdf",
            "--add-attachment",
            "one.txt",
            "--key=one",
            "--",
            "out.pdf",
        ]);
        let (residual, groups) = extract_attachment_groups(argv).unwrap();

        assert_eq!(
            residual,
            strs(&[
                "flpdf",
                "in.pdf",
                "--add-attachment",
                "one.txt",
                "--key=one",
                "--",
                "out.pdf",
            ])
        );
        assert_eq!(groups, vec![strs(&["one.txt", "--key=one"])]);
    }

    #[test]
    fn extract_repeated_attachment_groups_preserves_order_and_boundaries() {
        let argv = strs(&[
            "flpdf",
            "in.pdf",
            "--add-attachment",
            "one.txt",
            "--key=one",
            "--",
            "--add-attachment",
            "two.txt",
            "--key=two",
            "--",
            "out.pdf",
        ]);
        let (residual, groups) = extract_attachment_groups(argv).unwrap();

        assert_eq!(
            residual,
            strs(&[
                "flpdf",
                "in.pdf",
                "--add-attachment",
                "one.txt",
                "--key=one",
                "--",
                "out.pdf",
            ])
        );
        assert_eq!(
            groups,
            vec![
                strs(&["one.txt", "--key=one"]),
                strs(&["two.txt", "--key=two"]),
            ]
        );
    }

    #[test]
    fn extract_attachment_groups_leaves_opaque_sibling_values_untouched() {
        let argv = strs(&[
            "flpdf",
            "--pages",
            "--add-attachment",
            "one.txt",
            "--",
            "out.pdf",
        ]);
        let (residual, groups) = extract_attachment_groups(argv.clone()).unwrap();

        assert_eq!(residual, argv);
        assert!(groups.is_empty());
    }

    #[test]
    fn extract_attachment_groups_rejects_an_unterminated_group() {
        let error = extract_attachment_groups(strs(&["flpdf", "--add-attachment", "one.txt"]))
            .expect_err("an attachment group must have a terminator");

        assert_eq!(error.to_string(), "--add-attachment: missing -- terminator");
    }

    #[test]
    fn extract_attachment_groups_discards_the_equals_value_like_qpdfs_bare_option() {
        // qpdf's `--add-attachment` is a bare option (QPDFJob_argv.cc:38's
        // addBare): `QPDFArgParser` silently discards any `=value` attached
        // to the flag itself, so a later plain positional token becomes the
        // file. Confirmed against /usr/bin/qpdf 11.9.0: `--add-attachment=
        // bogus.txt payload.txt --` embeds `payload.txt` and drops `bogus.txt`.
        let argv = strs(&[
            "flpdf",
            "in.pdf",
            "--add-attachment=bogus.txt",
            "payload.txt",
            "--",
            "out.pdf",
        ]);
        let (_, groups) = extract_attachment_groups(argv).unwrap();

        assert_eq!(groups, vec![strs(&["payload.txt"])]);
    }

    #[test]
    fn extract_attachment_groups_equals_form_with_no_positional_yields_an_empty_segment() {
        // Confirmed against /usr/bin/qpdf 11.9.0: `--add-attachment=x --`
        // errors "add attachment: no file specified" because nothing
        // follows the discarded `=value` to serve as the file.
        let argv = strs(&[
            "flpdf",
            "in.pdf",
            "--add-attachment=payload.txt",
            "--",
            "out.pdf",
        ]);
        let (_, groups) = extract_attachment_groups(argv).unwrap();

        assert_eq!(groups, vec![Vec::<String>::new()]);
    }

    #[test]
    fn extract_single_overlay_group() {
        let argv = strs(&[
            "flpdf",
            "rewrite",
            "in.pdf",
            "--overlay",
            "over.pdf",
            "--",
            "out.pdf",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        // The flag, its tokens, and the terminating `--` are removed.
        assert_eq!(residual, strs(&["flpdf", "rewrite", "in.pdf", "out.pdf"]));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].kind, OverlayKind::Overlay);
        assert_eq!(specs[0].file, "over.pdf");
    }

    #[test]
    fn extract_two_overlay_groups_preserves_boundaries_and_order() {
        // The design-mandated case: two groups must split, not flatten.
        let argv = strs(&["--overlay", "a.pdf", "--", "--overlay", "b.pdf", "--"]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert!(
            residual.is_empty(),
            "all overlay tokens stripped: {residual:?}"
        );
        assert_eq!(
            specs.len(),
            2,
            "two distinct groups, not one flattened list"
        );
        assert_eq!(specs[0].file, "a.pdf");
        assert_eq!(specs[1].file, "b.pdf");
    }

    #[test]
    fn extract_mixed_overlay_underlay_preserves_declaration_order() {
        // Mixed kinds must keep CLI declaration order (overlay then underlay);
        // the library re-groups under-then-over internally.
        let argv = strs(&[
            "in.pdf",
            "--overlay",
            "one.pdf",
            "--",
            "--underlay",
            "two.pdf",
            "--",
            "out.pdf",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(residual, strs(&["in.pdf", "out.pdf"]));
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].kind, OverlayKind::Overlay);
        assert_eq!(specs[0].file, "one.pdf");
        assert_eq!(specs[1].kind, OverlayKind::Underlay);
        assert_eq!(specs[1].file, "two.pdf");
    }

    #[test]
    fn extract_captures_sub_flags_per_group() {
        let argv = strs(&[
            "--overlay",
            "--file=src.pdf",
            "--password=pw",
            "--from=1",
            "--to=2-3",
            "--repeat=1",
            "--",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert!(residual.is_empty());
        assert_eq!(specs.len(), 1);
        let s = &specs[0];
        assert_eq!(s.file, "src.pdf");
        assert_eq!(s.password.as_deref(), Some(OsStr::new("pw")));
        assert_eq!(s.from.as_deref(), Some("1"));
        assert_eq!(s.to.as_deref(), Some("2-3"));
        assert_eq!(s.repeat.as_deref(), Some("1"));
    }

    #[test]
    fn extract_leaves_trailing_top_level_flag_after_group_terminator() {
        // qtest form-xobject uo-3 style: a top-level flag appears AFTER the
        // overlay/underlay group's `--` terminator. The extractor must place
        // that trailing flag verbatim into the residual argv so clap sees it.
        // A regression here would put the trailing top-level flag in the wrong
        // parser group and make clap report it as an unknown option.
        let argv = strs(&[
            "flpdf",
            "in.pdf",
            "out.pdf",
            "--overlay",
            "src.pdf",
            "--",
            "--coalesce-contents",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(
            residual,
            strs(&["flpdf", "in.pdf", "out.pdf", "--coalesce-contents"])
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].file, "src.pdf");
    }

    #[test]
    fn extract_unterminated_group_errors() {
        // No bare `--` after the file => qpdf requires the terminator.
        let argv = strs(&["--overlay", "over.pdf", "out.pdf"]);
        let err = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(err.contains("terminated by a `--`"), "got: {err}");
    }

    #[test]
    fn extract_propagates_segment_parse_errors() {
        // An invalid page range inside a group surfaces the segment error.
        let argv = strs(&["--overlay", "over.pdf", "--to=abc!", "--"]);
        let err = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(err.contains("invalid --to="), "got: {err}");
    }

    #[test]
    fn extract_sub_options_before_file_within_group() {
        // qpdf accepts sub-options in any order within a UO segment, so the raw
        // argv `--overlay --to=1 src.pdf --` must parse to a single overlay
        // group with `to=1` and `file=src.pdf`. Mirrors the repro in the issue:
        // `flpdf rewrite in.pdf --overlay --to=1 src.pdf -- out.pdf`.
        let argv = strs(&[
            "flpdf",
            "rewrite",
            "in.pdf",
            "--overlay",
            "--to=1",
            "src.pdf",
            "--",
            "out.pdf",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(residual, strs(&["flpdf", "rewrite", "in.pdf", "out.pdf"]));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].kind, OverlayKind::Overlay);
        assert_eq!(specs[0].file, "src.pdf");
        assert_eq!(specs[0].to.as_deref(), Some("1"));
    }

    #[test]
    fn extract_overlay_equals_form_without_positional_file_is_rejected() {
        // qpdf discards the attached value on this bare option, so the segment
        // still has no source file and is rejected.
        let argv = strs(&["flpdf", "--overlay=discarded", "--"]);
        let err = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(err.contains("--overlay"), "got: {err}");
        assert!(err.contains("no source file"), "got: {err}");
    }

    #[test]
    fn extract_underlay_equals_form_without_positional_file_is_rejected() {
        let argv = strs(&["flpdf", "--underlay=discarded", "--"]);
        let err = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(err.contains("--underlay"), "got: {err}");
        assert!(err.contains("no source file"), "got: {err}");
    }

    #[test]
    fn extract_overlay_equals_form_discards_value_when_positional_source_follows() {
        let argv = strs(&["flpdf", "--overlay=discarded", "src.pdf", "--"]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(residual, strs(&["flpdf"]));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].file, "src.pdf");
    }

    #[test]
    fn extract_underlay_equals_form_discards_value_when_positional_source_follows() {
        let argv = strs(&["flpdf", "--underlay=discarded", "src.pdf", "--"]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(residual, strs(&["flpdf"]));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].file, "src.pdf");
    }

    #[test]
    fn extract_password_sub_flag_not_mistaken_for_terminator() {
        // `--password=…` starts with `--` but only a bare `--` terminates.
        let argv = strs(&["--overlay", "src.pdf", "--password=--weird", "--"]);
        let (_residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].password.as_deref(), Some(OsStr::new("--weird")));
    }

    #[test]
    fn extract_overlay_token_inside_encrypt_segment_is_not_a_group() {
        // `--overlay` here is a *value* of the value-terminated --encrypt segment
        // (a literal user password), not a new overlay group. It must survive in
        // the residual for clap, and no spurious group is produced.
        let argv = strs(&[
            "flpdf",
            "rewrite",
            "--encrypt",
            "--overlay",
            "owner",
            "128",
            "--use-aes=y",
            "--",
            "in.pdf",
            "out.pdf",
        ]);
        let (residual, specs) = extract_overlay_groups(argv.clone()).unwrap();
        assert!(specs.is_empty(), "no overlay group, got: {specs:?}");
        assert_eq!(residual, argv, "encrypt segment copied verbatim");
    }

    #[test]
    fn extract_underlay_token_inside_pages_segment_is_not_a_group() {
        // Same protection for the --pages segment (and --underlay).
        let argv = strs(&["--pages", "a.pdf", "--underlay", "b.pdf", "--", "out.pdf"]);
        let (residual, specs) = extract_overlay_groups(argv.clone()).unwrap();
        assert!(specs.is_empty(), "got: {specs:?}");
        assert_eq!(residual, argv);
    }

    #[test]
    fn extract_real_overlay_after_encrypt_segment_still_extracted() {
        // A genuine --overlay group AFTER the encrypt segment's own `--` is still
        // recognised and stripped; the encrypt segment is preserved verbatim.
        let argv = strs(&[
            "--encrypt",
            "u",
            "o",
            "128",
            "--",
            "--overlay",
            "src.pdf",
            "--",
            "in.pdf",
            "out.pdf",
        ]);
        let (residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].file, "src.pdf");
        assert_eq!(
            residual,
            strs(&["--encrypt", "u", "o", "128", "--", "in.pdf", "out.pdf"])
        );
    }

    #[test]
    fn extract_unterminated_sibling_segment_errors_at_parser_boundary() {
        // qpdf's named option table must be terminated before the parser
        // resumes the main table; the inner --overlay is not a new group.
        let argv = strs(&["--encrypt", "u", "o", "128", "--overlay", "x"]);
        let error = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(error.contains("--encrypt"), "got: {error}");
        assert!(error.contains("terminated"), "got: {error}");
    }

    #[test]
    fn extract_overlay_token_inside_equals_form_attachment_segment_is_not_a_group() {
        // `--add-attachment=discarded` (qpdf's bare-option equals-form) starts the
        // same opaque segment as `--add-attachment`. A file literally named
        // `--overlay` as the segment's positional token must not be hijacked into
        // a new overlay group by this earlier pass.
        let argv = strs(&["--add-attachment=discarded", "--overlay", "--", "out.pdf"]);
        let (residual, specs) = extract_overlay_groups(argv.clone()).unwrap();
        assert!(specs.is_empty(), "no overlay group, got: {specs:?}");
        assert_eq!(
            residual,
            strs(&["--add-attachment", "--overlay", "--", "out.pdf"]),
            "qpdf discards the equals value on a bare segment option"
        );
    }

    #[test]
    fn extract_overlay_groups_normalizes_equals_form_sibling_segments() {
        for (equals_form, bare_form) in [
            ("--encrypt=discarded", "--encrypt"),
            ("--pages=discarded", "--pages"),
            (
                "--copy-attachments-from=discarded",
                "--copy-attachments-from",
            ),
        ] {
            let argv = strs(&["in.pdf", equals_form, "--overlay", "--", "out.pdf"]);
            let (residual, specs) = extract_overlay_groups(argv).unwrap();
            assert!(specs.is_empty(), "no overlay group, got: {specs:?}");
            assert_eq!(
                residual,
                strs(&["in.pdf", bare_form, "--overlay", "--", "out.pdf"]),
                "equals value must be discarded like qpdf for {equals_form}"
            );
        }
    }

    #[test]
    fn extract_attachment_groups_normalizes_equals_form_sibling_segments() {
        for (equals_form, bare_form) in [
            ("--encrypt=discarded", "--encrypt"),
            ("--pages=discarded", "--pages"),
            (
                "--copy-attachments-from=discarded",
                "--copy-attachments-from",
            ),
        ] {
            let argv = strs(&[
                "flpdf",
                "in.pdf",
                equals_form,
                "source.pdf",
                "--",
                "out.pdf",
            ]);
            let (residual, groups) = extract_attachment_groups(argv).unwrap();
            assert!(
                groups.is_empty(),
                "sibling segment must not become attachment groups"
            );
            assert_eq!(
                residual,
                strs(&["flpdf", "in.pdf", bare_form, "source.pdf", "--", "out.pdf"]),
                "equals value must be discarded like qpdf for {equals_form}"
            );
        }
    }

    // --- build_overlay_specs --------------------------------------------

    fn compat_fixture(name: &str) -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(name)
            .to_str()
            .expect("utf-8 path")
            .to_string()
    }

    #[test]
    fn build_overlay_specs_opens_source_and_maps_fields() {
        // A bare unencrypted source with all ranges set: opens the Pdf, maps the
        // kind, and parses from/to/repeat (repeat present here).
        let cli_specs = vec![OverlaySpec {
            kind: OverlayKind::Underlay,
            file: compat_fixture("one-page.pdf").into(),
            password: None,
            raw_password: None,
            from: Some("1".into()),
            to: Some("1-2".into()),
            repeat: Some("1".into()),
        }];
        let built = build_overlay_specs(&cli_specs, false, &PasswordArgs::default()).unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].kind, flpdf::OverlayKind::Underlay);
        // repeat is Some when the segment supplied --repeat.
        assert!(built[0].repeat.is_some());
    }

    #[test]
    fn build_overlay_specs_defaults_ranges_when_absent() {
        // No from/to/repeat: from/to default to the empty (all-pages) range and
        // repeat stays None.
        let cli_specs = vec![OverlaySpec {
            kind: OverlayKind::Overlay,
            file: compat_fixture("one-page.pdf").into(),
            password: None,
            raw_password: None,
            from: None,
            to: None,
            repeat: None,
        }];
        let built = build_overlay_specs(&cli_specs, false, &PasswordArgs::default()).unwrap();
        assert_eq!(built[0].kind, flpdf::OverlayKind::Overlay);
        assert!(
            built[0].repeat.is_none(),
            "repeat None when --repeat absent"
        );
    }

    #[test]
    fn build_overlay_specs_missing_file_errors() {
        let cli_specs = vec![OverlaySpec {
            kind: OverlayKind::Overlay,
            file: "/nonexistent/overlay/source.pdf".into(),
            password: None,
            raw_password: None,
            from: None,
            to: None,
            repeat: None,
        }];
        // `flpdf::OverlaySpec` is not Debug (it holds a `Pdf`), so match the Ok
        // arm explicitly instead of `unwrap_err()`.
        let err = match build_overlay_specs(&cli_specs, false, &PasswordArgs::default()) {
            Ok(_) => panic!("expected error for a missing source file"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("source.pdf"),
            "error should name the unreadable file: {err}"
        );
    }

    fn encrypted_fixture(name: &str) -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/encrypted")
            .join(name)
            .to_str()
            .expect("utf-8 path")
            .to_string()
    }

    #[test]
    fn build_overlay_specs_distinguishes_absent_and_empty_from() {
        // qpdf parity: an absent `--from` defaults to all source pages, while an
        // explicit empty `--from=` selects no source pages (so `--repeat` cycles
        // from the first destination page).
        let file = compat_fixture("three-page.pdf");
        let spec = |from: Option<&str>| {
            vec![OverlaySpec {
                kind: OverlayKind::Overlay,
                file: file.clone().into(),
                password: None,
                raw_password: None,
                from: from.map(str::to_string),
                to: None,
                repeat: None,
            }]
        };

        let absent = build_overlay_specs(&spec(None), false, &PasswordArgs::default()).unwrap();
        assert_eq!(absent[0].from.resolve(3).unwrap(), vec![1, 2, 3]);

        let empty = build_overlay_specs(&spec(Some("")), false, &PasswordArgs::default()).unwrap();
        assert_eq!(empty[0].from.resolve(3).unwrap(), Vec::<u32>::new());

        let explicit =
            build_overlay_specs(&spec(Some("2")), false, &PasswordArgs::default()).unwrap();
        assert_eq!(explicit[0].from.resolve(3).unwrap(), vec![2]);
    }

    #[test]
    fn build_overlay_specs_distinguishes_absent_and_empty_to() {
        // qpdf parity: an absent `--to` defaults to all destination pages, while
        // an explicit empty `--to=` selects no destination pages (the overlay
        // becomes a no-op). A non-empty `--to=` is honored verbatim.
        let file = compat_fixture("three-page.pdf");
        let spec = |to: Option<&str>| {
            vec![OverlaySpec {
                kind: OverlayKind::Overlay,
                file: file.clone().into(),
                password: None,
                raw_password: None,
                from: None,
                to: to.map(str::to_string),
                repeat: None,
            }]
        };

        let absent = build_overlay_specs(&spec(None), false, &PasswordArgs::default()).unwrap();
        assert_eq!(absent[0].to.resolve(3).unwrap(), vec![1, 2, 3]);

        let empty = build_overlay_specs(&spec(Some("")), false, &PasswordArgs::default()).unwrap();
        assert_eq!(empty[0].to.resolve(3).unwrap(), Vec::<u32>::new());

        let explicit =
            build_overlay_specs(&spec(Some("2-3")), false, &PasswordArgs::default()).unwrap();
        assert_eq!(explicit[0].to.resolve(3).unwrap(), vec![2, 3]);
    }

    #[test]
    fn build_overlay_specs_distinguishes_absent_and_empty_repeat() {
        // qpdf parity: an explicit empty `--repeat=` is "no repeat", identical to
        // an absent `--repeat` (both map to `None`). A non-empty `--repeat=` is
        // parsed into a source-page range.
        let file = compat_fixture("three-page.pdf");
        let spec = |repeat: Option<&str>| {
            vec![OverlaySpec {
                kind: OverlayKind::Overlay,
                file: file.clone().into(),
                password: None,
                raw_password: None,
                from: None,
                to: None,
                repeat: repeat.map(str::to_string),
            }]
        };

        let absent = build_overlay_specs(&spec(None), false, &PasswordArgs::default()).unwrap();
        assert!(absent[0].repeat.is_none(), "absent --repeat -> None");

        let empty = build_overlay_specs(&spec(Some("")), false, &PasswordArgs::default()).unwrap();
        assert!(
            empty[0].repeat.is_none(),
            "explicit empty --repeat= -> None (no repeat), same as absent"
        );

        let explicit =
            build_overlay_specs(&spec(Some("2")), false, &PasswordArgs::default()).unwrap();
        assert_eq!(
            explicit[0].repeat.as_ref().unwrap().resolve(3).unwrap(),
            vec![2]
        );
    }

    #[test]
    fn build_overlay_specs_opens_rc4_source_without_allow_weak_crypto() {
        // qpdf-parity: RC4 (weak-crypto) overlay sources open unconditionally,
        // because the `--allow-weak-crypto` flag gates weak-crypto *writes*,
        // not reads. `build_overlay_specs` is a read-only inspection open — the
        // same category `run_check` handles — and matches qpdf's silent-accept
        // behavior on RC4 sources (verified against qtest `form-xobject` test 31
        // / uo-7).
        let cli_specs = vec![OverlaySpec {
            kind: OverlayKind::Overlay,
            file: encrypted_fixture("v2-rc4-128-r3.pdf").into(),
            password: Some("user-v2".into()),
            raw_password: None,
            from: None,
            to: None,
            repeat: None,
        }];

        let built = build_overlay_specs(&cli_specs, false, &PasswordArgs::default()).unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].kind, flpdf::OverlayKind::Overlay);
    }

    #[test]
    fn show_object_selector_defaults_generation_like_qpdf() {
        assert!(matches!(
            parse_show_object_selector("1"),
            Ok(ShowObjectSelector::Object(ObjectRef {
                number: 1,
                generation: 0,
            }))
        ));
        assert!(matches!(
            parse_show_object_selector("1,"),
            Ok(ShowObjectSelector::Object(ObjectRef {
                number: 1,
                generation: 0,
            }))
        ));
    }

    #[test]
    fn show_object_selector_integer_errors_match_qpdf() {
        let overflow = qpdf_selector_integer("9223372036854775808").expect_err("i64 overflow");
        assert_eq!(
            overflow.to_string(),
            "overflow/underflow converting 9223372036854775808 to 64-bit integer"
        );
        assert!(
            overflow.downcast_ref::<UsageError>().is_some(),
            "qpdf reports this as a QPDFUsage-class error (thrown from argv parsing, \
             before the input file is opened), so it must route through \
             flpdf-cli's usage_exit path rather than the generic error path"
        );

        // A digit run too long to even fit in the i128 staging type used to
        // detect i64 overflow (rather than merely exceeding i64's range).
        let huge = qpdf_selector_integer("99999999999999999999999999999999999999999999999999")
            .expect_err("digit run exceeding i128");
        assert_eq!(
            huge.to_string(),
            "overflow/underflow converting 99999999999999999999999999999999999999999999999999 to 64-bit integer"
        );
        assert!(huge.downcast_ref::<UsageError>().is_some());

        let narrowing = qpdf_selector_integer("2147483648").expect_err("i32 narrowing overflow");
        assert_eq!(
            narrowing.to_string(),
            "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type"
        );
        assert!(narrowing.downcast_ref::<UsageError>().is_some());
    }

    #[test]
    fn show_object_selector_parses_before_checking_for_an_input_file() {
        // qpdf's Config::showObject parses the selector during argv parsing,
        // before QPDFJob::run() ever opens an input file, so a usage error in
        // the selector must surface even when no input file was given.
        let error = run_show_object(
            None,
            false,
            &PasswordArgs::default(),
            "2147483648",
            false,
            false,
            false,
        )
        .expect_err("overflow selector with no input file");
        assert!(
            error.downcast_ref::<UsageError>().is_some(),
            "got {error} instead of a UsageError -- the missing-input-file check must not \
             run before selector parsing"
        );
    }
}
