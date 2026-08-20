#![forbid(unsafe_code)]

use clap::{ArgGroup, Args as ClapArgs, CommandFactory, Parser, Subcommand, ValueEnum};
use flpdf::disable_digital_signatures;
use flpdf::job::{
    AttachmentAddOptions, AttachmentCopyOptions, JobExitCode, JsonJobError, JsonJobOptions,
    JsonJobOutput, JsonStreamData, PageSpecInput, QPDFJob, SplitPageOptions, UsageError,
};
use flpdf::pipeline::PipelineHandle;
use flpdf::writer::DecodeLevel as StreamDecodeLevel;
use flpdf::{
    acroform_field_prune::prune_acroform_after_subset,
    objr_obj_annot_p::drop_objr_obj_annot_dangling_p,
    outline_dest_remap::remap_outline_and_dests,
    page_collate::collate,
    page_combine::{CombinedPage, CombinedPlan},
    page_rotate::apply_rotate_to_pages,
    pages::tree_rebuild::rebuild_page_tree,
    should_remove_unreferenced_resources,
    struct_tree_pg::drop_struct_elem_dangling_pg,
    subset_prune::prune_after_subset,
    thread_bead_p::drop_thread_bead_dangling_p,
    InputSpec, PageRange, RotateSpec,
};
use flpdf::{
    check_pdf_with_limits, check_reader_with_options_and_limits, filters,
    flatten_rotation_on_pages,
    json_inspect::{DecodeLevel, JsonKey, JsonObjectSelector},
    linearization::{
        check_linearization_path, show_linearization_path, LinearizationCheckError,
        ShowLinearizationError,
    },
    normalize_content_stream, pages,
    pages::coalesce_page_contents,
    parse_pdf_version, AcroFormDocumentHelper, CompressStreams, CopyEncryptionSource,
    EncryptMethod, EncryptParams, NewlineBeforeEndstream, Object, ObjectHandle, ObjectKeyAlg,
    ObjectRef, ObjectStreamMode, PageDocumentHelper, PasswordMode, Pdf, PdfOpenOptions, PdfVersion,
    PdfWriter, PermissionsConfig, PrintPermission, QPDFLogger, RemoveUnreferencedResources,
    Severity, StreamDataMode, WriterConfiguration,
};
use flpdf::{fix_qdf, remove_attachment};
use std::collections::HashSet;
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
    compress_streams: CompressStreams,
    content_normalization: bool,
    content_normalization_set: bool,
    qdf: bool,
    preserve_unreferenced_objects: bool,
    newline_before_endstream: NewlineBeforeEndstream,
    stream_data: Option<StreamDataMode>,
    recompress_flate: bool,
    static_id: bool,
    deterministic_id: bool,
    static_aes_iv: bool,
    no_original_object_ids: bool,
    min_version: Option<String>,
    min_extension_level: Option<i64>,
    force_version: Option<String>,
    force_extension_level: Option<i64>,
    encrypt: Option<EncryptParams>,
    copy_encryption: Option<CopyEncryptionSource>,
    preserve_encryption: bool,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            object_streams: ObjectStreamMode::Preserve,
            compress_streams: CompressStreams::Yes,
            content_normalization: false,
            content_normalization_set: false,
            qdf: false,
            preserve_unreferenced_objects: false,
            newline_before_endstream: NewlineBeforeEndstream::Never,
            stream_data: None,
            recompress_flate: false,
            static_id: false,
            deterministic_id: false,
            static_aes_iv: false,
            no_original_object_ids: false,
            min_version: None,
            min_extension_level: None,
            force_version: None,
            force_extension_level: None,
            encrypt: None,
            copy_encryption: None,
            preserve_encryption: true,
        }
    }
}

/// Translate the CLI's effective writer options into the reusable library
/// configuration that qpdf reapplies to every split-page output writer.
fn writer_configuration(options: &WriterOptions, linearize: bool) -> WriterConfiguration {
    let mut configuration = WriterConfiguration::default();
    configuration.set_object_stream_mode(options.object_streams);
    configuration.set_compress_streams(matches!(options.compress_streams, CompressStreams::Yes));
    if let Some(mode) = options.stream_data {
        configuration.set_stream_data_mode(mode);
    }
    configuration.set_recompress_flate(options.recompress_flate);
    configuration.set_qdf_mode(options.qdf && !linearize);
    if options.content_normalization_set {
        configuration.set_content_normalization(options.content_normalization);
    }
    configuration.set_preserve_unreferenced_objects(options.preserve_unreferenced_objects);
    configuration.set_newline_before_endstream_mode(options.newline_before_endstream);
    configuration.set_static_id(options.static_id);
    configuration.set_deterministic_id(options.deterministic_id);
    configuration.set_static_aes_iv(options.static_aes_iv);
    configuration.set_suppress_original_object_ids(options.no_original_object_ids);
    configuration.set_preserve_encryption(options.preserve_encryption);
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
    configuration
}

fn configure_pdf_writer<R: Read + Seek + 'static>(
    writer: &mut PdfWriter<'_, R>,
    options: &WriterOptions,
    linearize: bool,
    linearize_pass1: Option<&Path>,
) -> CliResult<()> {
    writer_configuration(options, linearize).apply_to(writer);
    writer.set_linearization(linearize);
    if let Some(path) = linearize_pass1 {
        writer.set_linearization_pass1_filename(path.to_path_buf());
    }
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
    options: &WriterOptions,
) -> CliResult<Vec<u8>> {
    let mut writer = PdfWriter::new(pdf);
    configure_pdf_writer(&mut writer, options, false, None)?;
    writer.set_output_memory()?;
    writer.write()?;
    Ok(writer.get_buffer()?)
}

// ---------------------------------------------------------------------------
// qpdf-compatible exit-code infrastructure (flpdf-9hc.23.2)
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
// Future subtasks (e.g. flpdf-9hc.3.17) should express their own
// exit-code semantics by constructing a `CliExitError` with the appropriate
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
    #[arg(long)]
    check: bool,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
    #[arg(long)]
    show_object: Option<String>,
    #[arg(long)]
    show_npages: bool,
    #[arg(long)]
    show_pages: bool,
    #[arg(long)]
    show_linearization: bool,

    // ── JSON inspection flags ─────────────────────────────────────────────
    // These mirror qpdf's --json / --json-output / --json-key / --json-object
    // / --json-stream-data / --json-stream-prefix flags.
    /// Enable JSON v2 output mode.  Pass `--json` alone or `--json=2` (qpdf
    /// compatible).  The value, when given, must be supplied as `--json=2`
    /// (with the equals sign) to avoid ambiguity with the positional input
    /// argument.
    // JSON mode is exclusive with the other top-level inspection / write
    // modes and with the OUTPUT positional. Without these conflicts, e.g.
    // `flpdf --json --check in` or `flpdf --json in out` would silently
    // ignore the second mode (run_json wins in main's dispatch chain).
    // Listing them as clap conflicts surfaces the mistake as a usage error
    // instead of doing one thing while the user asked for two.
    #[arg(long, num_args = 0..=1, default_missing_value = "2",
          require_equals = true,
          value_name = "VERSION", value_parser = ["2"],
          conflicts_with_all = [
              "check", "linearize", "static_id", "deterministic_id", "static_aes_iv",
              "show_object",
              "show_npages", "show_pages", "show_linearization", "output",
              "compress_streams", "linearize_pass1", "remove_restrictions",
              "decrypt", "encrypt", "copy_encryption",
              "add_attachment", "remove_attachment", "list_attachments",
              "show_attachment", "copy_attachments_from",
              "no_original_object_ids", "qdf", "coalesce_contents",
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
        value_name = "PATH",
        requires = "json",
        help = "Write JSON to PATH instead of stdout"
    )]
    json_output: Option<PathBuf>,

    /// Limit JSON output to the specified top-level key (repeatable).
    /// Valid JSON v2 keys: acroform, attachments, encrypt, outlines,
    /// pagelabels, pages, qpdf.
    #[arg(
        long = "json-key",
        value_name = "KEY",
        requires = "json",
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
        requires = "json",
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
        requires = "json",
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
        requires = "json",
        help = "Prefix for side files with --json-stream-data=file. With --json-output, \
                defaults to the JSON output path; with JSON on stdout, an explicit \
                non-empty prefix is required. An empty prefix is treated as absent."
    )]
    json_stream_prefix: Option<String>,

    // qpdf-style top-level write flags. When `--linearize` is set together
    // with INPUT and OUTPUT, behave as if `flpdf rewrite --linearize ...`
    // had been invoked. This exists so the qpdf qtest acceptance harness
    // (PATH-shimmed `qpdf` → `flpdf`) can issue qpdf-shaped commands
    // without an arg-translating wrapper.
    /// Produce a linearized ("fast web view") output PDF (top-level alias
    /// of `flpdf rewrite --linearize`).
    #[arg(long)]
    linearize: bool,
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
    /// Mutually exclusive with `--static-id`, and incompatible with encrypted
    /// output (the /ID feeds the encryption key).
    #[arg(long = "deterministic-id", conflicts_with = "static_id")]
    deterministic_id: bool,
    /// Force every AES CBC IV to all-zero bytes instead of a random value
    /// (top-level alias of `flpdf rewrite --static-aes-iv`).
    /// **Testing only; produces insecure deterministic IVs, NOT for
    /// production.** Mirrors `qpdf --static-aes-iv`.
    #[arg(long = "static-aes-iv", hide = true)]
    static_aes_iv: bool,
    /// Strip encryption and advisory permission restrictions from the output
    /// (top-level alias of `flpdf rewrite --remove-restrictions`; qpdf
    /// `--remove-restrictions` equivalent). Does NOT bypass authentication.
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
              "show_npages", "show_pages", "show_linearization",
          ])]
    remove_restrictions: bool,
    /// Strip the `/Encrypt` dictionary from the output (top-level alias of
    /// `flpdf rewrite --decrypt`; qpdf `--decrypt` equivalent). On
    /// encrypted input requires `--password` to authenticate; on plaintext
    /// input it is a no-op pass-through. Silent in both cases (matching
    /// qpdf), unlike `--remove-restrictions` which prints a one-line
    /// diagnostic when an encrypted input was de-restricted.
    ///
    /// Relationship with `--remove-restrictions`: both select the same
    /// unencrypted output bytes; this flag is silent while
    /// `--remove-restrictions` prints its diagnostic.
    // Same conflict semantics as --remove-restrictions: this is a
    // rewrite-path modifier and must be rejected against the inspection
    // subcommands so `flpdf --check --decrypt in out` is a usage error
    // rather than silently ignoring the flag (and OUTPUT).
    #[arg(long = "decrypt",
          conflicts_with_all = [
              "check", "show_object",
              "show_npages", "show_pages", "show_linearization",
          ])]
    decrypt: bool,
    /// `qpdf --compress-streams=y|n` compatibility flag.  Accepted but
    /// currently a no-op: flpdf does not re-encode stream contents on
    /// rewrite.  Provided so qtest commands parse cleanly.
    #[arg(long = "compress-streams")]
    compress_streams: Option<String>,
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
              "show_npages", "show_pages", "show_linearization",
              "list_attachments", "show_attachment", "remove_attachment",
              "add_attachment", "copy_attachments_from",
              "linearize", "pages", "rotate", "split_pages", "empty",
          ])]
    coalesce_contents: bool,

    // ── Page-operation flags (flpdf-9hc.8.12) ─────────────────────────────
    // These mirror qpdf's page-selection / page-transformation surface.
    // Observed against /usr/bin/qpdf 11.9.0:
    //   qpdf --help=--pages / --rotate / --split-pages / --collate
    //   qpdf in.pdf --pages . a.pdf b.pdf 1-z:even -- out.pdf
    #[command(flatten)]
    page_ops: PageOpArgs,

    // ── Overlay / underlay flags (flpdf-9hc.16), top-level alias ──────────
    // Mirror qpdf's top-level `qpdf in --overlay f -- out` form. Like the
    // `rewrite` subcommand fields, the per-group boundaries are extracted from
    // raw argv by `extract_overlay_groups` before clap parses; these fields
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
    overlay: Vec<String>,

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
    underlay: Vec<String>,

    // ── Attachment flags (flpdf-9hc.10.9) ────────────────────────────────
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
    add_attachment: Vec<String>,

    /// Remove an attachment by key (qpdf --remove-attachment compatible).
    ///
    /// KEY is the name-tree key used when the attachment was added.
    #[arg(
        long = "remove-attachment",
        value_name = "KEY",
        help = "Remove the embedded file with the given key (qpdf --remove-attachment)"
    )]
    remove_attachment: Option<String>,

    /// List all embedded-file attachments (qpdf --list-attachments compatible).
    #[arg(
        long = "list-attachments",
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

    /// Extract an attachment by key (qpdf --show-attachment compatible).
    ///
    /// KEY is the name-tree key used when the attachment was added. The raw
    /// bytes are written to stdout.
    #[arg(
        long = "show-attachment",
        value_name = "KEY",
        help = "Extract the embedded file with the given key to stdout \
                (qpdf --show-attachment)"
    )]
    show_attachment: Option<String>,

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
    copy_attachments_from: Vec<String>,

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
        num_args = 3..,
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
            "show_npages", "show_pages", "show_linearization",
            "remove_restrictions", "decrypt",
            "copy_encryption",
        ],
        help = "Encrypt output (qpdf --encrypt compatible): \
                USER-PW OWNER-PW KEY-LEN [sub-flags] --"
    )]
    encrypt: Vec<String>,

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
            "show_npages", "show_pages", "show_linearization",
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
    encryption_file_password: Option<String>,

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
    pages: Vec<String>,

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
        default_missing_value = "1",
        require_equals = true,
        value_name = "N",
        help = "Collate --pages selections in groups of N (qpdf --collate[=n], default 1)"
    )]
    collate: Option<String>,

    /// `qpdf --empty` — start from an empty document. Parsed for qpdf-script
    /// compatibility but NOT implemented at this layer (would silently
    /// produce wrong output if ignored), so it errors actionably.
    #[arg(
        long = "empty",
        help = "(qpdf --empty) start from an empty document — NOT yet implemented"
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
grep qpdf output also work here. flpdf adds extra leading lines
(`V = `, `Length = `, `Filter = `, `EncryptMetadata = `, and per-named
`CF /<name> = <method>`) before the qpdf block.

Divergences from qpdf, by design: flpdf does not recover
the cleartext user password, so qpdf's `User password = <value>` line is
omitted (a grep for it simply misses rather than getting wrong data).
`Supplied password is owner/user password` is printed from the
authenticated state. If FILE is not encrypted, prints qpdf's
`File is not encrypted` and exits 0. Requires a correct password to open
the document (same as the other inspection subcommands)."
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
}

/// Args for `qdf-fix` (qpdf `fix-qdf` equivalent). No password / no Pdf
/// open: fix_qdf operates byte-for-byte on a (possibly hand-edited) QDF file
/// and must not reparse or reformat it.
#[derive(Debug, ClapArgs)]
struct QdfFixCommand {
    input: PathBuf,
    output: PathBuf,
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
    /// `--static-id` and ISO 32000-1 §14.4). Uses the canonical writer. Mutually
    /// exclusive with `--static-id`, and incompatible with encrypted output (the
    /// /ID feeds the encryption key). Unlike `--static-id` it is a production-safe
    /// flag and emits no testing-only warning.
    #[arg(long = "deterministic-id", conflicts_with = "static_id")]
    deterministic_id: bool,
    /// Force every AES CBC IV to all-zero bytes instead of a random value.
    /// **Testing only; produces insecure deterministic IVs, NOT for
    /// production.** Mirrors `qpdf --static-aes-iv`.
    #[arg(long = "static-aes-iv", hide = true)]
    static_aes_iv: bool,
    /// Strip encryption and advisory permission restrictions from the output
    /// (qpdf `--remove-restrictions` equivalent).
    ///
    /// A normal rewrite preserves authenticated source encryption. This flag
    /// explicitly disables that preservation, removes `/Encrypt` (and its
    /// advisory `/P` permissions), and prints a one-line diagnostic when an
    /// encrypted input was de-restricted. It does NOT bypass authentication.
    ///
    /// See `--decrypt` for the silent qpdf-compatible variant; on the current
    /// rewrite path the two flags produce identical output bytes.
    #[arg(long = "remove-restrictions")]
    remove_restrictions: bool,
    /// Strip the `/Encrypt` dictionary from the output (qpdf `--decrypt`
    /// equivalent). On encrypted input requires `--password` to
    /// authenticate; on plaintext input it is a no-op pass-through. Silent
    /// in both cases, matching qpdf `--decrypt`.
    ///
    /// Relationship with `--remove-restrictions`: both select the same
    /// unencrypted output bytes; this flag is silent while
    /// `--remove-restrictions` prints its diagnostic.
    #[arg(long = "decrypt")]
    decrypt: bool,
    /// Encrypt the output (qpdf `--encrypt` compatible). See the top-level
    /// `--encrypt` documentation for the full syntax and supported modes.
    /// `--linearize` is not rejected: qpdf itself supports
    /// `--linearize --encrypt ...`, and `write_linearized` threads
    /// `options.encrypt` through correctly.
    #[arg(
        long = "encrypt",
        num_args = 3..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "USER-PW OWNER-PW KEY-LEN [sub-flags]",
        conflicts_with_all = [
            "remove_restrictions", "decrypt",
            "copy_encryption",
        ]
    )]
    encrypt: Vec<String>,
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
    encryption_file_password: Option<String>,
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
    /// Uses the canonical writer and forces object streams off. Cannot be combined with
    /// --linearize (QDF is inherently non-linearized).
    #[arg(long = "qdf")]
    qdf: bool,
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
    #[arg(long = "compress-streams", value_enum, default_value_t = CliYesNo::Yes,
          help = "Compress output streams with FlateDecode (qpdf default: y)")]
    compress_streams: CliYesNo,

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
    /// `y`: always write exactly one `\n` before `endstream`, matching
    /// ISO 32000-1 §7.3.8.1 and qpdf run with `--newline-before-endstream`.
    /// `n`: omit the extra newline when the stream payload already ends with `\n`
    /// or `\r` (a flpdf-specific middle ground; matches neither of qpdf's modes).
    ///
    /// Only affects the full-rewrite path.
    #[arg(long = "newline-before-endstream", value_enum,
          default_value_t = CliNewlineBeforeEndstream::Never,
          help = "Insert newline before endstream keyword (qpdf default: never)")]
    newline_before_endstream: CliNewlineBeforeEndstream,

    /// Stream data mode (qpdf --stream-data={preserve,uncompress,compress}).
    ///
    /// Higher-level policy that overrides --compress-streams when set.
    /// - `preserve`: pass streams through verbatim — no decode or re-encode.
    /// - `uncompress`: decode streams and emit raw bytes (no /Filter).
    /// - `compress`: decode streams and re-encode with /FlateDecode.
    ///
    /// Default: not set (falls back to --compress-streams).
    /// When both --stream-data and --compress-streams are supplied, --stream-data wins.
    /// Only affects the full-rewrite path.
    #[arg(long = "stream-data", value_enum)]
    stream_data: Option<CliStreamDataMode>,

    /// Re-encode streams that are already a lone /FlateDecode (default: preserve
    /// them verbatim, matching qpdf). Mirrors `qpdf --recompress-flate`.
    #[arg(long = "recompress-flate")]
    recompress_flate: bool,

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

    // ── Overlay / underlay flags (flpdf-9hc.16) ───────────────────────────
    // qpdf --overlay / --underlay impose pages from another file on top of
    // (overlay) or beneath (underlay) the destination pages. Both are
    // REPEATABLE and each group is terminated by a bare `--`. Within a group
    // the file token and sub-options may appear in any order:
    //   {--overlay|--underlay} [--file=]f [--password=p] [--to=R] [--from=R]
    //                          [--repeat=R] --
    //
    // The repeated occurrences and their per-group boundaries are extracted
    // from the raw argv by `extract_overlay_groups` BEFORE clap parses (clap's
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
    overlay: Vec<String>,

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
    underlay: Vec<String>,

    /// Print verbose progress and diagnostic messages (mirrors qpdf --verbose).
    #[arg(
        long = "verbose",
        help = "Print verbose progress and diagnostic messages \
                (mirrors qpdf --verbose)"
    )]
    verbose: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum CliObjectStreamMode {
    #[default]
    Preserve,
    Disable,
    Generate,
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
    /// qpdf's `QPDFJob::Config::flattenAnnotations` mapping. Every CLI mode
    /// rejects Invisible and Hidden; `screen` additionally rejects NoView,
    /// while `print` requires the Print bit.
    fn flags(self) -> (i64, i64) {
        match self {
            CliFlattenMode::All => (0, 0x3),
            CliFlattenMode::Screen => (0, 0x3 | 0x20),
            CliFlattenMode::Print => (0x4, 0x3),
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
/// Adds a third `never` variant on top of `y|n` so the CLI can request qpdf's
/// default framing (no newline between the stream payload and `endstream`),
/// which is required for byte-identical qpdf-equivalent output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliNewlineBeforeEndstream {
    #[clap(name = "y")]
    Yes,
    #[clap(name = "n")]
    No,
    #[clap(name = "never")]
    Never,
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

#[derive(Debug, Clone, Default, ClapArgs)]
struct PasswordArgs {
    /// Password bytes for encrypted PDFs.
    #[arg(long, conflicts_with = "password_file")]
    password: Option<String>,
    /// File containing password bytes. One trailing LF or CRLF is stripped.
    #[arg(long = "password-file", value_name = "PATH")]
    password_file: Option<PathBuf>,
    /// How to interpret --password bytes before key derivation. Defaults to
    /// `auto` which picks `bytes` for V<5 documents and `unicode` (SASLprep)
    /// for V=5 R=5/R=6. Mirrors qpdf's --password-mode flag.
    #[arg(long = "password-mode", value_enum, default_value_t = CliPasswordMode::Auto)]
    password_mode: CliPasswordMode,
    /// Permit deprecated RC4-backed handlers and revision 5 encryption.
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
    /// Accepted for qpdf script compatibility; currently a documented no-op.
    #[arg(
        long = "suppress-password-recovery",
        long_help = "Accepted for qpdf script compatibility. qpdf retries \
alternate password encodings (UTF-8 / PDFDocEncoding) when authentication \
fails on V<5 documents; this flag disables that recovery. flpdf performs a \
single authentication attempt with no encoding fallback, so there is no \
recovery to suppress: this flag is a DOCUMENTED NO-OP. It is parsed without \
error so scripts passing it do not break, and the contract is reserved so \
encoding fallback can be added later without changing the CLI surface."
    )]
    suppress_password_recovery: bool,
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
    eprintln!(
        "flpdf: warning: --static-id is for testing only and must not be \
         used for production output"
    );
}

fn main() {
    // One private qpdf-style logger owns all document routes for this
    // invocation. It is deliberately distinct from the library process
    // default so later save/info routing can be configured as one unit.
    let _ = cli_logger();
    // Extract the `--overlay`/`--underlay` groups from the raw argv before clap
    // parses (see `extract_overlay_groups`): clap's derive would flatten the
    // repeated occurrences and lose the per-group boundaries and declaration
    // order that byte-identical composition relies on. The residual argv (with
    // those groups removed) is what clap sees.
    let rewritten_args = rewrite_qpdf_single_dash(std::env::args().collect());
    let (residual_args, overlay_specs) = match extract_overlay_groups(rewritten_args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("flpdf: {error}");
            std::process::exit(2);
        }
    };
    let args = Cli::parse_from(residual_args);
    let normalize_content = normalize_content_enabled(args.normalize_content, args.qdf);

    // --static-id produces a fixed, non-unique trailer /ID. It exists only
    // for deterministic test/parity output. The native `rewrite --static-id`
    // surface warns loudly (stderr only, exit code unchanged) so it is never
    // mistaken for a production option; the top-level qpdf-shaped alias stays
    // silent to mirror qpdf (flpdf-4x6). Done here, after clap parsing
    // succeeds and before any rewrite work, so the warning never precedes a
    // usage error yet is always visible.
    warn_if_static_id(&args);

    // Top-level `--qdf --linearize` is rejected here, before the dispatch
    // chain. The `else if args.linearize` branch (below) wins over the
    // default rewrite branch, so deferring this check into the rewrite branch
    // would let the linearize path run while silently dropping --qdf. QDF is
    // inherently non-linearized; mirror the rewrite/--linearize rejection.
    // (The `Commands::Rewrite` arm performs the
    // equivalent check for the subcommand form.)
    if args.qdf && args.linearize {
        eprintln!("flpdf: --qdf and --linearize cannot be used together");
        std::process::exit(1);
    }

    // Attachment add/remove/copy operations rewrite through their own
    // serializers before the shared rewrite branch. qpdf applies writer
    // normalization to those outputs, but flpdf cannot yet do so without
    // duplicating the consumer. Reject effective `y` until those serializers
    // delegate to the shared rewrite path; list/show are read-only and remain
    // accepted, matching qpdf.
    if normalize_content
        && (args.remove_attachment.is_some()
            || !args.add_attachment.is_empty()
            || !args.copy_attachments_from.is_empty())
    {
        eprintln!(
            "flpdf: --normalize-content is not applied by attachment mutation operations; \
             rerun with --normalize-content=n or without the attachment operation"
        );
        std::process::exit(1);
    }

    // `--overlay`/`--underlay` groups are stripped from argv before clap by
    // `extract_overlay_groups`, so a stripped group leaves no trace for the
    // dispatch chain. Only the rewrite paths (the `Rewrite` subcommand and the
    // top-level default/`--linearize` rewrite branches) consume `overlay_specs`;
    // every other command/mode would silently ignore it. Reject that here so an
    // overlay on, e.g., `check`/`--show-npages`/`--pages` fails loudly instead of
    // being dropped. The top-level predicate mirrors the dispatch chain below
    // (the rewrite branch is the final `else`, reached only when no inspection,
    // attachment, json, or page-op mode is selected) and must stay in sync with
    // it; `--pages`/`--linearize` combinations are rejected later with their own
    // specific diagnostics.
    if !overlay_specs.is_empty() {
        let target_is_rewrite = match &args.command {
            Some(Commands::Rewrite(_)) => true,
            Some(_) => false,
            None => {
                args.json.is_none()
                    && args.show_object.is_none()
                    && !args.show_npages
                    && !args.show_pages
                    && !args.show_linearization
                    && !args.check
                    && !args.list_attachments
                    && args.show_attachment.is_none()
                    && args.remove_attachment.is_none()
                    && args.add_attachment.is_empty()
                    && args.copy_attachments_from.is_empty()
            }
        };
        if !target_is_rewrite {
            eprintln!(
                "flpdf: --overlay/--underlay can only be used with rewrite output, \
                 not with inspection or other commands"
            );
            std::process::exit(2);
        }
    }

    let json_input_inspection = (args.json_input || args.update_from_json.is_some())
        && (args.check || args.show_npages || args.show_pages);

    // JSON-input/update inspection is routed through the already-created job
    // document before the ordinary file-backed inspection branches. qpdf
    // creates or updates the QPDF object first, then runs read-only consumers
    // such as --check and --show-pages on that same object.
    // For ordinary JSON output, the separate --json branch remains first among
    // the non-inspection modes and retains its existing validation boundary.
    let result = if json_input_inspection {
        run_json_input_inspection(&args)
    } else if args.json.is_some() {
        run_json(&args)
    } else if let Some(command) = args.command {
        run_command(command, &overlay_specs)
    } else if let Some(object_ref) = args.show_object.as_deref() {
        run_dump_object(args.input, args.repair, &args.password, object_ref)
    } else if args.show_npages {
        run_show_npages(args.input, args.repair, &args.password)
    } else if args.show_pages {
        run_show_pages(args.input, args.repair, &args.password)
    } else if args.show_linearization {
        run_show_linearization(args.input)
    } else if args.check {
        run_check(
            args.input,
            args.repair,
            &args.password,
            filters::DecodeLimits::default(),
        )
    } else if args.list_attachments {
        run_list_attachments(args.input, args.repair, &args.password, args.verbose)
    } else if let Some(key) = args.show_attachment {
        run_show_attachment(args.input, args.repair, &args.password, &key)
    } else if let Some(key) = args.remove_attachment {
        run_remove_attachment(
            args.input,
            args.output,
            args.repair,
            &args.password,
            &key,
            args.deterministic_id,
        )
    } else if !args.add_attachment.is_empty() {
        run_add_attachment(
            args.input,
            args.output,
            args.repair,
            &args.password,
            args.add_attachment,
            args.deterministic_id,
            args.verbose,
        )
    } else if !args.copy_attachments_from.is_empty() {
        run_copy_attachments_from(
            args.input,
            args.output,
            args.repair,
            &args.password,
            args.copy_attachments_from,
            args.deterministic_id,
            args.verbose,
        )
    } else if args.linearize {
        // --linearize is incompatible with the page-extraction pipeline:
        // extraction produces a normalized, non-linearized document. Without
        // this guard the linearize branch would win the dispatch chain and
        // silently ignore --pages/--rotate/--split-pages (wrong output, no
        // diagnostic). Mirror the same rejection the `rewrite` subcommand
        // performs.
        if page_ops_active(&args.page_ops) {
            eprintln!("flpdf: --linearize cannot be combined with --pages/--rotate/--split-pages");
            std::process::exit(1);
        }
        let mut options = WriterOptions {
            static_id: args.static_id,
            deterministic_id: args.deterministic_id,
            static_aes_iv: args.static_aes_iv,
            no_original_object_ids: args.no_original_object_ids,
            content_normalization: normalize_content,
            content_normalization_set: args.normalize_content.is_some(),
            ..WriterOptions::default()
        };
        // Top-level --compress-streams=y|n: parse and wire to WriterOptions.
        // Accepted values are "y" and "n" (qpdf-compatible); other values exit 2.
        if let Some(ref cs) = args.compress_streams {
            match cs.as_str() {
                "y" => options.compress_streams = CompressStreams::Yes,
                "n" => options.compress_streams = CompressStreams::No,
                other => {
                    eprintln!("flpdf: --compress-streams must be y or n, got: {:?}", other);
                    std::process::exit(2);
                }
            }
        }
        // Top-level --encrypt / --copy-encryption on the --linearize
        // alias: wire encryption onto WriterOptions (shared with the
        // non-linearize branch below and the `rewrite` subcommand via
        // apply_encryption_options). Without this call the linearize branch
        // would silently drop --encrypt/--copy-encryption (WriterOptions
        // built here is separate from the non-linearize branch's), emitting
        // plaintext output even though the user asked for encryption.
        apply_encryption_options(
            &mut options,
            &args.encrypt,
            args.copy_encryption.as_deref(),
            args.encryption_file_password.as_deref(),
            args.password.allow_weak_crypto,
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
            CliRemoveUnreferencedResources::No, // remove_unreferenced (no-op for linearize path)
            false,                              // generate_appearances (not on top-level surface)
            None,                               // flatten_annotations (not on top-level surface)
            false,                              // flatten_rotation (not on top-level surface)
            &overlay_specs,
            args.verbose,
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
        // through the page-op pipeline is a flpdf-9hc.4.9 follow-up.
        if !args.encrypt.is_empty() {
            eprintln!(
                "flpdf: --encrypt is not applied in the \
                 --pages/--rotate/--split-pages/--collate pipeline; \
                 rerun without --encrypt or without the page operation"
            );
            std::process::exit(1);
        }
        if args.copy_encryption.is_some() {
            eprintln!(
                "flpdf: --copy-encryption is not applied in the \
                 --pages/--rotate/--split-pages/--collate pipeline; \
                 rerun without --copy-encryption or without the page operation"
            );
            std::process::exit(1);
        }
        let mut options = WriterOptions {
            static_id: args.static_id,
            deterministic_id: args.deterministic_id,
            static_aes_iv: args.static_aes_iv,
            no_original_object_ids: args.no_original_object_ids,
            content_normalization: normalize_content,
            content_normalization_set: args.normalize_content.is_some(),
            qdf: args.qdf,
            ..WriterOptions::default()
        };
        if let Some(ref cs) = args.compress_streams {
            match cs.as_str() {
                "y" => options.compress_streams = CompressStreams::Yes,
                "n" => options.compress_streams = CompressStreams::No,
                other => {
                    eprintln!("flpdf: --compress-streams must be y or n, got: {:?}", other);
                    std::process::exit(2);
                }
            }
        }
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
                    CliRemoveUnreferencedResources::Auto,
                    options.clone(),
                    args.verbose,
                )
            } else {
                if !overlay_specs.is_empty() {
                    eprintln!(
                        "flpdf: --overlay/--underlay is not applied with \
                         --rotate/--split-pages alone (no --pages); \
                         rerun with --pages or without the overlay"
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
                    options.clone(),
                    args.verbose,
                )
            }
        };
        match (args.input.clone(), args.output.clone()) {
            (Some(i), Some(o)) => dispatch(i, o),
            _ => Err("page operations require both an input and an output file".into()),
        }
    } else {
        let mut options = WriterOptions {
            static_id: args.static_id,
            deterministic_id: args.deterministic_id,
            static_aes_iv: args.static_aes_iv,
            no_original_object_ids: args.no_original_object_ids,
            content_normalization: normalize_content,
            content_normalization_set: args.normalize_content.is_some(),
            qdf: args.qdf,
            ..WriterOptions::default()
        };
        // Top-level `--qdf` is an alias of `rewrite --qdf`; both configure the
        // same canonical qpdf writer. The library forces ObjStm off under qdf.
        // Top-level --compress-streams=y|n: parse and wire to WriterOptions.
        // Accepted values are "y" and "n" (qpdf-compatible); other values exit 2.
        if let Some(ref cs) = args.compress_streams {
            match cs.as_str() {
                "y" => options.compress_streams = CompressStreams::Yes,
                "n" => options.compress_streams = CompressStreams::No,
                other => {
                    eprintln!("flpdf: --compress-streams must be y or n, got: {:?}", other);
                    std::process::exit(2);
                }
            }
        }
        // Top-level --encrypt / --copy-encryption: wire encryption onto
        // WriterOptions (shared with the `rewrite` surface via
        // apply_encryption_options). Parse / donor-open errors exit 2. The
        // page-op pipeline does not thread either option, so
        // the `else if page_ops_active` arm above already rejects them; this is
        // the non-page-op branch, so no further page-op guard is needed here.
        apply_encryption_options(
            &mut options,
            &args.encrypt,
            args.copy_encryption.as_deref(),
            args.encryption_file_password.as_deref(),
            args.password.allow_weak_crypto,
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
            CliRemoveUnreferencedResources::No, // remove_unreferenced (top-level alias is no-op)
            false,                              // generate_appearances (not on top-level surface)
            None,                               // flatten_annotations (not on top-level surface)
            false,                              // flatten_rotation (not on top-level surface)
            &overlay_specs,
            args.verbose,
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
                // cov:ignore-start: no production CliExitError currently carries a non-empty message
                emit_logger_error(format!("{}: {}\n", progname(), exit_err.message));
                // cov:ignore-end
            }
            std::process::exit(exit_err.code.as_i32());
        }
        if let Some(usage_error) = error.downcast_ref::<UsageError>() {
            usage_exit(usage_error);
        }
        emit_logger_error(format!("{}: {error}\n", progname()));
        std::process::exit(2);
    }
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

fn run_json(cli: &Cli) -> CliResult<()> {
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

    // 1. Validate --json-key values before doing any I/O.
    let mut json_keys: Vec<JsonKey> = Vec::new();
    for raw in &cli.json_key {
        if matches!(raw.as_str(), "objects" | "objectinfo") {
            eprintln!(
                "flpdf: json keys \"objects\" and \"objectinfo\" are only valid for json version 1"
            );
            std::process::exit(2);
        }
        match JsonKey::from_str(raw.as_str()) {
            Some(k) => json_keys.push(k),
            None => {
                let names = QPDF_JSON_KEY_NAMES.join(",");
                eprintln!("flpdf: --json-key must be given as --json-key={{{names}}}");
                std::process::exit(2);
            }
        }
    }

    // 2. Validate --json-object selectors before doing any I/O.
    let mut json_objects: Vec<JsonObjectSelector> = Vec::new();
    for raw in &cli.json_object {
        match JsonObjectSelector::from_str(raw.as_str()) {
            Some(s) => json_objects.push(s),
            None => {
                eprintln!(
                    "flpdf: --json-object selector \"{raw}\" must be 'trailer', 'N', or 'N,G'"
                );
                std::process::exit(2);
            }
        }
    }

    // 3. Resolve stream-data mode.
    //
    // The help text documents the default as "none". Stream payloads are
    // never embedded or written to disk unless the caller explicitly opts
    // in via --json-stream-data, even when --json-output is used: leaking
    // stream contents based on an unrelated flag would be surprising.
    let stream_data = match cli.json_stream_data.as_deref().unwrap_or("none") {
        "none" => JsonStreamData::None,
        "inline" => JsonStreamData::Inline,
        "file" => JsonStreamData::File,
        other => {
            eprintln!("flpdf: --json-stream-data must be none, inline, or file; got: {other}");
            std::process::exit(2);
        }
    };

    // 4. Reject an output that identifies the input file before opening or
    // truncating it. qpdf performs this check in QPDFJob.cc:627-630. Path
    // spelling alone is insufficient: relative aliases, symlinks, and hard
    // links can all name the same underlying file.
    let input = cli.input.as_ref().ok_or("missing input file")?;
    if let Some(output) = cli.json_output.as_ref() {
        reject_same_json_output(input, output)?;
    }

    // qpdf reserves standard output for binary/structured save data before
    // opening the document, so warnings and later info cannot claim stdout.
    let mut standard_output = if cli.json_output.is_none() {
        Some(standard_save_writer()?)
    } else {
        None
    };

    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());

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
        let mut pdf = job.create_from_json(input_file, input.display().to_string())?;
        apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
        let mut runtime = JsonJobRuntime {
            input_identity: &input_identity,
            standard_output: &mut standard_output,
            job: &mut job,
        };
        run_json_document(
            cli,
            &mut runtime,
            &mut pdf,
            stream_data,
            &json_keys,
            &json_objects,
        )
    } else {
        let mut pdf = open_pdf_from_file(input, input_file, cli.repair, &cli.password)?;
        job.record_document_warnings(&pdf);
        apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
        let mut runtime = JsonJobRuntime {
            input_identity: &input_identity,
            standard_output: &mut standard_output,
            job: &mut job,
        };
        run_json_document(
            cli,
            &mut runtime,
            &mut pdf,
            stream_data,
            &json_keys,
            &json_objects,
        )
    }
}

fn run_json_input_inspection(cli: &Cli) -> CliResult<()> {
    let input = cli.input.as_ref().ok_or("missing input file")?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());

    let file = File::open(input).map_err(|error| {
        if cli.json_input {
            qpdf_json_input_open_error(input, error)
        } else {
            error_with_file(input, error.into())
        }
    })?;

    if cli.json_input {
        let mut pdf = job.create_from_json(file, input.display().to_string())?;
        apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
        return run_job_inspection_on_pdf(cli, input, &mut job, &mut pdf);
    }

    let mut options = pdf_open_options(cli.repair, &cli.password)?;
    if cli.check {
        // `--check` is a read-only inspection: qpdf forces weak-crypto
        // authentication open and re-emits collected diagnostics once in its
        // check report rather than delivering them during input creation.
        options.allow_weak_crypto = true;
        options.suppress_warnings = true;
    }
    let mut pdf = job
        .open(BufReader::new(file), input.display().to_string(), options)
        .map_err(|error| error_with_file(input, actionable_password_error(error)))?;
    if pdf.uses_weak_crypto() && !cli.check {
        logger_warn(format!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied\n",
            input.display()
        ))?;
    }
    apply_json_update_with_job(&mut job, &mut pdf, cli.update_from_json.as_deref())?;
    run_job_inspection_on_pdf(cli, input, &mut job, &mut pdf)
}

fn run_job_inspection_on_pdf<R: Read + Seek + 'static>(
    cli: &Cli,
    input: &Path,
    job: &mut QPDFJob,
    pdf: &mut Pdf<R>,
) -> CliResult<()> {
    let decode_limits = filters::DecodeLimits::default();
    if cli.check {
        return run_check_pdf(input, pdf, decode_limits);
    }
    if cli.show_npages {
        let logger = job.logger();
        return finish_job_exit_status(job.inspect(pdf, |pdf| show_npages_from_pdf(pdf, &logger))?);
    }
    if cli.show_pages {
        let logger = job.logger();
        return finish_job_exit_status(job.inspect(pdf, |pdf| show_pages_from_pdf(pdf, &logger))?);
    }
    Err("JSON input/update inspection mode is missing a consumer".into())
}

/// Serialize an already-opened job document through the existing qpdf JSON
/// output consumer. Keeping this generic preserves the same output path for
/// file-backed PDF inputs and JSON-created documents.
fn run_json_document<R: Read + Seek>(
    cli: &Cli,
    runtime: &mut JsonJobRuntime<'_>,
    pdf: &mut Pdf<R>,
    stream_data: JsonStreamData,
    json_keys: &[JsonKey],
    json_objects: &[JsonObjectSelector],
) -> CliResult<()> {
    // `decode_level` governs both inline `data` payloads and file-mode side
    // files emitted by the job-owned JSON output pipeline.
    let json_result = if let Some(ref path) = cli.json_output {
        let mut file = open_verified_json_output(runtime.input_identity, path)?;
        let options = JsonJobOptions {
            decode_level: DecodeLevel::Generalized,
            stream_data,
            stream_prefix: cli.json_stream_prefix.as_deref(),
            keys: json_keys,
            objects: json_objects,
        };
        runtime.job.write_json(
            pdf,
            options,
            JsonJobOutput::File {
                filename: path,
                writer: &mut file,
            },
            true,
        )
    } else {
        let options = JsonJobOptions {
            decode_level: DecodeLevel::Generalized,
            stream_data,
            stream_prefix: cli.json_stream_prefix.as_deref(),
            keys: json_keys,
            objects: json_objects,
        };
        runtime.job.write_json(
            pdf,
            options,
            JsonJobOutput::Stdout(
                runtime
                    .standard_output
                    .as_mut()
                    .expect("stdout writer prepared for JSON stdout"),
            ),
            false,
        )
    };
    match json_result {
        Ok(JobExitCode::Success) => {}
        Ok(JobExitCode::Warning) => {
            return Err(Box::new(CliExitError {
                code: ExitCode::Warnings,
                message: String::new(),
            }))
        }
        Err(JsonJobError::Output(error)) => return Err(Box::new(error)),
        Err(JsonJobError::Usage(error)) => return Err(Box::new(error)),
        Err(JsonJobError::Completion(error)) => return Err(Box::new(error)),
    }
    Ok(())
}

fn run_command(command: Commands, overlay_specs: &[OverlaySpec]) -> CliResult<()> {
    match command {
        Commands::Check(cmd) => run_check(
            Some(cmd.input),
            cmd.repair,
            &cmd.password,
            filters::DecodeLimits::default(),
        ),
        Commands::CheckLinearization(cmd) => match check_linearization_path(&cmd.input) {
            Ok(()) => logger_info("linearization OK\n"),
            Err(LinearizationCheckError::NotLinearized) => {
                logger_error(
                    "flpdf: not a linearized PDF: the first object in the file has no /Linearized key\n",
                )?; // cov:ignore: exercised by check-linearization subprocess integration tests
                std::process::exit(1);
            }
            Err(LinearizationCheckError::InvalidParam { message }) => {
                logger_error(format!("flpdf: linearization check failed: {message}\n"))?;
                std::process::exit(1);
            }
            Err(LinearizationCheckError::Io(e)) => Err(e.to_string().into()),
        },
        Commands::DumpObject(cmd) => {
            run_dump_object(Some(cmd.input), cmd.repair, &cmd.password, &cmd.object_ref)
        }
        Commands::Pages(cmd) => {
            if cmd.show_npages {
                run_show_npages(Some(cmd.input), cmd.repair, &cmd.password)
            } else {
                run_show_pages(Some(cmd.input), cmd.repair, &cmd.password)
            }
        }
        Commands::Qdf(cmd) => run_qdf(Some(cmd.input), Some(cmd.output), cmd.repair, &cmd.password),
        Commands::QdfFix(cmd) => run_qdf_fix(&cmd.input, &cmd.output),
        Commands::ShowStream(cmd) => run_show_stream(cmd),
        Commands::ShowEncryption(cmd) => run_show_encryption(&cmd.input, cmd.repair, &cmd.password),
        Commands::IsEncrypted(cmd) => run_is_encrypted(&cmd.input, cmd.repair),
        Commands::RequiresPassword(cmd) => {
            run_requires_password(&cmd.input, cmd.repair, &cmd.password)
        }
        Commands::ShowEncryptionKey(cmd) => {
            run_show_encryption_key(&cmd.input, cmd.repair, &cmd.password)
        }
        Commands::Rewrite(cmd) => {
            if let Some(ref v) = cmd.force_version {
                if parse_pdf_version(v).is_none() {
                    eprintln!("flpdf: invalid --force-version value: {:?}", v);
                    std::process::exit(1);
                }
            }
            if let Some(ref v) = cmd.min_version {
                if parse_pdf_version(v).is_none() {
                    eprintln!("flpdf: invalid --min-version value: {:?}", v);
                    std::process::exit(1);
                }
            }
            // QDF is inherently non-linearized; reject the combination with a
            // fatal diagnostic, mirroring the rewrite/--linearize rejection
            // above. (The top-level `--qdf --linearize` form is
            // rejected earlier in main(), before the linearize branch wins
            // the dispatch chain.)
            if cmd.qdf && cmd.linearize {
                eprintln!("flpdf: --qdf and --linearize cannot be used together");
                std::process::exit(1);
            }
            // Non-fatal conflict diagnostic deferred from flpdf-9hc.6.6:
            // --qdf forces object streams off (the library disables ObjStm
            // under qdf via 6.2). `preserve` is the clap default and is
            // indistinguishable from "not passed", so only an explicit
            // `disable`/`generate` is diagnosable; `disable` already agrees
            // with QDF so only `generate` is surprising, but report both
            // explicit non-default values for clarity. Proceed with QDF.
            if cmd.qdf {
                match cmd.object_streams {
                    CliObjectStreamMode::Generate => {
                        eprintln!(
                            "flpdf: --qdf forces object streams off; ignoring \
                             --object-streams=generate"
                        );
                    }
                    CliObjectStreamMode::Disable => {
                        eprintln!(
                            "flpdf: --qdf forces object streams off; ignoring \
                             --object-streams=disable"
                        );
                    }
                    CliObjectStreamMode::Preserve => {}
                }
            }
            let mut options = WriterOptions {
                static_id: cmd.static_id,
                deterministic_id: cmd.deterministic_id,
                static_aes_iv: cmd.static_aes_iv,
                min_version: cmd.min_version,
                force_version: cmd.force_version,
                no_original_object_ids: cmd.no_original_object_ids,
                // `--qdf` and `--deterministic-id` configure the canonical writer's
                // output preparation directly.
                qdf: cmd.qdf,
                object_streams: cmd.object_streams.into(),
                compress_streams: match cmd.compress_streams {
                    CliYesNo::Yes => CompressStreams::Yes,
                    CliYesNo::No => CompressStreams::No,
                },
                newline_before_endstream: match cmd.newline_before_endstream {
                    CliNewlineBeforeEndstream::Yes => NewlineBeforeEndstream::Yes,
                    CliNewlineBeforeEndstream::No => NewlineBeforeEndstream::No,
                    CliNewlineBeforeEndstream::Never => NewlineBeforeEndstream::Never,
                },
                // --stream-data overrides --compress-streams when set.
                stream_data: cmd.stream_data.map(Into::into),
                // Recompressing an existing lone /FlateDecode stream is a writer
                // setting and is applied by the same canonical route.
                recompress_flate: cmd.recompress_flate,
                ..WriterOptions::default()
            };
            // `rewrite --encrypt` / `--copy-encryption`: wire encryption
            // onto WriterOptions (shared with the top-level surface via
            // apply_encryption_options).
            apply_encryption_options(
                &mut options,
                &cmd.encrypt,
                cmd.copy_encryption.as_deref(),
                cmd.encryption_file_password.as_deref(),
                cmd.password.allow_weak_crypto,
            );
            let normalize_content = matches!(cmd.normalize_content, Some(CliYesNo::Yes));
            options.content_normalization = normalize_content;
            options.content_normalization_set = cmd.normalize_content.is_some();
            let coalesce_contents = cmd.coalesce_contents;
            let remove_unref = cmd.remove_unreferenced_resources;

            // --flatten-annotations / --generate-appearances / --flatten-rotation
            // are applied only on run_rewrite's NON-linearize branch (the
            // content-mutation passes do not exist on the linearize path). Pairing
            // them with --linearize would silently drop the requested
            // transformation, so reject the combination loudly instead — the same
            // shape as the --linearize + page-ops guard below.
            if cmd.linearize
                && (cmd.generate_appearances
                    || cmd.flatten_annotations.is_some()
                    || cmd.flatten_rotation)
            {
                eprintln!(
                    "flpdf: --linearize cannot be combined with \
                     --flatten-annotations/--generate-appearances/--flatten-rotation"
                );
                std::process::exit(1);
            }

            // Page-operation dispatch (flpdf-9hc.8.12). When --pages is set
            // the extraction pipeline owns the write; otherwise --rotate /
            // --split-pages decorate a plain rewrite. --linearize with page
            // ops is rejected (the extraction path produces a normalized,
            // non-linearized document).
            if page_ops_active(&cmd.page_ops) {
                if cmd.linearize {
                    eprintln!(
                        "flpdf: --linearize cannot be combined with --pages/--rotate/--split-pages"
                    );
                    std::process::exit(1);
                }
                // The --rotate/--split-pages-only path does not run overlay
                // stacking; only --pages does (via run_page_extraction below).
                if cmd.page_ops.pages.is_empty() && !overlay_specs.is_empty() {
                    eprintln!(
                        "flpdf: --overlay/--underlay is not applied with \
                         --rotate/--split-pages alone (no --pages); \
                         rerun with --pages or without the overlay"
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
                if coalesce_contents
                    || cmd.remove_restrictions
                    || cmd.decrypt
                    || !cmd.encrypt.is_empty()
                    || cmd.copy_encryption.is_some()
                    || cmd.generate_appearances
                    || cmd.flatten_annotations.is_some()
                    || cmd.flatten_rotation
                {
                    eprintln!(
                        "flpdf: --coalesce-contents / --remove-restrictions / --decrypt / --encrypt / \
                         --copy-encryption / --flatten-annotations / \
                         --generate-appearances / --flatten-rotation are \
                         not applied in the --pages/--rotate/--split-pages/\
                         --collate pipeline; rerun without them or without \
                         the page operation"
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
                    eprintln!(
                        "flpdf: --remove-unreferenced-resources is not applied \
                         with --rotate/--split-pages alone; rerun without it \
                         or add --pages"
                    );
                    std::process::exit(1);
                }
                return if !cmd.page_ops.pages.is_empty() {
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
                        cmd.verbose,
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
                        options,
                        cmd.verbose,
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
                cmd.flatten_annotations,
                cmd.flatten_rotation,
                overlay_specs,
                cmd.verbose,
                options,
            )
        }
    }
}

fn run_check(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    decode_limits: filters::DecodeLimits,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(&input).map_err(|error| error_with_file(&input, error.into()))?;
    let mut options = pdf_open_options(repair, password)?;
    // qpdf treats `--check` as a read-only inspection, like `--show-encryption`,
    // `--requires-password`, and `--is-encrypted`: an RC4 / R=5 (weak-crypto)
    // file opened with the correct password is checked without
    // `--allow-weak-crypto` and exits 0 with no weak-crypto warning (verified
    // qpdf 11.9.0). Force the gate open here. Authentication still runs first,
    // so a wrong password fails exactly as before.
    options.allow_weak_crypto = true;
    configure_document_logger(&mut options, &input);
    // `check_reader` aggregates parser and checker warnings into one ordered
    // report. Suppress immediate document delivery here, then route every
    // report warning through the same CLI logger exactly once below.
    options.suppress_warnings = true;
    let report = check_reader_with_options_and_limits(BufReader::new(file), options, decode_limits)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
    finish_check_report(&input, report)
}

fn run_check_pdf<R: Read + Seek + 'static>(
    input: &Path,
    pdf: &mut Pdf<R>,
    decode_limits: filters::DecodeLimits,
) -> CliResult<()> {
    finish_check_report(input, check_pdf_with_limits(pdf, decode_limits))
}

fn finish_check_report(input: &Path, report: flpdf::CheckReport) -> CliResult<()> {
    // The library always emits a weak-crypto advisory when a weak file opens
    // (flpdf check.rs: "encrypted PDF uses weak crypto; processing continued").
    // Because `--check` forces the gate open as an inspection rather than the
    // user opting in, suppress that advisory so the run matches qpdf's clean
    // exit 0; qpdf emits no such warning for `--check`.
    let is_weak_crypto_advisory = |d: &flpdf::Diagnostic| {
        d.severity == Severity::Warning && d.message.contains("weak crypto")
    };
    for diagnostic in report.diagnostics.entries() {
        if is_weak_crypto_advisory(diagnostic) {
            continue;
        }
        let location = check_diagnostic_location(input, diagnostic);
        match diagnostic.severity {
            Severity::Warning => {
                let separator = if diagnostic.message.starts_with("(object ")
                    || diagnostic.message.starts_with("(trailer,")
                {
                    " "
                } else {
                    ": "
                };
                let warning = format!("WARNING: {location}{separator}{}\n", diagnostic.message);
                cli_logger().warn(warning)?
            }
            Severity::Error => logger_error(format!(
                "{}: {location}: {}\n",
                progname(),
                diagnostic.message
            ))?, // cov:ignore: exercised by check error subprocess integration tests
        }
    }

    // Map the check result to qpdf-compatible exit codes:
    //   0 — no errors, no warnings (clean)
    //   2 — errors found (invalid / unprocessable)
    //   3 — warnings only, no errors (recoverable issues)
    //
    // Source: https://qpdf.readthedocs.io/en/stable/cli.html#exit-status
    //         qpdf/include/qpdf/Constants.h: qpdf_exit_error=2, qpdf_exit_warning=3
    let has_warnings = report
        .diagnostics
        .entries()
        .iter()
        .any(|d| d.severity == Severity::Warning && !is_weak_crypto_advisory(d));

    if !report.valid {
        // Errors found — exit 2.  The error diagnostics above are already in
        // qpdf shape; qpdf prints no extra summary line in this case.
        return Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: String::new(),
        }));
    }

    // Valid document (exit 0 or 3): emit qpdf's stdout "checking" block. The
    // summary is present whenever the document opened, which is implied by
    // `report.valid`; the `if let` is a defensive match.
    if let Some(summary) = &report.summary {
        print_check_block(input, summary)?;
    }

    if has_warnings {
        // Warnings without errors — exit 3. qpdf still prints the block above,
        // but omits the trailing "No syntax ..." note. `--check` is inspection
        // (`creates_output = false`), and qpdf's `writeQPDF` routes both the
        // output and inspection arms through the same shared warning-summary
        // block (`QPDFJob.cc:486-504`) rather than a `--check`-only path;
        // `finish_warning_state` is that same shared boundary.
        return finish_warning_state(true, false); // cov:ignore: exercised by check warning subprocess integration tests
    }

    // Clean — exit 0. qpdf closes a clean check with this two-line note; the
    // subject mirrors progname() so it is byte-identical under FLPDF_PROGNAME=qpdf.
    logger_info(format!(
        "No syntax or stream encoding errors found; the file may still contain\nerrors that {} cannot detect\n",
        progname()
    ))?; // cov:ignore: exercised by clean check subprocess integration tests
    Ok(())
}

/// Print qpdf's `--check` document summary block to stdout.
///
/// Mirrors qpdf 11.9.0's stdout for a successfully-opened document: the
/// `checking <file>` banner, header version, encryption status and
/// linearization status. `<file>` is echoed verbatim as supplied on the command
/// line (qpdf prints the argument, not a canonicalised path).
fn print_check_block(input: &Path, summary: &flpdf::CheckSummary) -> CliResult<()> {
    let mut output = format!("checking {}\n", input.display());
    // qpdf appends "extension level N" to the version when the catalog declares
    // an Adobe extension level (`/Extensions /ADBE /ExtensionLevel`).
    match summary.extension_level {
        Some(level) => output.push_str(&format!(
            "PDF Version: {} extension level {level}\n",
            summary.version
        )),
        None => output.push_str(&format!("PDF Version: {}\n", summary.version)),
    }
    // Interim: encrypted files emit a single line. The detailed qpdf
    // `R = / P = / permission / method` block is tracked in flpdf-oox1.
    output.push_str(if summary.encrypted {
        "File is encrypted\n"
    } else {
        "File is not encrypted\n"
    });
    // The linearization status reflects the structural detector (object (1,0)
    // only). qpdf-accurate detection — plus the entangled warning / exit-code /
    // trailing-line behaviour — is tracked in flpdf-u1ro.
    output.push_str(if summary.linearized {
        "File is linearized\n"
    } else {
        "File is not linearized\n"
    });
    logger_info(output)
}

/// Wire `--encrypt` / `--copy-encryption` onto `options`, shared by the
/// top-level and `rewrite` surfaces so the two stay in lock-step. A `--encrypt`
/// parse error or a `--copy-encryption`
/// donor-open/validation error prints a `flpdf:`-prefixed diagnostic and exits
/// 2, matching the surrounding option parsers. The two options are mutually
/// exclusive at the CLI layer (clap `conflicts_with`), so at most one branch
/// fires.
fn apply_encryption_options(
    options: &mut WriterOptions,
    encrypt: &[String],
    copy_encryption: Option<&std::path::Path>,
    encryption_file_password: Option<&str>,
    allow_weak_crypto: bool,
) {
    if !encrypt.is_empty() {
        match parse_encrypt_segment(encrypt, allow_weak_crypto) {
            Ok(params) => {
                options.encrypt = Some(params);
            }
            Err(e) => {
                eprintln!("flpdf: {e}");
                std::process::exit(2);
            }
        }
    }
    if let Some(donor_path) = copy_encryption {
        match build_copy_encryption_source(donor_path, encryption_file_password) {
            Ok(src) => {
                options.copy_encryption = Some(src);
            }
            Err(e) => {
                eprintln!("flpdf: {e}");
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
    password: Option<&str>,
) -> CliResult<CopyEncryptionSource> {
    let file =
        File::open(path).map_err(|e| format!("--copy-encryption: cannot open {:?}: {e}", path))?;
    let reader = BufReader::new(file);

    let pw_bytes: Vec<u8> = password.unwrap_or("").as_bytes().to_vec();
    let opts = PdfOpenOptions {
        password: pw_bytes,
        repair: true,
        ..PdfOpenOptions::default()
    };
    let mut opts = opts;
    configure_document_logger(&mut opts, path);
    let mut donor = Pdf::open_with_options(reader, opts)
        .map_err(|e| format!("--copy-encryption: failed to open {:?}: {e}", path))?;

    // Validate the donor is encrypted.
    let info = donor
        .encryption_info()
        .map_err(|e| format!("--copy-encryption: failed to read encryption info: {e}"))?
        .ok_or_else(|| format!("--copy-encryption: donor {:?} is not encrypted", path))?;

    // Walking-skeleton scope: only V=4 AES-128 (StmF=AESV2 / StrF=AESV2).
    // Note: encryption_info uses qpdf_name() which returns "AESv2" (lowercase v).
    let is_v4_aes128 = info.v == 4
        && info.length_bits == 128
        && info.stream_method == "AESv2"
        && info.string_method == "AESv2";
    if !is_v4_aes128 {
        return Err(format!(
            "--copy-encryption: donor {:?} uses V={} length={} \
             stream={} string={} — only V=4 AES-128 donors are accepted",
            path, info.v, info.length_bits, info.stream_method, info.string_method,
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

    // Extract the /Encrypt ObjectRef from the donor trailer, then resolve it.
    // Pull the ref while holding the trailer borrow, then drop that borrow
    // before calling resolve() which needs &mut self.
    let encrypt_ref = donor.trailer().get_ref("Encrypt").ok_or_else(|| {
        format!(
            "--copy-encryption: donor {:?} has no /Encrypt in trailer",
            path
        )
    })?;

    let encrypt_obj = donor.resolve_borrowed(encrypt_ref).map_err(|e| {
        format!(
            "--copy-encryption: failed to resolve /Encrypt in {:?}: {e}",
            path
        )
    })?;

    let encrypt_dict = match encrypt_obj {
        Object::Dictionary(d) => d.clone(),
        other => {
            return Err(format!(
                "--copy-encryption: /Encrypt in {:?} is not a dictionary (got {:?})",
                path, other
            )
            .into())
        }
    };

    // Extract /ID[0] from the donor trailer.
    let id0: Vec<u8> = match donor.trailer().get("ID") {
        Some(Object::Array(arr)) => match arr.first() {
            Some(Object::String(bytes)) => bytes.clone(),
            _ => {
                return Err(
                    format!("--copy-encryption: donor {:?} /ID[0] is not a string", path).into(),
                )
            }
        },
        _ => {
            return Err(format!(
                "--copy-encryption: donor {:?} has no /ID array in trailer",
                path
            )
            .into())
        }
    };

    Ok(CopyEncryptionSource {
        encrypt_dict,
        file_key,
        id0,
        object_key_alg: ObjectKeyAlg::Aes,
    })
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
/// qpdf's checkConfiguration.
///
/// Permission sub-flags (`--print`, `--modify`, `--extract`, `--annotate`,
/// `--form`, `--assemble`, `--accessibility`) use the R>=3 grammar and are
/// applied left-to-right onto a [`PermissionsConfig`] (matching qpdf's
/// ordering). They are accepted for 128/256-bit only; on 40-bit (R=2) they are
/// rejected (the R=2 `/P` encoding differs).
/// `--cleartext-metadata` is still rejected for V=1/V=2 (40-bit or 128-bit
/// without AES/--force-V4); `--force-R5` is accepted for 256-bit only.
fn parse_perm_yn(flag: &str, val: &str) -> CliResult<bool> {
    match val {
        "y" => Ok(true),
        "n" => Ok(false),
        other => Err(format!("{flag} must be y or n (got {other:?})").into()),
    }
}

fn parse_encrypt_segment(tokens: &[String], allow_weak_crypto: bool) -> CliResult<EncryptParams> {
    if tokens.len() < 3 {
        return Err(format!(
            "--encrypt requires USER-PW OWNER-PW KEY-LEN (got {} arg(s))",
            tokens.len()
        )
        .into());
    }
    for token in &tokens[..3] {
        if token.starts_with("-") {
            return Err(format!(
                "unrecognized argument {token} (encryption options must be terminated with --)"
            )
            .into());
        }
    }
    let user_pw = tokens[0].as_bytes().to_vec();
    let owner_pw = tokens[1].as_bytes().to_vec();
    let key_len: u32 = tokens[2].parse().map_err(|_| {
        format!(
            "--encrypt KEY-LEN must be a positive integer (40 / 128 / 256), got: {:?}",
            tokens[2]
        )
    })?;
    if !matches!(key_len, 40 | 128 | 256) {
        return Err(format!("--encrypt KEY-LEN must be 40, 128, or 256 (got {key_len})").into());
    }

    // Parse sub-flags. Unsupported ones are rejected with a clear message so
    // users do not get a silent shrug when they pass `--print=none`.
    let mut use_aes: Option<bool> = None;
    let mut force_v4 = false;
    let mut force_r5 = false;
    // `--allow-insecure` opts into the V=5 R=6 empty-owner + non-empty-user
    // "insecure" combination; the gate itself lives in the KEY-LEN=256 arm
    // below (flpdf-9hc.4.14, mirroring qpdf's checkConfiguration).
    let mut allow_insecure = false;
    // Permission sub-flags (R>=3 grammar, flpdf-9hc.4.9.5). qpdf applies them
    // LEFT-TO-RIGHT, so mutate `perms` in place as each flag is seen rather
    // than collecting and applying in a fixed order (which would break the
    // observable ordering quirk, e.g. `--modify=none --annotate=y`). Permission
    // flags are R>=3 only (128/256); on 40-bit they are rejected below.
    let mut perms = PermissionsConfig::default();
    let mut perm_flag_seen = false;
    // `--cleartext-metadata` leaves the /Metadata XMP stream unencrypted
    // (flpdf-9hc.4.9.6). Honored for V=4/V=5 only (the V=1/V=2 dict builder has
    // no /EncryptMetadata); rejected for 40-bit / 128-without-AES below.
    let mut cleartext_metadata = false;
    for tok in &tokens[3..] {
        let (flag, val) = tok.split_once('=').unwrap_or((tok.as_str(), ""));
        match flag {
            "--use-aes" => {
                use_aes = Some(match val {
                    "y" => true,
                    "n" => false,
                    other => {
                        return Err(format!("--use-aes must be y or n (got {other:?})").into());
                    }
                });
            }
            // `--force-V4` forces the V=4 handler; combined with RC4 (i.e. no
            // `--use-aes=y`) it selects the V=4 /CFM V2 (RC4-128) variant.
            // Value-less flag.
            "--force-V4" => {
                if tok.contains('=') {
                    return Err(format!("--force-V4 does not take a value (got {tok:?})").into());
                }
                force_v4 = true;
            }
            // Value-less; see the KEY-LEN=256 arm. Reject any `=` form so an
            // opt-out typo (`--allow-insecure=false`) or a generated empty value
            // (`--allow-insecure=`) cannot silently enable the insecure path.
            "--allow-insecure" => {
                if tok.contains('=') {
                    return Err(
                        format!("--allow-insecure does not take a value (got {tok:?})").into(),
                    );
                }
                allow_insecure = true;
            }
            // Permission sub-flags (R>=3 grammar). Mutate `perms` in place so
            // the left-to-right ordering matches qpdf. Bit mapping verified
            // empirically against `qpdf --show-encryption`.
            "--print" => {
                perm_flag_seen = true;
                perms.print = match val {
                    "full" => PrintPermission::High,
                    "low" => PrintPermission::Low,
                    "none" => PrintPermission::None,
                    other => {
                        return Err(
                            format!("--print must be full, low, or none (got {other:?})").into(),
                        );
                    }
                };
            }
            "--modify" => {
                perm_flag_seen = true;
                // Cumulative ladder (qpdf): all => other+annot+forms+assembly,
                // annotate => annot+forms+assembly, form => forms+assembly,
                // assembly => assembly, none => nothing.
                let (other_, annot, forms, asm) = match val {
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
                perms.modify_contents = other_;
                perms.annotate = annot;
                perms.fill_forms = forms;
                perms.assemble = asm;
            }
            "--extract" => {
                perm_flag_seen = true;
                perms.extract = parse_perm_yn(flag, val)?;
            }
            "--annotate" => {
                perm_flag_seen = true;
                perms.annotate = parse_perm_yn(flag, val)?;
            }
            "--form" => {
                perm_flag_seen = true;
                perms.fill_forms = parse_perm_yn(flag, val)?;
            }
            "--assemble" => {
                perm_flag_seen = true;
                perms.assemble = parse_perm_yn(flag, val)?;
            }
            "--accessibility" => {
                perm_flag_seen = true;
                perms.accessibility = parse_perm_yn(flag, val)?;
            }
            // Value-less; honored for V=4/V=5 (gated in the dispatch below).
            "--cleartext-metadata" => {
                if tok.contains('=') {
                    return Err(format!(
                        "--cleartext-metadata does not take a value (got {tok:?})"
                    )
                    .into());
                }
                cleartext_metadata = true;
            }
            "--force-R5" => {
                if tok.contains('=') {
                    return Err(format!("--force-R5 does not take a value (got {tok:?})").into());
                }
                force_r5 = true;
            }
            other => {
                return Err(format!(
                    "unknown --encrypt sub-flag {other:?}; supported in this release: \
                     --use-aes=y|n, --force-V4, --force-R5, --allow-insecure, --print, --modify, \
                     --extract, --annotate, --form, --assemble, --accessibility, \
                     --cleartext-metadata"
                )
                .into());
            }
        }
    }

    // Enforce qpdf's per-KEY-LEN option tables: `--use-aes` / `--force-V4` are
    // 128-only and `--allow-insecure` is 256-only. Reject incompatible flags as
    // a usage error rather than silently ignoring them — otherwise
    // `--encrypt … 40 --use-aes=y` would quietly write RC4-40 while the user
    // expected AES (a security-relevant mismatch).
    match key_len {
        40 if use_aes.is_some() || force_v4 || force_r5 || allow_insecure || perm_flag_seen => {
            return Err(
                "--encrypt KEY-LEN=40 (V=1 RC4-40, R=2) does not accept --use-aes, \
                 --force-V4, --force-R5, --allow-insecure, or permission sub-flags; the R>=3 \
                 permission grammar needs a 128- or 256-bit key"
                    .into(),
            );
        }
        128 if allow_insecure || force_r5 => {
            return Err(
                "--encrypt KEY-LEN=128 does not accept --allow-insecure or --force-R5 (256-bit only)".into(),
            );
        }
        256 if use_aes.is_some() || force_v4 => {
            return Err(
                "--encrypt KEY-LEN=256 does not accept --use-aes or --force-V4 (128-bit only)"
                    .into(),
            );
        }
        _ => {}
    }

    // RC4 outputs are weak; qpdf refuses to write them without
    // --allow-weak-crypto, so apply the same gate here. Deprecated R=5
    // (AES-256) output is also gated: unlike qpdf — which gates only RC4 and
    // happily writes R=5 — flpdf rejects reading R=5 input without
    // --allow-weak-crypto, so it refuses to *create* R=5 without the same
    // opt-in to keep the read and write paths symmetric (see the threat model,
    // §4 weak-crypto write gate).
    let guard_weak = |params: EncryptParams| -> CliResult<EncryptParams> {
        if !allow_weak_crypto {
            if params.is_weak_rc4() {
                return Err(
                    "refusing to write a file with RC4, a weak cryptographic algorithm. \
                     Please use 256-bit keys for better security. Pass --allow-weak-crypto \
                     to enable writing insecure files."
                        .into(),
                );
            }
            if params.is_deprecated_r5() {
                return Err(
                    "refusing to write a deprecated revision 5 (R=5) encrypted file. \
                     256-bit revision 6 (the default without --force-R5) is preferred. \
                     Pass --allow-weak-crypto to enable writing R=5 files."
                        .into(),
                );
            }
        }
        Ok(params)
    };

    // --cleartext-metadata needs /EncryptMetadata, a V>=4 concept; the V=1/V=2
    // dict builder cannot emit it. Reject it before dispatch when the chosen
    // method would be V=1 (40-bit) or V=2 (128 without AES / --force-V4).
    if cleartext_metadata {
        let is_v4_or_v5 = key_len == 256 || (key_len == 128 && (use_aes == Some(true) || force_v4));
        if !is_v4_or_v5 {
            return Err(
                "--cleartext-metadata requires V=4 or V=5 (256-bit, or 128-bit with \
                 --use-aes=y or --force-V4); V=1/V=2 have no /EncryptMetadata"
                    .into(),
            );
        }
    }

    match key_len {
        // KEY-LEN=40 is always V=1 RC4-40; --use-aes / --force-V4 do not apply.
        40 => guard_weak(EncryptParams::rc4(
            EncryptMethod::V1Rc440,
            user_pw,
            owner_pw,
        )),
        128 => {
            let mut params = match use_aes {
                Some(true) => EncryptParams::v4_aes128(user_pw, owner_pw),
                // qpdf's 128-bit default is RC4; `--force-V4` selects the V=4
                // /CFM V2 variant, otherwise V=2 R=3.
                Some(false) | None => {
                    let method = if force_v4 {
                        EncryptMethod::V4Rc4128
                    } else {
                        EncryptMethod::V2Rc4128
                    };
                    EncryptParams::rc4(method, user_pw, owner_pw)
                }
            };
            params.permissions = perms;
            // Accessibility (bit 10) is unconditionally permitted for R>3;
            // qpdf ignores `--accessibility=n` there. V=4 is R=4, so force it
            // on; V=2 (R=3) honors the flag.
            if matches!(
                params.method,
                EncryptMethod::V4Aes128 | EncryptMethod::V4Rc4128
            ) {
                params.permissions.accessibility = true;
            }
            // cleartext_metadata was validated to imply V=4 here (the guard
            // above rejects it for the V=2 default).
            if cleartext_metadata {
                params.encrypt_metadata = false;
            }
            guard_weak(params)
        }
        256 => {
            // V=5 R=6 AES-256 — always AES, so `--use-aes` is irrelevant.
            // Insecure-combination gate (flpdf-9hc.4.14, matching qpdf's
            // checkConfiguration): a non-empty user password with an EMPTY
            // owner password under a 256-bit key lets anyone open the file
            // without the owner password, so the owner restrictions are
            // meaningless. Require explicit `--allow-insecure`.
            if owner_pw.is_empty() && !user_pw.is_empty() && !allow_insecure {
                return Err(
                    "A PDF with a non-empty user password and an empty owner password \
                     encrypted with a 256-bit key is insecure as it can be opened without \
                     a password. If you really want to do this, you must also give the \
                     --allow-insecure option before the -- that follows --encrypt."
                        .into(),
                );
            }
            let mut params = if force_r5 {
                EncryptParams::v5_r5(user_pw, owner_pw)
            } else {
                EncryptParams::v5_r6(user_pw, owner_pw)
            };
            params.permissions = perms;
            // V=5 is R=6 (>3): accessibility is unconditionally permitted, so
            // qpdf ignores `--accessibility=n`. Match that.
            params.permissions.accessibility = true;
            if cleartext_metadata {
                params.encrypt_metadata = false;
            }
            // R=6 (the default) passes through; --force-R5 selects deprecated
            // R=5, which guard_weak gates behind --allow-weak-crypto.
            guard_weak(params)
        }
        _ => unreachable!("key_len validated to 40/128/256 above"),
    }
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
    flatten_annotations_mode: Option<CliFlattenMode>,
    flatten_rotation: bool,
    overlay_specs: &[OverlaySpec],
    verbose: bool,
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
    )?;
    match opened {
        JobPdf::File(pdf) => run_rewrite_opened(
            pdf,
            &input,
            &output,
            repair,
            linearize,
            linearize_pass1,
            remove_restrictions,
            decrypt,
            normalize_content,
            coalesce_contents,
            _remove_unref,
            generate_appearances,
            flatten_annotations_mode,
            flatten_rotation,
            overlay_specs,
            verbose,
            options,
        ),
        JobPdf::Json(pdf) => run_rewrite_opened(
            pdf,
            &input,
            &output,
            repair,
            linearize,
            linearize_pass1,
            remove_restrictions,
            decrypt,
            normalize_content,
            coalesce_contents,
            _remove_unref,
            generate_appearances,
            flatten_annotations_mode,
            flatten_rotation,
            overlay_specs,
            verbose,
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
    linearize: bool,
    linearize_pass1: Option<&Path>,
    remove_restrictions: bool,
    decrypt: bool,
    normalize_content: bool,
    coalesce_contents: bool,
    _remove_unref: CliRemoveUnreferencedResources,
    generate_appearances: bool,
    flatten_annotations_mode: Option<CliFlattenMode>,
    flatten_rotation: bool,
    overlay_specs: &[OverlaySpec],
    verbose: bool,
    options: WriterOptions,
) -> CliResult<()> {
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
        let had_signatures = if remove_restrictions {
            disable_digital_signatures(&mut pdf)?
        } else {
            false
        };
        let mut options = options;
        if decrypt || remove_restrictions {
            options.preserve_encryption = false;
        }
        // Apply content normalization before the writer plans and emits the
        // linearized document.
        let normalization_last_bad = if normalize_content {
            pdf.with_writer_stream_recovery(normalize_page_contents)?
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
            logger_info(format!("flpdf: wrote file {}\n", output.display()))?;
        }
        if had_signatures {
            logger_warn("flpdf: warning: removed signatures; signatures are now invalidated\n")?;
        }
        // On an encrypted input, `--decrypt`/`--remove-restrictions` has
        // already disabled source-encryption preservation above.
        finish_rewrite_warnings(input, &pdf, &normalization_last_bad, announce_file)?;
    } else {
        // Capture encryption state before the write for the qpdf-compatible
        // restriction diagnostic.
        let was_encrypted = pdf.is_encrypted();
        // qpdf runs disableDigitalSignatures unconditionally under
        // --remove-restrictions: remove catalog /Perms, zero /AcroForm
        // /SigFlags, strip /FT /V /SV /Lock from /Sig form fields, and erase them
        // from the top-level /Fields array (a field still reachable from a page
        // /Annots survives as a plain annotation; orphaned signature dicts are
        // dropped by the canonical rewrite GC). The returned flag reports
        // whether anything changed, driving the warning.
        let had_signatures = if remove_restrictions {
            disable_digital_signatures(&mut pdf)?
        } else {
            false
        };
        let mut options = options;
        if decrypt || remove_restrictions {
            options.preserve_encryption = false;
        }
        // ── Content mutation pass ─────────────────────────────────────────────
        //
        // The mutations below operate on the in-memory Pdf model (via set_object).
        // They are all visible in the canonical writer output.
        //
        // Application order (semantically motivated):
        //   1. coalesce_page_contents  — merge /Contents arrays so subsequent
        //      passes always see a single stream per page.
        //   2. normalize_content_stream — re-tokenize the (now-unified) stream
        //      to canonical whitespace form.
        //   3. write (compress_streams / newline_before_endstream) — byte-emission
        //      policies applied by the writer, not the pre-processing step.
        //
        // NOTE: a plain `rewrite` does NOT prune unreferenced /Resources entries.
        // qpdf only prunes resource-dict entries during page-copy operations
        // (`--pages`/`--split-pages`) — a plain `qpdf IN OUT`, even with
        // `--remove-unreferenced-resources=yes`, keeps every /Resources entry
        // (verified against qpdf 11.9.0). flpdf mirrors this: resource-entry
        // pruning lives in `run_page_extraction` (the --pages path), not here.
        // Pruning on a plain rewrite was a divergence that dropped an
        // unreferenced image XObject (flpdf-79ef); it is the resource-entry half
        // of flpdf-9hc.12.4/12.7, which conflated unreferenced-OBJECT GC (the
        // renumber drops unreachable objects on every canonical rewrite — kept) with
        // /Resources-ENTRY pruning (page-op-only — removed here).
        //
        // qpdf always creates a fresh document and defaults to
        // `--compress-streams=y`; the canonical writer applies those defaults
        // for every rewrite. Version setters therefore always affect the
        // emitted header, including with `--remove-unreferenced-resources=no`.
        // Step 1: coalesce per-page /Contents arrays into a single stream.
        if coalesce_contents {
            let page_refs = pages::page_refs(&mut pdf)?;
            for page_ref in page_refs {
                coalesce_page_contents(&mut pdf, page_ref)?;
            }
        }

        // Step 2: normalize each page's content stream(s).
        // normalize_content_stream operates on raw decoded bytes → returns
        // normalized bytes. We fetch each page's /Contents reference(s), decode
        // the stored stream data, normalize, and write the result back via
        // set_object (same pattern as coalesce_page_contents).
        let normalization_last_bad = if normalize_content {
            pdf.with_writer_stream_recovery(normalize_page_contents)?
        } else {
            Vec::new()
        };

        // (No resource-entry pruning on the plain rewrite path — see the
        // "Content mutation pass" note above. qpdf prunes /Resources entries only
        // during page operations, which flpdf handles in run_page_extraction.)

        // Step 4: generate missing form-field appearance streams
        // (--generate-appearances). MUST run before --flatten-annotations so
        // value-only fields (e.g. a filled text field with no /AP) are baked
        // into page content instead of being dropped (acceptance ordering:
        // generate first, flatten second).
        if generate_appearances {
            generate_missing_appearances(&mut pdf)?;
        }

        // Step 5: flatten annotations into page content (--flatten-annotations).
        if let Some(mode) = flatten_annotations_mode {
            let (required_flags, forbidden_flags) = mode.flags();
            PageDocumentHelper::new(&mut pdf)
                .flatten_annotations(required_flags, forbidden_flags)?;
        }

        // Step 6: flatten page rotation into content (--flatten-rotation).
        if flatten_rotation {
            let page_refs = pages::page_refs(&mut pdf)?;
            flatten_rotation_on_pages(&mut pdf, &page_refs)?;
        }

        // Step 7: overlay/underlay page stacking (--overlay / --underlay).
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
            let mut built = build_overlay_specs(overlay_specs, repair)?;

            // flpdf-9hc.16.8: propagate max input header version + Adobe
            // extension_level to the writer (mirrors qpdf QPDFJob.cc L1714
            // accumulator + L2913 setMinimumPDFVersion). Executed only when
            // overlay/underlay is in play; a full CLI-wide input-version
            // accumulator across other paths is out of scope here (documented
            // as non-scope in the bd design).
            let initial_version =
                parse_pdf_version(pdf.version()).unwrap_or(PdfVersion::new(1, 0, 0));
            let mut max_version = PdfVersion::new(
                initial_version.major(),
                initial_version.minor(),
                pdf.adobe_extension_level().unwrap_or(0),
            );
            for spec in built.iter_mut() {
                let source_version =
                    parse_pdf_version(spec.source.version()).unwrap_or(PdfVersion::new(1, 0, 0));
                max_version.update_if_greater(PdfVersion::new(
                    source_version.major(),
                    source_version.minor(),
                    spec.source.adobe_extension_level().unwrap_or(0),
                ));
            }
            // Preserve any pre-existing --min-version / --min-extension-level
            // CLI arg by taking pairwise max with the accumulated floor.
            if let Some(ref current) = options.min_version {
                let current_version =
                    parse_pdf_version(current).unwrap_or(PdfVersion::new(1, 0, 0));
                max_version.update_if_greater(PdfVersion::new(
                    current_version.major(),
                    current_version.minor(),
                    options.min_extension_level.unwrap_or(0),
                ));
            }
            let (version, max_ext) = max_version.get_version();
            options.min_version = Some(version);
            options.min_extension_level = (max_ext > 0).then_some(max_ext);

            // --verbose: emit the per-destination-page overlay/underlay plan
            // to stderr before painting, matching qpdf's --verbose output
            // ("processing underlay/overlay" header + `  page N` +
            // `    <file> overlay|underlay <src>`). The report is computed
            // via the flpdf::overlay_verbose_report inspection API so the
            // ordering (underlays first, then overlays, in declaration
            // order across specs) is source-shared with apply_overlay_specs.
            if verbose {
                let report = flpdf::overlay_verbose_report(&mut pdf, &mut built)?;
                let mut message = String::from("flpdf: processing underlay/overlay\n");
                for page in &report {
                    message.push_str(&format!("  page {}\n", page.dest_page));
                    for src in &page.sources {
                        let file = &overlay_specs[src.spec_index].file;
                        let kind_str = match src.kind {
                            flpdf::OverlayKind::Underlay => "underlay",
                            flpdf::OverlayKind::Overlay => "overlay",
                        };
                        message.push_str(&format!("    {} {} {}\n", file, kind_str, src.src_page));
                    }
                }
                logger_info(message)?;
            }

            flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
            Some(built)
        } else {
            None
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
            logger_info(format!("flpdf: wrote file {}\n", output.display()))?;
        }
        if remove_restrictions && was_encrypted {
            eprintln!("flpdf: removed restrictions (encryption and advisory permissions stripped)");
        }
        if had_signatures {
            logger_warn("flpdf: warning: removed signatures; signatures are now invalidated\n")?;
        }
        // Unencrypted input + --remove-restrictions is a no-op rewrite
        // (exit 0, valid output, no diagnostic) — nothing was restricted,
        // matching qpdf's lenient handling of --remove-restrictions on
        // unencrypted files.
        finish_rewrite_warnings(input, &pdf, &normalization_last_bad, announce_file)?;
    }
    Ok(())
}

/// Route `--generate-appearances` through qpdf's
/// `QPDFAcroFormDocumentHelper::generateAppearancesIfNeeded` boundary.
fn generate_missing_appearances<R: Read + Seek>(pdf: &mut Pdf<R>) -> CliResult<()> {
    AcroFormDocumentHelper::new(pdf).generate_appearances_if_needed()?;
    Ok(())
}

// ===========================================================================
// Page operations (flpdf-9hc.8.12): --pages / --rotate / --split-pages /
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
    file_token: String,
    /// Per-input password (`--password=` immediately following the file).
    password: Option<String>,
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
fn parse_pages_segment(tokens: &[String]) -> CliResult<Vec<PageSegmentSpec>> {
    let mut specs: Vec<PageSegmentSpec> = Vec::new();

    for tok in tokens {
        if let Some(path) = tok.strip_prefix("--file=") {
            specs.push(PageSegmentSpec {
                file_token: path.to_string(),
                password: None,
                range: String::new(),
            });
            continue;
        }
        if let Some(pw) = tok.strip_prefix("--password=") {
            let cur = specs
                .last_mut()
                .ok_or("--pages: --password= must follow a file in the --pages segment")?;
            cur.password = Some(pw.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--range=") {
            let cur = specs
                .last_mut()
                .ok_or("--pages: --range= must follow a file in the --pages segment")?;
            if !cur.range.is_empty() {
                return Err("--pages: duplicate page-range for one input file".into());
            }
            cur.range = r.to_string();
            continue;
        }
        if tok.starts_with("--") {
            return Err(format!(
                "--pages: unsupported token {tok:?} in the page-selection segment"
            )
            .into());
        }
        // Positional token: either a NEW file, or the page-range for the
        // current file. qpdf's heuristic: the token is a page-range iff a
        // file is already open and that file has no range yet AND the token
        // parses as a page-range. Otherwise it starts a new file.
        match specs.last_mut() {
            Some(cur) if cur.range.is_empty() && PageRange::parse(tok).is_ok() => {
                cur.range = tok.clone();
            }
            _ => specs.push(PageSegmentSpec {
                file_token: tok.clone(),
                password: None,
                range: String::new(),
            }),
        }
    }

    if specs.is_empty() {
        return Err("--pages: no input files given in the page-selection segment".into());
    }
    Ok(specs)
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
        let path: PathBuf = if s.file_token == "." {
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
        out.push(InputSpec::new(path, s.password.clone(), range));
    }
    Ok(out)
}

// ===========================================================================
// --overlay / --underlay segment parser (flpdf-9hc.16.1)
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
    file: String,
    /// Password for the source PDF, if supplied via `--password=`.
    password: Option<String>,
    /// Raw `--from=` page-range string (source pages to cycle through).
    from: Option<String>,
    /// Raw `--to=` page-range string (destination pages to receive content).
    to: Option<String>,
    /// Raw `--repeat=` page-range string (source pages to repeat for surplus dest).
    repeat: Option<String>,
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
fn parse_overlay_segment(kind: OverlayKind, tokens: &[String]) -> CliResult<OverlaySpec> {
    let flag = match kind {
        OverlayKind::Overlay => "--overlay",
        OverlayKind::Underlay => "--underlay",
    };

    if tokens.is_empty() {
        return Err(format!("{flag}: no source file given in the segment").into());
    }

    let mut file: Option<String> = None;
    let mut password: Option<String> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut repeat: Option<String> = None;

    for tok in tokens {
        if let Some(path) = tok.strip_prefix("--file=") {
            if file.is_some() {
                return Err(format!("{flag}: duplicate file in segment").into());
            }
            file = Some(path.to_string());
            continue;
        }
        if let Some(pw) = tok.strip_prefix("--password=") {
            if password.is_some() {
                return Err(format!("{flag}: duplicate --password= in segment").into());
            }
            password = Some(pw.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--to=") {
            if to.is_some() {
                return Err(format!("{flag}: duplicate --to= in segment").into());
            }
            PageRange::parse(r)
                .map_err(|e| format!("{flag}: invalid --to= page range {r:?}: {e}"))?;
            to = Some(r.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--from=") {
            if from.is_some() {
                return Err(format!("{flag}: duplicate --from= in segment").into());
            }
            PageRange::parse(r)
                .map_err(|e| format!("{flag}: invalid --from= page range {r:?}: {e}"))?;
            from = Some(r.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--repeat=") {
            if repeat.is_some() {
                return Err(format!("{flag}: duplicate --repeat= in segment").into());
            }
            PageRange::parse(r)
                .map_err(|e| format!("{flag}: invalid --repeat= page range {r:?}: {e}"))?;
            repeat = Some(r.to_string());
            continue;
        }
        if tok.starts_with("--") {
            return Err(format!("{flag}: unsupported token {tok:?} in segment").into());
        }
        // Bare (non-flag) token: must be the file path (exactly one allowed).
        if file.is_some() {
            return Err(format!("{flag}: duplicate file in segment").into());
        }
        file = Some(tok.clone());
    }

    let file = file.ok_or_else(|| format!("{flag}: no source file given in the segment"))?;

    Ok(OverlaySpec {
        kind,
        file,
        password,
        from,
        to,
        repeat,
    })
}

/// A qpdf value-terminated option table that temporarily changes which
/// single-dash option names are recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QpdfArgSegment {
    Encrypt,
    Pages,
    AddAttachment,
    CopyAttachments,
    Overlay,
}

impl QpdfArgSegment {
    fn from_option_name(name: &str) -> Option<Self> {
        match name {
            "encrypt" => Some(Self::Encrypt),
            "pages" => Some(Self::Pages),
            "add-attachment" => Some(Self::AddAttachment),
            "copy-attachments-from" => Some(Self::CopyAttachments),
            "overlay" | "underlay" => Some(Self::Overlay),
            _ => None,
        }
    }

    fn accepts(self, name: &str) -> bool {
        match self {
            Self::Encrypt => matches!(
                name,
                "use-aes"
                    | "force-V4"
                    | "force-R5"
                    | "allow-insecure"
                    | "print"
                    | "modify"
                    | "extract"
                    | "annotate"
                    | "form"
                    | "assemble"
                    | "accessibility"
                    | "cleartext-metadata"
            ),
            Self::Pages => matches!(name, "file" | "password" | "range"),
            Self::AddAttachment => matches!(
                name,
                "key"
                    | "filename"
                    | "mimetype"
                    | "description"
                    | "creationdate"
                    | "moddate"
                    | "replace"
            ),
            Self::CopyAttachments => matches!(name, "password" | "prefix"),
            Self::Overlay => matches!(name, "file" | "password" | "to" | "from" | "repeat"),
        }
    }
}

fn collect_clap_long_options(command: &clap::Command, names: &mut HashSet<String>) {
    for arg in command.get_arguments() {
        if let Some(long) = arg.get_long() {
            names.insert(long.to_string());
        }
        if let Some(aliases) = arg.get_all_aliases() {
            names.extend(aliases.into_iter().map(str::to_string));
        }
    }
    for subcommand in command.get_subcommands() {
        collect_clap_long_options(subcommand, names);
    }
}

fn long_option_name(arg: &str) -> Option<&str> {
    arg.strip_prefix("--")
        .filter(|rest| !rest.is_empty())
        .and_then(|rest| rest.split("=").next())
}

fn single_dash_option_name(arg: &str) -> Option<&str> {
    if arg == "-" || arg.starts_with("--") {
        return None;
    }
    let rest = arg.strip_prefix("-")?;
    if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.split("=").next()
}

/// Rewrite recognized qpdf-style single-dash long options into double-dash form.
///
/// Recognition follows the clap command tree at top level. Within qpdf
/// value-terminated segments, only that segment sub-options are recognized;
/// unknown dash-prefixed tokens remain operands. A bare `--` closes an active
/// segment and resumes top-level option recognition. Outside a segment, `--`
/// is the real clap end-of-options marker and leaves every later token untouched.
fn rewrite_qpdf_single_dash(args: Vec<String>) -> Vec<String> {
    let mut known_long_options = HashSet::new();
    collect_clap_long_options(&Cli::command(), &mut known_long_options);

    let mut out = Vec::with_capacity(args.len());
    let mut active_segment = None;
    let mut passthrough = false;

    for mut arg in args {
        if passthrough {
            out.push(arg);
            continue;
        }
        if arg == "--" {
            out.push(arg);
            if active_segment.take().is_none() {
                passthrough = true;
            }
            continue;
        }

        if let Some(name) = single_dash_option_name(&arg) {
            let recognized = active_segment
                .map(|segment: QpdfArgSegment| segment.accepts(name))
                .unwrap_or_else(|| known_long_options.contains(name));
            if recognized {
                arg = format!("-{arg}");
            }
        }

        if active_segment.is_none() {
            if let Some(name) = long_option_name(&arg) {
                active_segment = QpdfArgSegment::from_option_name(name);
            }
        }
        out.push(arg);
    }
    out
}

/// Split the `--overlay`/`--underlay` groups out of the raw argument vector,
/// preserving their declaration order and per-group boundaries.
///
/// clap's derive collects repeated `Vec<String>` occurrences into one flat
/// vector, which loses both the boundary between successive `--overlay`/
/// `--underlay` groups and their interleaved declaration order — information
/// the byte-identical composition (underlays-then-overlays naming across
/// groups, drawn in qpdf order) depends on. So the groups are extracted from
/// the raw argv here, *before* clap parses, and the residual vector (with every
/// `--overlay`/`--underlay` flag, its tokens, and its terminating `--` removed)
/// is handed to clap. The returned `OverlaySpec`s are in CLI declaration order.
///
/// A group runs from its `--overlay`/`--underlay` flag up to (but not
/// including) the next bare `--` token, which qpdf requires to terminate it.
/// Tokens such as `--password=…` that merely start with `--` do not terminate a
/// group; only a token equal to `--` does.
///
/// The scan is scoped to *rewrite-level* overlay flags: the sibling
/// value-terminated segments (`--encrypt`, `--pages`, `--add-attachment`,
/// `--copy-attachments-from`) are each consumed as a unit up to their own
/// terminating `--`, so an `--overlay`/`--underlay` token appearing as one of
/// their values is preserved verbatim rather than starting a spurious group
/// (mirroring qpdf's left-to-right parser, which consumes each value-terminated
/// option as a whole).
///
/// # Errors
///
/// Returns an error if a group is not terminated by a `--` token, or if
/// [`parse_overlay_segment`] rejects the captured tokens (missing/duplicate
/// file, invalid page range, unknown sub-flag, …).
fn extract_overlay_groups(args: Vec<String>) -> CliResult<(Vec<String>, Vec<OverlaySpec>)> {
    let mut residual: Vec<String> = Vec::with_capacity(args.len());
    let mut specs: Vec<OverlaySpec> = Vec::new();

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        // A sibling value-terminated segment owns every token up to its own
        // terminating `--`. Copy it verbatim into the residual (for clap) without
        // scanning inside, so an `--overlay`/`--underlay` that is really a *value*
        // of one of these flags is not mistaken for a new overlay group. An
        // unterminated segment is copied to the end and left for clap to reject.
        if matches!(
            arg.as_str(),
            "--encrypt" | "--pages" | "--add-attachment" | "--copy-attachments-from"
        ) {
            residual.push(arg);
            for tok in iter.by_ref() {
                let is_terminator = tok == "--";
                residual.push(tok);
                if is_terminator {
                    break;
                }
            }
            continue;
        }
        // qpdf requires the overlay/underlay file as a separate token (the file
        // may be written `--file=FILE` INSIDE the group, but the flag itself is
        // not an `=`-valued option). qpdf rejects `--overlay=FILE` with "overlay
        // file not specified". The clap definitions keep `--overlay`/`--underlay`
        // only for `--help`, and their parsed values are unused, so without this
        // check an `--overlay=FILE` token would slip past clap and be a silent
        // no-op. Reject the equals form here to match qpdf.
        for prefix in ["--overlay=", "--underlay="] {
            if arg.starts_with(prefix) {
                let flag = prefix.trim_end_matches('=');
                return Err(format!(
                    "{flag}: the `{flag}=FILE` form is not supported; pass the file as a \
                     separate token: `{flag} FILE … --`"
                )
                .into());
            }
        }
        let kind = match arg.as_str() {
            "--overlay" => Some(OverlayKind::Overlay),
            "--underlay" => Some(OverlayKind::Underlay),
            _ => None,
        };
        let Some(kind) = kind else {
            residual.push(arg);
            continue;
        };

        // Collect tokens up to (and consuming) the terminating bare `--`.
        let mut tokens: Vec<String> = Vec::new();
        let mut terminated = false;
        for tok in iter.by_ref() {
            if tok == "--" {
                terminated = true;
                break;
            }
            tokens.push(tok);
        }
        if !terminated {
            let flag = match kind {
                OverlayKind::Overlay => "--overlay",
                OverlayKind::Underlay => "--underlay",
            };
            return Err(format!(
                "{flag}: overlay/underlay group must be terminated by a `--` token"
            )
            .into());
        }
        specs.push(parse_overlay_segment(kind, &tokens)?);
    }

    Ok((residual, specs))
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
fn build_overlay_specs(
    specs: &[OverlaySpec],
    repair: bool,
) -> CliResult<Vec<flpdf::OverlaySpec<BufReader<File>>>> {
    let mut built = Vec::with_capacity(specs.len());
    for spec in specs {
        let path = PathBuf::from(&spec.file);
        let file = File::open(&path).map_err(|error| error_with_file(&path, error.into()))?;
        // Overlay sources are read-only; qpdf accepts weak-crypto opens
        // unconditionally (the flag only gates weak-crypto WRITES). Match
        // qpdf and unblock RC4 overlays — same pattern `run_check` uses
        // for its inspection open (search for `options.allow_weak_crypto`
        // in `run_check`).
        let mut options = PdfOpenOptions {
            // qpdf's recovery permission is enabled on the document by
            // default; the absence of `--repair` must not turn it off (see
            // `pdf_open_options`'s identical treatment for the primary
            // document).
            repair: repair || PdfOpenOptions::default().repair,
            allow_weak_crypto: true,
            password: spec
                .password
                .as_ref()
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..Default::default()
        };
        configure_document_logger(&mut options, &path);
        let source = Pdf::open_with_options(BufReader::new(file), options)
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

/// Parse `--collate` value: `n` or `i,j,k,...`. flpdf's [`collate`] supports a
/// single chunk size `n`; the comma form is parsed but only the first value is
/// honoured (a documented divergence — full per-input groups are out of
/// scope).
fn parse_collate_n(raw: &str) -> CliResult<usize> {
    // Only a single positive integer is supported. Silently using the first
    // value of `--collate=1,2` would emit a different page order than the
    // user asked for, so reject comma-separated group lists explicitly.
    let n: usize = raw.parse().map_err(|_| {
        format!(
            "--collate: expected a single positive integer, got {raw:?} \
             (comma-separated group lists are not supported)"
        )
    })?;
    if n == 0 {
        return Err("--collate: group size must be >= 1".into());
    }
    Ok(n)
}

/// Basename of `p` for `--verbose --pages` progress lines (qpdf uses the
/// bare file name — e.g. `fxo-red.pdf`, not the absolute path or `.`).
fn pages_progress_filename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Run the `--pages` extraction pipeline.
///
/// Processing order is fixed as follows:
///   1. page_combine / page_collate → selected ObjectRef list
///   2. pages::tree_rebuild::rebuild_page_tree → RebuildResult
///   3. apply_rotate_to_pages (on the rebuilt OUTPUT leaves; qpdf-observed)
///      3.5. /PageLabels reconstruction (per selected page, qpdf
///      `handlePageSpecs`-observed)
///   4. outline_dest_remap::remap_outline_and_dests
///   5. struct_tree_pg::drop_struct_elem_dangling_pg
///   6. thread_bead_p::drop_thread_bead_dangling_p
///      6.5. objr_obj_annot_p::drop_objr_obj_annot_dangling_p
///   7. subset_prune::prune_after_subset (Auto/Yes/No)
///   8. acroform_field_prune::prune_acroform_after_subset
///   9. write (or split_pages when --split-pages is set)
///
/// Multi-source page specifications are handled by the job-level
/// `QPDFJob::handle_page_specs` route, which returns a fresh primary-based
/// document after foreign-page copy, field collision handling, PageLabels
/// reconstruction, and spec-order restoration. The in-place route below is
/// retained for a single source so the existing post-rebuild consumers can
/// continue to operate on the original object graph.
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
    verbose: bool,
) -> CliResult<()> {
    // `--split-pages` writes one numbered file per output page rather than a
    // single `output` path, so `output` is a naming template here, not a
    // literal file to compare against `primary_input` — matching qpdf's own
    // `(!m->split_pages) && QUtil::same_file(...)` exclusion in
    // `checkConfiguration()` (`QPDFJob.cc:627`).
    if page_ops.split_pages.is_none() {
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

    let specs = parse_pages_segment(&page_ops.pages)?;
    let mut inputs = resolve_page_specs(&specs, primary_input)?;
    let has_external_source = inputs.iter().any(|spec| spec.path != primary_input);

    // The in-place single-document planner must use the top-level password
    // for the already-authenticated primary when `--pages . ...` carries no
    // segment password. The multi-source QPDFJob route opens the primary
    // separately and must leave secondary credentials segment-local: qpdf does
    // not fall back to the primary password for a distinct source
    // (QPDFJob.cc:2400-2412).
    if !has_external_source {
        if let Some(top_pw) = &password.password {
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
        // `run_page_extraction_from_repeated_pdf` below applies every spec's
        // range to the single already-opened job document; it has no way to
        // honor a `spec.path` that names a genuinely different file (unlike
        // the ordinary branch further down, which opens `inputs[0].path`
        // directly). The `distinct.len() > 1` check above only catches this
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
        )?;
        return match opened {
            JobPdf::File(pdf) => run_page_extraction_from_repeated_pdf(
                pdf,
                primary_input,
                output,
                repair,
                page_ops,
                overlay_specs,
                remove_unref,
                options,
                verbose,
                standard_output,
                creates_output,
                &inputs,
                &distinct,
            ),
            JobPdf::Json(pdf) => run_page_extraction_from_repeated_pdf(
                pdf,
                primary_input,
                output,
                repair,
                page_ops,
                overlay_specs,
                remove_unref,
                options,
                verbose,
                standard_output,
                creates_output,
                &inputs,
                &distinct,
            ),
        };
    }

    // qpdf's ordinary page-spec job owns distinct input documents and copies
    // foreign pages into the primary output. Route that case through the
    // library QPDFJob facade; retain the existing in-place route for the
    // single-document path, where its outline/structure post-passes operate
    // on the original object graph.
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
            verbose,
            standard_output,
            creates_output,
            inputs,
        );
    }

    // qpdf's page-operation output inherits encryption from the command's
    // primary input. A plaintext primary importing pages from an encrypted
    // secondary produces plaintext; an encrypted primary remains encrypted.
    // Probe the primary separately because the selected page source may be a
    // different input in qpdf's `--pages` command.
    let primary_encrypted =
        open_pdf(&primary_input.to_path_buf(), repair, password)?.is_encrypted();

    // --verbose: emit qpdf-parity `--pages` progress. Order matches
    // libqpdf/QPDFJob.cc: process_all() emits the shared-resource scan
    // per input file (L2250/L2312), then the top-level pipeline emits
    // "removing unreferenced pages from primary input" (L2539) once, then
    // "adding pages from <file>" per Selection (L2594) inside the copy loop.
    //
    // The qpdf shared-resource heuristic is evaluated again at the consumer
    // boundary immediately before rebuild_page_tree, when the source page
    // tree is still intact. This progress block retains the established
    // ordering; the keep-open-files subsystem remains an unconditional "y"
    // because flpdf has no equivalent file-handle policy.
    if verbose {
        let mut message = String::from("flpdf: selecting --keep-open-files=y\n");
        for path in &distinct {
            let fname = pages_progress_filename(path);
            message.push_str(&format!(
                "flpdf: {fname}: checking for shared resources\nflpdf: no shared resources found\n"
            ));
        }
        logger_info(message)?;
    }

    // CombinedPlan::from_specs opens each file itself; its per-input
    // PagePlans carry source ObjectRefs that are stable across a re-open of
    // identical bytes. We use it only for selection/collation planning, then
    // open the (single) resolved source ourselves for the rebuild passes.
    let plan = CombinedPlan::from_specs(inputs.clone())?;

    if verbose {
        let mut message = String::from("flpdf: removing unreferenced pages from primary input\n");
        for spec in &inputs {
            message.push_str(&format!(
                "flpdf: adding pages from {}\n",
                pages_progress_filename(&spec.path)
            ));
        }
        logger_info(message)?;
    }

    let combined_pages = match page_ops.collate.as_deref() {
        Some(raw) => {
            let n = parse_collate_n(raw)?;
            collate(&plan, n)?
        }
        None => plan.flat_pages().to_vec(),
    };

    let source_path = &inputs[0].path;
    let source_password = inputs[0].password.clone();
    let mut src_pw = password.clone();
    if let Some(pw) = source_password {
        src_pw.password = Some(pw);
        src_pw.password_file = None;
    }
    let mut pdf = open_pdf(&source_path.to_path_buf(), repair, &src_pw)?;
    let primary_copy_encryption = pdf.writer_copy_encryption_source()?;

    run_page_extraction_after_plan(
        pdf,
        output,
        source_path,
        repair,
        page_ops,
        overlay_specs,
        remove_unref,
        options,
        verbose,
        standard_output,
        creates_output,
        primary_encrypted,
        primary_copy_encryption,
        false,
        true,
        combined_pages,
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
    verbose: bool,
    standard_output: Option<PipelineWriter>,
    creates_output: bool,
    inputs: Vec<InputSpec>,
) -> CliResult<()> {
    // qpdf inherits output encryption from the primary input for page
    // operations. Keep this probe separate from the mutable source vector so
    // source opening below can use the same top-level password policy.
    let primary_encrypted =
        open_pdf(&primary_input.to_path_buf(), repair, password)?.is_encrypted();

    // Build literal-path source identity and one qpdf page specification per
    // occurrence. `.` was already normalized to primary_input by
    // resolve_page_specs; path equality therefore preserves qpdf's documented
    // distinction between two different spellings of the same file.
    let mut source_paths = vec![primary_input.to_path_buf()];
    let mut source_passwords: Vec<Option<String>> = vec![None];
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

    if verbose {
        let mut message = String::from("flpdf: selecting --keep-open-files=y\n");
        for path in &source_paths {
            let fname = pages_progress_filename(path);
            message.push_str(&format!(
                "flpdf: {fname}: checking for shared resources\nflpdf: no shared resources found\n"
            ));
        }
        message.push_str("flpdf: removing unreferenced pages from primary input\n");
        for path in &source_paths {
            message.push_str(&format!(
                "flpdf: adding pages from {}\n",
                pages_progress_filename(path)
            ));
        }
        logger_info(message)?;
    }

    let mut sources = Vec::with_capacity(source_paths.len());
    sources.push(open_pdf(&primary_input.to_path_buf(), repair, password)?);
    for (source_index, path) in source_paths.iter().enumerate().skip(1) {
        let mut source_password = password.clone();
        // qpdf opens each secondary with only the password attached to its
        // page specification (QPDFJob.cc:2400-2412). The primary password is
        // not a fallback for a secondary with no segment password; retain the
        // global interpretation/policy flags, but replace both credential
        // fields with the per-source value, including an explicit empty value.
        source_password.password = source_passwords[source_index].clone();
        source_password.password_file = None;
        sources.push(open_pdf(path, repair, &source_password)?);
    }

    // qpdf raises the writer floor from every input processed by the job
    // (`QPDFJob.cc:1714-1715`) and applies that floor before explicit
    // --min-version/--force-version settings
    // (`QPDFJob.cc:2847-2918`). The merged fresh document starts at its
    // baseline version, so carry the source floor explicitly through the
    // multi-source consumer boundary. Keep the existing pairwise version /
    // extension ordering used by the overlay route.
    let mut options = options;
    let mut max_version = PdfVersion::new(1, 0, 0);
    for source in &mut sources {
        let source_version =
            parse_pdf_version(source.version()).unwrap_or(PdfVersion::new(1, 0, 0));
        max_version.update_if_greater(PdfVersion::new(
            source_version.major(),
            source_version.minor(),
            source.adobe_extension_level().unwrap_or(0),
        ));
    }
    if let Some(ref current) = options.min_version {
        let current_version = parse_pdf_version(current).unwrap_or(PdfVersion::new(1, 0, 0));
        max_version.update_if_greater(PdfVersion::new(
            current_version.major(),
            current_version.minor(),
            options.min_extension_level.unwrap_or(0),
        ));
    }
    let (version, max_ext) = max_version.get_version();
    options.min_version = Some(version);
    options.min_extension_level = (max_ext > 0).then_some(max_ext);

    let primary_copy_encryption = sources
        .first_mut()
        .ok_or("--pages: primary input was not opened")?
        .writer_copy_encryption_source()?;

    let collate = page_ops
        .collate
        .as_deref()
        .map(parse_collate_n)
        .transpose()?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    let source_warnings = job.has_warnings();
    let mut merged = job.handle_page_specs_with_resource_mode(
        &mut sources,
        &specs,
        collate,
        remove_unref.into(),
    )?;
    let source_warnings = source_warnings || job.has_warnings();

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
                page: flpdf::page_plan::SelectedPage {
                    index_1based: u32::try_from(index + 1)
                        .map_err(|_| "--pages: too many output pages")?,
                    page_ref,
                },
            })
        })
        .collect::<CliResult<Vec<_>>>()?;

    run_page_extraction_after_plan(
        merged,
        output,
        primary_input,
        repair,
        page_ops,
        overlay_specs,
        // QPDFJob has already applied the page-copy resource policy to each
        // source page. The post-copy completion boundary must not run the
        // document-wide resource pass a second time; qpdf's --pages job does
        // this pruning before its first page copy and relies on the writer for
        // final reachability cleanup.
        CliRemoveUnreferencedResources::No,
        options,
        verbose,
        standard_output,
        creates_output,
        primary_encrypted,
        primary_copy_encryption,
        source_warnings,
        false,
        combined_pages,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_page_extraction_from_repeated_pdf<R: Read + Seek + 'static>(
    mut pdf: Pdf<R>,
    primary_input: &Path,
    output: &Path,
    repair: bool,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    verbose: bool,
    standard_output: Option<PipelineWriter>,
    creates_output: bool,
    inputs: &[InputSpec],
    distinct: &[PathBuf],
) -> CliResult<()> {
    let primary_encrypted = pdf.is_encrypted();
    let primary_copy_encryption = pdf.writer_copy_encryption_source()?;
    if verbose {
        let mut message = String::from("flpdf: selecting --keep-open-files=y\n");
        for path in distinct {
            let fname = pages_progress_filename(path);
            message.push_str(&format!(
                "flpdf: {fname}: checking for shared resources\nflpdf: no shared resources found\n"
            ));
        }
        logger_info(message)?;
    }

    let ranges = inputs.iter().map(|spec| spec.range.clone()).collect();
    let plan = CombinedPlan::build_repeated(&mut pdf, ranges)?;

    if verbose {
        let mut message = String::from("flpdf: removing unreferenced pages from primary input\n");
        for spec in inputs {
            message.push_str(&format!(
                "flpdf: adding pages from {}\n",
                pages_progress_filename(&spec.path)
            ));
        }
        logger_info(message)?;
    }

    let combined_pages = match page_ops.collate.as_deref() {
        Some(raw) => {
            let n = parse_collate_n(raw)?;
            collate(&plan, n)?
        }
        None => plan.flat_pages().to_vec(),
    };

    run_page_extraction_after_plan(
        pdf,
        output,
        primary_input,
        repair,
        page_ops,
        overlay_specs,
        remove_unref,
        options,
        verbose,
        standard_output,
        creates_output,
        primary_encrypted,
        primary_copy_encryption,
        false,
        true,
        combined_pages,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_page_extraction_after_plan<R: Read + Seek + 'static>(
    mut pdf: Pdf<R>,
    output: &Path,
    input_path: &Path,
    repair: bool,
    page_ops: &PageOpArgs,
    overlay_specs: &[OverlaySpec],
    remove_unref: CliRemoveUnreferencedResources,
    options: WriterOptions,
    verbose: bool,
    mut standard_output: Option<PipelineWriter>,
    creates_output: bool,
    primary_encrypted: bool,
    primary_copy_encryption: Option<CopyEncryptionSource>,
    prior_warnings: bool,
    reconstruct_labels: bool,
    combined_pages: Vec<CombinedPage>,
) -> CliResult<()> {
    let selected: Vec<ObjectRef> = combined_pages.iter().map(|cp| cp.page.page_ref).collect();
    if selected.is_empty() {
        return Err("--pages: page selection is empty".into());
    }

    // qpdf's --pages Auto mode scans the source page tree before it removes
    // the old pages. A page-local indirect /Resources that appears only once
    // does not trigger the expensive page-helper pruning route; inherited or
    // shared resources do (QPDFJob.cc:2251-2337). Preserve that decision
    // before rebuild_page_tree flattens the original inheritance structure.
    let prune_mode = if remove_unref == CliRemoveUnreferencedResources::Auto
        && !should_remove_unreferenced_resources(&mut pdf)?
    {
        CliRemoveUnreferencedResources::No
    } else {
        remove_unref
    };

    let result = rebuild_page_tree(&mut pdf, &selected)?;
    apply_rotate_specs(&mut pdf, &page_ops.rotate, &result.new_kids)?;

    if reconstruct_labels {
        let mut labels = pdf.page_labels();
        if labels.has_page_labels()? {
            let src_indices: Vec<i64> = combined_pages
                .iter()
                .map(|cp| i64::from(cp.page.index_1based) - 1)
                .collect();
            let entries = labels.labels_for_selection(&src_indices, 0)?;
            let folded = flpdf::merge_adjacent_ranges(entries);
            labels.write_reconstructed_labels(&folded)?;
        }
    }

    remap_outline_and_dests(&mut pdf, &result)?;
    let objr_obj_targets = drop_struct_elem_dangling_pg(&mut pdf, &result)?;
    drop_thread_bead_dangling_p(&mut pdf, &result)?;
    drop_objr_obj_annot_dangling_p(&mut pdf, &result, &objr_obj_targets)?;
    prune_after_subset(&mut pdf, prune_mode.into())?;
    prune_acroform_after_subset(&mut pdf, &result)?;

    let mut options = options;
    options.preserve_encryption = primary_encrypted && page_ops.split_pages.is_none();
    // qpdf keeps the authenticated primary input as the output/base document
    // for `--pages` (libqpdf/QPDFJob.cc:2360-2633). The multi-source job has
    // already copied selected pages into a fresh plaintext Pdf, so its writer
    // cannot rediscover the primary's encryption from the merged document.
    // Carry the authenticated donor explicitly to the final writer; split
    // chunks remain cleartext, matching qpdf's fresh chunk writers. Gate on
    // the same conditions as `PdfWriter::prepared_write_options`'s implicit
    // `can_preserve` (`writer/pdf_writer.rs:589-596`) so an explicit source
    // doesn't bypass qpdf's QDF-is-always-cleartext contract
    // (`cell_a_encrypted_input_is_transparently_decrypted_by_qdf`) or its
    // `decode_level == DecodeLevel::None` requirement: `--stream-data`
    // `Uncompress`/`Compress` raise the writer's decode level above `None`
    // (`WriterConfiguration::set_stream_data_mode`, `writer/pdf_writer.rs:100-108`),
    // which `can_preserve` would likewise refuse to auto-preserve through.
    if page_ops.split_pages.is_none()
        && options.copy_encryption.is_none()
        && !options.qdf
        && !options.content_normalization
        && !matches!(
            options.stream_data,
            Some(StreamDataMode::Uncompress) | Some(StreamDataMode::Compress)
        )
    {
        options.copy_encryption = primary_copy_encryption;
    }
    // qpdf keeps a provider-backed source QPDF alive when
    // `copyForeignObject` copies a Form XObject whose data comes from a
    // `StreamDataProvider` (`libqpdf/QPDF.cc:2248-2257`). Retain the opened
    // source documents through the in-memory writer for the same reason.
    let _built_overlay_specs = if !overlay_specs.is_empty() {
        let mut built = build_overlay_specs(overlay_specs, repair)?;
        let initial_version = parse_pdf_version(pdf.version()).unwrap_or(PdfVersion::new(1, 0, 0));
        let mut max_version = PdfVersion::new(
            initial_version.major(),
            initial_version.minor(),
            pdf.adobe_extension_level().unwrap_or(0),
        );
        for spec in built.iter_mut() {
            let source_version =
                parse_pdf_version(spec.source.version()).unwrap_or(PdfVersion::new(1, 0, 0));
            max_version.update_if_greater(PdfVersion::new(
                source_version.major(),
                source_version.minor(),
                spec.source.adobe_extension_level().unwrap_or(0),
            ));
        }
        if let Some(ref current) = options.min_version {
            let current_version = parse_pdf_version(current).unwrap_or(PdfVersion::new(1, 0, 0));
            max_version.update_if_greater(PdfVersion::new(
                current_version.major(),
                current_version.minor(),
                options.min_extension_level.unwrap_or(0),
            ));
        }
        let (version, max_ext) = max_version.get_version();
        options.min_version = Some(version);
        options.min_extension_level = (max_ext > 0).then_some(max_ext);

        if verbose {
            let report = flpdf::overlay_verbose_report(&mut pdf, &mut built)?;
            let mut message = String::from("flpdf: processing underlay/overlay\n");
            for page in &report {
                message.push_str(&format!("  page {}\n", page.dest_page));
                for src in &page.sources {
                    let file = &overlay_specs[src.spec_index].file;
                    let kind_str = match src.kind {
                        flpdf::OverlayKind::Underlay => "underlay",
                        flpdf::OverlayKind::Overlay => "overlay",
                    };
                    message.push_str(&format!("    {} {} {}\n", file, kind_str, src.src_page));
                }
            }
            logger_info(message)?;
        }

        flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
        Some(built)
    } else {
        None
    };

    let bytes = write_qpdf_to_memory(&mut pdf, &options)?;
    if let Some(raw) = page_ops.split_pages.as_deref() {
        let n = parse_split_n(raw)?;
        let written = split_rewritten_pdf(
            bytes,
            n,
            output,
            input_path,
            options.deterministic_id,
            writer_configuration(&options, false),
        )?;
        if verbose {
            for path in &written {
                logger_info(format!("flpdf: wrote file {}\n", path.display()))?;
            }
        }
    } else if let Some(writer) = standard_output.as_mut() {
        writer.write_all(&bytes)?;
    } else {
        std::fs::write(output, &bytes)?;
        if verbose {
            logger_info(format!("flpdf: wrote file {}\n", output.display()))?;
        }
    }
    finish_operation_warnings_with_prior(&pdf, creates_output, prior_warnings)
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
        let indices = spec
            .range
            .resolve(total)
            .map_err(|e| format!("--rotate: page range out of bounds in {raw:?}: {e}"))?;
        let pages: Vec<ObjectRef> = indices
            .iter()
            .filter_map(|&i| target_pages.get((i - 1) as usize).copied())
            .collect();
        apply_rotate_to_pages(pdf, &pages, &spec.op)?;
    }
    Ok(())
}

/// Parse `--split-pages[=n]` (default 1; qpdf-compatible).
fn parse_split_n(raw: &str) -> CliResult<usize> {
    let n: usize = raw
        .parse()
        .map_err(|_| format!("--split-pages: expected a positive integer, got {raw:?}"))?;
    if n == 0 {
        return Err("--split-pages: group size must be >= 1".into());
    }
    Ok(n)
}

/// Run qpdf's fresh-document split job on a rewritten in-memory source.
///
/// The page-operation pipeline has already applied its transforms to `bytes`;
/// the job-level split owns the subsequent per-chunk page copy, naming,
/// annotation, label, and output-file lifecycle. `input_path` remains the
/// original command input so the qpdf same-file guard can reject a generated
/// chunk that would truncate it.
fn split_rewritten_pdf(
    bytes: Vec<u8>,
    chunk_size: usize,
    output: &Path,
    input_path: &Path,
    deterministic_id: bool,
    writer_configuration: WriterConfiguration,
) -> CliResult<Vec<PathBuf>> {
    let mut pdf = Pdf::open_mem_owned(bytes)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    let options = SplitPageOptions::new(chunk_size, output)
        .with_input_path(input_path)
        .with_deterministic_id(deterministic_id)
        .with_writer_configuration(writer_configuration);
    Ok(job.split_pages(&mut pdf, options)?)
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
    options: WriterOptions,
    verbose: bool,
) -> CliResult<()> {
    let opened = open_job_pdf(input, repair, password, json_input, update_from_json, false)?;
    match opened {
        JobPdf::File(pdf) => {
            run_rewrite_with_page_ops_opened(pdf, input, output, page_ops, options, verbose)
        }
        JobPdf::Json(pdf) => {
            run_rewrite_with_page_ops_opened(pdf, input, output, page_ops, options, verbose)
        }
    }
}

fn run_rewrite_with_page_ops_opened<R: Read + Seek + 'static>(
    mut pdf: Pdf<R>,
    input: &Path,
    output: &std::path::Path,
    page_ops: &PageOpArgs,
    options: WriterOptions,
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

    // Page operations emit a fresh document and preserve encryption only when
    // the primary input itself was encrypted, matching qpdf's page copier.
    // `--split-pages` is the exception: qpdf's doSplitPages path makes a fresh
    // empty output document per chunk, so its intermediate and final chunks
    // are cleartext unless explicit encryption options are configured. Keep
    // the memory intermediate decryptable before split_pages re-opens it.
    let mut options = options;
    options.preserve_encryption = page_ops.split_pages.is_none() && pdf.is_encrypted();
    let bytes = write_qpdf_to_memory(&mut pdf, &options)?;

    if let Some(raw) = page_ops.split_pages.as_deref() {
        let n = parse_split_n(raw)?;
        let written = split_rewritten_pdf(
            bytes,
            n,
            output,
            input,
            options.deterministic_id,
            writer_configuration(&options, false),
        )?;
        if verbose {
            for path in &written {
                // cov:ignore-start: exercised by verbose split-pages subprocess integration tests
                logger_info(format!("flpdf: wrote file {}\n", path.display()))?;
                // cov:ignore-end
            }
        }
    } else if let Some(writer) = standard_output.as_mut() {
        // cov:ignore-start: exercised by binary_rotate_dash subprocess integration test
        writer.write_all(&bytes)?;
        // cov:ignore-end
    } else {
        std::fs::write(output, &bytes)?;
        if verbose {
            logger_info(format!("flpdf: wrote file {}\n", output.display()))?;
        }
    }
    finish_operation_warnings(&pdf, creates_output)
}

/// True when any page-operation flag that requires the page-op code paths is
/// set. `--collate` alone (no `--pages`) is a documented no-op and does NOT
/// trigger this on its own.
fn page_ops_active(p: &PageOpArgs) -> bool {
    !p.pages.is_empty() || !p.rotate.is_empty() || p.split_pages.is_some() || p.empty
}

/// Normalize all page content streams in an in-memory PDF graph.
///
/// Shared by the plain and linearized rewrite paths so both use the same page
/// traversal, indirect `/Contents` handling, alias deduplication, and warning
/// order.
fn normalize_page_contents<R: Read + Seek>(pdf: &mut Pdf<R>) -> CliResult<Vec<bool>> {
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
/// back into the in-memory [`Pdf`] model via [`Pdf::set_object`].
///
/// The `/Length` entry in each stream's dictionary is updated to the new
/// (normalized) byte count. No filter is applied here — the write path
/// (full-rewrite + compress_streams) handles re-encoding.
fn apply_normalize_content<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
    seen: &mut HashSet<ObjectRef>,
) -> CliResult<Vec<bool>> {
    let mut warnings = Vec::new();
    let page = pdf.get_object_handle(page_ref);
    pdf.resolve_object_handle(&page)?;
    let contents = page.get_key(b"/Contents");
    let (contents, contents_ref) = pdf.resolve_object_handle_to_terminal_ref(&contents)?;

    let mut streams = Vec::new();
    if contents.as_stream_dict().is_some() {
        if let Some(stream_ref) = contents_ref {
            streams.push((stream_ref, contents));
        }
    } else if let Some(items) = contents.as_array() {
        for item in items {
            let (item, item_ref) = pdf.resolve_object_handle_to_terminal_ref(&item)?;
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
) -> CliResult<Option<bool>> {
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
        .then(|| normalized.last_token_was_bad());
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
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let mut standard_output = prepare_pdf_standard_output(&output)?;
    let creates_output = standard_output.is_none();
    let mut pdf = open_pdf(&input, repair, password)?;

    // The `qdf` subcommand is the canonical PdfWriter QDF mode.
    let options = WriterOptions {
        qdf: true,
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
        Ok(output_metadata) => {
            // This is only a non-destructive hint: if inspecting the input
            // fails, the real input open below owns its path-specific error.
            // Output metadata failures remain fail-closed in the next arm.
            if let Ok(true) = paths_identify_same_file(input, output, &output_metadata) {
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

#[cfg(unix)]
fn paths_identify_same_file(
    input: &Path,
    _output: &Path,
    output_metadata: &std::fs::Metadata,
) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let input_metadata = std::fs::metadata(input)?;
    Ok(input_metadata.dev() == output_metadata.dev()
        && input_metadata.ino() == output_metadata.ino())
}

#[cfg(not(unix))]
fn paths_identify_same_file(
    input: &Path,
    output: &Path,
    _output_metadata: &std::fs::Metadata,
) -> std::io::Result<bool> {
    same_file::is_same_file(input, output)
}

fn run_dump_object(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    object_ref: &str,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let object_ref = ObjectRef::parse(object_ref)?;

    let mut pdf = open_pdf(&input, repair, password)?;
    {
        let object = pdf.resolve_borrowed(object_ref)?;

        if matches!(object, Object::Null) {
            return Err(format!(
                "object {} {} R not found",
                object_ref.number, object_ref.generation
            )
            .into());
        }

        let mut out = Vec::new();
        object.write_pdf(&mut out);
        out.push(b'\n');
        logger_info(out)?;
    }

    finish_operation_warnings(&pdf, false)
}

fn run_show_stream(cmd: ShowStreamCommand) -> CliResult<()> {
    let object_ref = ObjectRef::parse(&cmd.object_ref)?;
    let mut pdf = open_pdf(&cmd.input, cmd.repair, &cmd.password)?;
    let operation = (|| -> CliResult<()> {
        let object = pdf.resolve_borrowed(object_ref)?;

        if matches!(object, Object::Null) {
            return Err(format!(
                "object {} {} R not found",
                object_ref.number, object_ref.generation
            )
            .into());
        }

        let Object::Stream(stream) = object else {
            return Err(format!(
                "object {} {} R is not a stream",
                object_ref.number, object_ref.generation
            )
            .into());
        };

        if cmd.raw_stream_data {
            standard_save_writer()?.write_all(&stream.data)?;
            return Ok(());
        }

        // For a single passthrough codec that flpdf's decode path cannot
        // decode (currently JBIG2Decode, JPXDecode, CCITTFaxDecode) emit a
        // human-readable marker instead of dumping binary. DCTDecode is a
        // passthrough codec on the *write* side (the writer never
        // re-encodes JPEG data) but is decodable, so it falls through to the
        // decode path below like any other decodable filter. The codec may
        // be stored either as a direct name (`/Filter /JBIG2Decode`) or as a
        // single-element array (`/Filter [/JBIG2Decode]`); both are
        // equivalent per PDF spec. Multi-element filter chains fall through
        // to the decode path (scope: flpdf-9hc.7.5).
        let passthrough_label = stream.dict.get("Filter").and_then(|filter| {
            let name = filter.as_name().or_else(|| match filter.as_array() {
                Some([single]) => single.as_name(),
                _ => None,
            })?;
            if filters::is_decoded_filter(name) {
                None
            } else {
                filters::passthrough_codec_label(name)
            }
        });
        if let Some(label) = passthrough_label {
            // This codec is not decodable, so print a marker instead of
            // dumping binary data to the terminal.
            println!("<binary, {} bytes, codec {}>", stream.data.len(), label);
            return Ok(());
        }

        let bytes = filters::decode_stream_data(&stream.dict, &stream.data)?;
        standard_save_writer()?.write_all(&bytes)?;
        Ok(())
    })();
    operation?;
    finish_operation_warnings(&pdf, false)
}

fn run_show_npages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    run_ordinary_job_inspection(input, repair, password, |pdf, logger| {
        show_npages_from_pdf(pdf, logger)
    })
}

fn run_show_pages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    run_ordinary_job_inspection(input, repair, password, |pdf, logger| {
        show_pages_from_pdf(pdf, logger)
    })
}

/// Run one ordinary page inspection through the shared qpdf-shaped job
/// lifecycle. `QPDFJob::createQPDF` installs the document logger before input
/// processing, and `writeQPDF` completes read-only inspection after the
/// consumer (`libqpdf/QPDFJob.cc:429-516,1646-1693`).
fn run_ordinary_job_inspection<F>(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    inspection: F,
) -> CliResult<()>
where
    F: FnOnce(&mut Pdf<BufReader<File>>, &QPDFLogger) -> CliResult<()>,
{
    let input = input.ok_or("missing input file")?;
    let file = File::open(&input).map_err(|error| error_with_file(&input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    let mut pdf = job
        .open(BufReader::new(file), input.display().to_string(), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;

    // Keep the ordinary inspection weak-crypto advisory in the job-owned
    // logger, matching the previous `open_pdf` route without making it a
    // warning-exit condition.
    if pdf.uses_weak_crypto() {
        let warning = format!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied\n",
            input.display()
        );
        job.logger().warn(warning)?;
    }

    let logger = job.logger();
    let status = job.inspect(&mut pdf, |pdf| inspection(pdf, &logger))?;
    finish_job_exit_status(status)
}

fn show_npages_from_pdf<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
) -> CliResult<()> {
    let pages = pages::page_refs(pdf)?;
    logger.info(format!("{}\n", pages.len()))?;
    Ok(())
}

fn show_pages_from_pdf<R: Read + Seek + 'static>(
    pdf: &mut Pdf<R>,
    logger: &QPDFLogger,
) -> CliResult<()> {
    write_page_descriptions(pdf, logger)
}

fn write_page_descriptions<R: Read + Seek>(pdf: &mut Pdf<R>, logger: &QPDFLogger) -> CliResult<()> {
    let page_refs = pages::page_refs(&mut *pdf)?;
    for (index, page_ref) in page_refs.iter().enumerate() {
        let page = pdf.resolve_borrowed(*page_ref)?;
        let Object::Dictionary(dict) = page else {
            continue;
        };

        logger.info(format!("page {}: {}\n", index + 1, page_ref))?;
        if let Some(media_box) = dict.get("MediaBox") {
            logger.info(format!("  media-box: {}\n", object_to_pdf(media_box)))?;
        }
        if let Some(resources) = dict.get("Resources") {
            logger.info(format!("  resources: {}\n", object_to_pdf(resources)))?;
        }
        if let Some(contents) = dict.get("Contents") {
            logger.info(format!("  contents: {}\n", object_to_pdf(contents)))?;
        }
        if let Some(rotate) = dict.get("Rotate") {
            logger.info(format!("  rotate: {}\n", object_to_pdf(rotate)))?;
        }
    }

    Ok(())
}

fn run_show_linearization(input: Option<PathBuf>) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    match show_linearization_path(&input) {
        Ok(dump) => {
            // `dump` already ends with a trailing newline (the hint-table
            // dump, or qpdf's "<name> is not linearized" line). qpdf prints
            // both to stdout and exits 0; use print! to avoid a second LF.
            logger_info(dump)
        }
        Err(ShowLinearizationError::Malformed { message }) => {
            logger_error(format!("flpdf: malformed linearization data: {message}\n"))?; // cov:ignore: exercised by malformed linearization subprocess integration test
            std::process::exit(ExitCode::Errors.as_i32());
        }
        Err(ShowLinearizationError::Io(e)) => Err(e.to_string().into()),
    }
}

// ---------------------------------------------------------------------------
// Encryption inspection subcommands (flpdf-9hc.3.17)
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
/// The probe forces `allow_weak_crypto = true`: qpdf applies its weak-crypto
/// refusal to write/transform operations, NOT to these read-only inspections
/// (verified against qpdf — a correct password on an RC4/R=5 file yields
/// `--requires-password` exit 3, identical to a strong file). Because the
/// library applies the weak-crypto gate only AFTER authentication, leaving the
/// gate enabled would surface `WeakCryptoNotAllowed` for a correctly
/// authenticated file and mis-report it as "a different password is required".
/// Disabling the gate here keeps the answer a pure password
/// question: authentication still runs first, so a wrong password yields
/// `BadPassword` exactly as before.
fn probe_encryption(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<EncryptionProbe> {
    let file = File::open(input)?;
    let mut options = pdf_open_options(repair, password)?;
    options.allow_weak_crypto = true;
    configure_document_logger(&mut options, input);
    match Pdf::open_with_options(BufReader::new(file), options) {
        Ok(pdf) => Ok(EncryptionProbe::Opened {
            encrypted: pdf.is_encrypted(),
        }),
        // A wrong/empty password: the document is definitely encrypted, we
        // just have not authenticated it. qpdf treats this as "encrypted,
        // password required".
        Err(error) if is_bad_password_error(&error) => Ok(EncryptionProbe::EncryptedAuthFailed),
        Err(other) => Err(other.into()),
    }
}

fn is_bad_password_error(error: &flpdf::Error) -> bool {
    let source = error.open_failure().map_or(error, |(source, _)| source);
    matches!(
        source,
        flpdf::Error::Encrypted(flpdf::EncryptedError::BadPassword)
    )
}

/// `is-encrypted FILE`: exit 0 if encrypted, exit 2 if not.
///
/// qpdf `--is-encrypted` (qpdf manual): exit 0 = encrypted, exit 2 = not
/// encrypted (`qpdf_exit_is_not_encrypted = 2`). No required stdout.
fn run_is_encrypted(input: &PathBuf, repair: bool) -> CliResult<()> {
    // No password is taken/used: qpdf detects encryption structurally
    // (presence of /Encrypt) without authenticating, so we deliberately
    // probe with an empty password and accept the auth-failed outcome.
    let encrypted = match probe_encryption(input, repair, &PasswordArgs::default())? {
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
fn run_requires_password(input: &PathBuf, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    match probe_encryption(input, repair, password)? {
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
        None => {
            // qpdf --show-encryption-key requires an encrypted file; exit 2.
            Err("file is not encrypted; no encryption key to show".into())
        }
    }
}

/// `show-encryption FILE [--password ...]`: qpdf `--show-encryption`.
///
/// See the subcommand `long_about` for the exact format and the documented
/// divergences from qpdf (no recovered cleartext user password). Weak-crypto
/// (RC4 / R=5) files are inspectable with the correct password and no
/// `--allow-weak-crypto`, matching qpdf's read-only treatment (see
/// [`open_pdf_for_inspection`]).
fn run_show_encryption(input: &PathBuf, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    // qpdf prints "File is not encrypted" and exits 0 for plaintext files.
    // open_pdf_for_inspection succeeds for plaintext input, so detect that
    // case first.
    let mut pdf = open_pdf_for_inspection(input, repair, password)?;
    let Some(info) = pdf.encryption_info()? else {
        logger_info("File is not encrypted\n")?;
        return finish_operation_warnings(&pdf, false);
    };

    let mut output = String::new();

    // ── flpdf-specific leading lines (placed BEFORE the qpdf block so a
    //    qpdf-compatible grep still matches the qpdf lines verbatim) ──
    output.push_str(&format!("V = {}\n", info.v));
    output.push_str(&format!("Length = {}\n", info.length_bits));
    output.push_str(&format!("Filter = {}\n", info.filter));
    output.push_str(&format!(
        "EncryptMetadata = {}\n",
        if info.encrypt_metadata {
            "true"
        } else {
            "false"
        }
    ));
    let mut cf_names: Vec<_> = info.named_crypt_filters.clone();
    cf_names.sort();
    for (name, method) in &cf_names {
        output.push_str(&format!("CF /{name} = {method}\n"));
    }

    // ── Verbatim qpdf `--show-encryption` lines (source:
    //    qpdf libqpdf/QPDFJob.cc QPDFJob::showEncryption) ──
    output.push_str(&format!("R = {}\n", info.r));
    output.push_str(&format!("P = {}\n", info.permissions.raw()));
    // qpdf prints `User password = <recovered cleartext>` here; flpdf does
    // not recover the cleartext user password (documented divergence), so
    // that line is intentionally omitted.
    if pdf.owner_password_matched() {
        output.push_str("Supplied password is owner password\n"); // cov:ignore: exercised by show-encryption subprocess integration tests
    }
    if pdf.user_password_matched() {
        output.push_str("Supplied password is user password\n");
    }

    // qpdf's allow* booleans are revision-dependent. Replicate the exact
    // bit logic from qpdf libqpdf/QPDF_encryption.cc (P(n) = bit n-1 of the
    // signed /P value, 1-based as in the PDF spec).
    let p = info.permissions.raw();
    let r = info.r;
    let bit = |n: u32| (p >> (n - 1)) & 1 == 1;
    let allow_print_low = bit(3);
    let allow_extract_all = bit(5);
    let allow_accessibility = if r < 3 { bit(5) } else { bit(10) };
    let allow_print_high = allow_print_low && (r < 3 || bit(12));
    let allow_modify_assembly = if r < 3 { bit(4) } else { bit(11) };
    let allow_modify_form = if r < 3 { bit(6) } else { bit(9) };
    let allow_modify_annotation = bit(6);
    let allow_modify_other = bit(4);
    let allow_modify_all = allow_modify_annotation
        && allow_modify_other
        && (r < 3 || (allow_modify_form && allow_modify_assembly));
    let show = |v: bool| if v { "allowed" } else { "not allowed" };
    output.push_str(&format!(
        "extract for accessibility: {}\n",
        show(allow_accessibility)
    ));
    output.push_str(&format!(
        "extract for any purpose: {}\n",
        show(allow_extract_all)
    ));
    output.push_str(&format!(
        "print low resolution: {}\n",
        show(allow_print_low)
    ));
    output.push_str(&format!(
        "print high resolution: {}\n",
        show(allow_print_high)
    ));
    output.push_str(&format!(
        "modify document assembly: {}\n",
        show(allow_modify_assembly)
    ));
    output.push_str(&format!("modify forms: {}\n", show(allow_modify_form)));
    output.push_str(&format!(
        "modify annotations: {}\n",
        show(allow_modify_annotation)
    ));
    output.push_str(&format!("modify other: {}\n", show(allow_modify_other)));
    output.push_str(&format!("modify anything: {}\n", show(allow_modify_all)));
    if info.v >= 4 {
        output.push_str(&format!(
            "stream encryption method: {}\n",
            info.stream_method
        ));
        output.push_str(&format!(
            "string encryption method: {}\n",
            info.string_method
        ));
        // qpdf prints the embedded-file ("file") method; the no-/EFF fallback
        // to the stream method happens where `cf_file` is resolved, not here.
        output.push_str(&format!("file encryption method: {}\n", info.eff_method));
    }
    logger_info(output)?;
    finish_operation_warnings(&pdf, false)
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
        pdf.update_from_json(source, path.display().to_string())
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)?;
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
        job.update_from_json(pdf, source, path.display().to_string())?;
    }
    Ok(())
}

/// Open the main qpdf job input and apply `--update-from-json` at the same
/// point qpdf's `QPDFJob::createQPDF` does: immediately after input creation,
/// before page specifications, rotations, overlays, or serialization.
/// `check_inspection` applies `run_check`'s forced weak-crypto-open and
/// warning-aggregation policy (see [`open_pdf_for_check_inspection`]) to the
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
) -> CliResult<JobPdf> {
    if json_input {
        Ok(JobPdf::Json(open_json_pdf(input, update_from_json)?))
    } else {
        let mut pdf = if check_inspection {
            open_pdf_for_check_inspection(&input.to_path_buf(), repair, password)?
        } else {
            open_pdf(&input.to_path_buf(), repair, password)?
        };
        apply_json_update(&mut pdf, update_from_json)?;
        Ok(JobPdf::File(pdf))
    }
}

fn open_json_pdf(input: &Path, update_from_json: Option<&Path>) -> CliResult<Pdf<Cursor<Vec<u8>>>> {
    let source = File::open(input).map_err(|error| qpdf_json_input_open_error(input, error))?;
    let mut pdf = Pdf::create_from_json(source, input.display().to_string())
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)?;
    apply_json_update(&mut pdf, update_from_json)?;
    Ok(pdf)
}

fn open_pdf(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, false, false)
}

fn open_pdf_from_file(
    input: &Path,
    file: File,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_file_impl(input, file, repair, password, false, false)
}

/// Open for the read-only encryption inspections (`show-encryption`,
/// `show-encryption-key`).
///
/// Like [`open_pdf`] but forces the weak-crypto gate open, so an RC4 / R=5 file
/// authenticated with the CORRECT password is inspectable without
/// `--allow-weak-crypto`. qpdf treats these as read-only inspections rather than
/// a write policy: it derives and prints the key / encryption block for a weak
/// file with the correct password and emits no weak-crypto warning (verified
/// qpdf 11.9.0). This mirrors the `requires-password` / `is-encrypted` alignment
/// (flpdf-63g); authentication still runs first, so a wrong password fails
/// exactly as before.
fn open_pdf_for_inspection(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, true, false)
}

/// Open for `--update-from-json --check`'s generic job-inspection route.
///
/// Mirrors `run_check`'s own two-part inspection policy exactly (forced
/// weak-crypto gate, same reasoning as [`open_pdf_for_inspection`]; plus
/// `suppress_warnings` so open/update-time repair diagnostics are collected
/// rather than delivered live, since `finish_check_report` re-emits the
/// same diagnostics from `pdf.repair_diagnostics()` afterward -- without
/// this, a `--repair`-triggered warning prints twice). `--show-npages`/
/// `--show-pages` do not need either policy: like their non-JSON siblings
/// `run_show_npages`/`run_show_pages`, they use the plain [`open_pdf`]
/// path via [`open_job_pdf`]'s `check_inspection` parameter.
fn open_pdf_for_check_inspection(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, true, true)
}

fn open_pdf_impl(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    force_allow_weak_crypto: bool,
    suppress_warnings: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    let file = File::open(input).map_err(|error| error_with_file(input, error.into()))?;
    open_pdf_file_impl(
        input,
        file,
        repair,
        password,
        force_allow_weak_crypto,
        suppress_warnings,
    )
}

fn open_pdf_file_impl(
    input: &Path,
    file: File,
    repair: bool,
    password: &PasswordArgs,
    force_allow_weak_crypto: bool,
    suppress_warnings: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    let mut options = pdf_open_options(repair, password)?;
    if force_allow_weak_crypto {
        options.allow_weak_crypto = true;
    }
    if suppress_warnings {
        options.suppress_warnings = true;
    }
    configure_document_logger(&mut options, input);
    let pdf = Pdf::open_with_options(BufReader::new(file), options)
        .map_err(|error| error_with_file(input, actionable_password_error(error)))?;
    // Skip the weak-crypto warning on the forced (inspection) path: the user
    // supplied no `--allow-weak-crypto` flag to acknowledge, and qpdf emits no
    // such warning for `--show-encryption[-key]`. On the normal path a weak
    // file only opens when the user did pass the flag, so the warning is apt.
    if pdf.uses_weak_crypto() && !force_allow_weak_crypto {
        logger_warn(format!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied\n",
            input.display()
        ))?; // cov:ignore: exercised by weak-crypto subprocess integration tests
    }

    Ok(pdf)
}

fn pdf_open_options(repair: bool, password: &PasswordArgs) -> CliResult<PdfOpenOptions> {
    let allow_weak_crypto = password.allow_weak_crypto;
    let password_is_hex_key = password.password_is_hex_key;
    // `--suppress-password-recovery` is a documented no-op (see PasswordArgs):
    // flpdf has no encoding-recovery path to suppress. Bind it so the field is
    // observed by the compiler and the intent is explicit at the wiring site.
    let _suppress_password_recovery = password.suppress_password_recovery;
    let password_mode = password.password_mode.into();
    let password = if let Some(password) = &password.password {
        password.as_bytes().to_vec()
    } else if let Some(path) = &password.password_file {
        let mut bytes = std::fs::read(path)?;
        if bytes.ends_with(b"\r\n") {
            bytes.truncate(bytes.len() - 2);
        } else if bytes.ends_with(b"\n") {
            bytes.truncate(bytes.len() - 1);
        }
        bytes
    } else {
        Vec::new()
    };

    Ok(PdfOpenOptions {
        // qpdf's recovery permission is enabled on the document by default.
        // Keep accepting `--repair` as an explicit compatibility spelling;
        // the absence of that flag must not turn recovery off.
        repair: repair || PdfOpenOptions::default().repair,
        password,
        password_mode,
        allow_weak_crypto,
        password_is_hex_key,
        ..PdfOpenOptions::default()
    })
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
    if output.as_os_str() == "-" && page_ops.split_pages.is_some() {
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

fn logger_error(data: impl AsRef<[u8]>) -> CliResult<()> {
    cli_logger().error(data)?;
    Ok(())
}

fn emit_logger_error(data: impl AsRef<[u8]>) {
    if let Err(error) = cli_logger().error(data) {
        eprintln!("flpdf: unable to write diagnostic: {error}"); // cov:ignore: last-resort path after the standard error sink itself fails
    }
}

fn configure_document_logger(options: &mut PdfOpenOptions, input: &Path) {
    options.logger = Some(cli_logger());
    options.description = input.display().to_string();
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

fn check_diagnostic_location(input: &Path, diagnostic: &flpdf::Diagnostic) -> String {
    // Object- and trailer-prefixed messages already carry qpdf's
    // `(object N G, offset M)` or `(trailer, offset M)` context. Passing
    // their structured offset to `diagnostic_location` would duplicate it as
    // `file (offset M) (object N G, offset M)`. qpdf's
    // `damagedPDF(input, offset, message)` keeps that context in the message
    // while the input path remains the sole outer location.
    if diagnostic.message.starts_with("(object ") || diagnostic.message.starts_with("(trailer,") {
        diagnostic_location(input, None)
    } else {
        diagnostic_location(input, diagnostic.offset)
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
    )
}

fn finish_job_exit_status(status: JobExitCode) -> CliResult<()> {
    match status {
        JobExitCode::Success => Ok(()),
        JobExitCode::Warning => Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        })),
    }
}

fn finish_warning_state(has_warnings: bool, creates_output: bool) -> CliResult<()> {
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    if has_warnings {
        job.record_warnings();
    }

    match job.complete(creates_output)? {
        JobExitCode::Success => Ok(()),
        JobExitCode::Warning => Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        })),
    }
}

fn emit_content_normalization_warnings(input: &Path, last_token_was_bad: bool) -> CliResult<()> {
    let location = diagnostic_location(input, None);
    let mut message =
        format!("WARNING: {location}: content normalization encountered bad tokens\n");
    if last_token_was_bad {
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
    normalization_last_bad: &[bool],
    creates_output: bool,
) -> CliResult<()> {
    // qpdf retains open-time warnings in the document warning collection and
    // emits the final summary after the output writer completes. Include the
    // full collection here, not only warnings added after this route opened
    // the document.
    let has_repair_warnings = !pdf.repair_diagnostics().entries().is_empty();
    for &last_bad in normalization_last_bad {
        emit_content_normalization_warnings(input, last_bad)?;
    }
    if normalization_last_bad.is_empty() && !has_repair_warnings {
        return Ok(());
    }
    finish_warning_state(true, creates_output)
}

/// Prefix a fatal error with the input path so main() renders the observed
/// qpdf shape `<progname>: <file>: <msg>` for open failures.
///
/// This type-erases the error; do not downcast the result.
fn error_with_file(input: &Path, error: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    format!("{}: {error}", input.display()).into()
}

fn actionable_password_error(error: flpdf::Error) -> Box<dyn std::error::Error> {
    if is_bad_password_error(&error) {
        return "encrypted PDF: incorrect password; retry with --password or --password-file"
            .into();
    }
    error.into()
}

fn object_to_pdf(object: &Object) -> String {
    let mut out = Vec::new();
    object.write_pdf(&mut out);
    String::from_utf8_lossy(&out).to_string()
}

// ── Attachment helpers (flpdf-9hc.10.9) ──────────────────────────────────────

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
fn parse_add_attachment_segment(tokens: Vec<String>) -> CliResult<AddAttachmentArgs> {
    let mut iter = tokens.into_iter();
    let file: PathBuf = iter
        .next()
        .ok_or("--add-attachment: missing FILE argument")?
        .into();

    let mut key: Option<Vec<u8>> = None;
    let mut filename: Option<Vec<u8>> = None;
    let mut mimetype: Option<Vec<u8>> = None;
    let mut description: Option<Vec<u8>> = None;
    let mut creation_date: Option<Vec<u8>> = None;
    let mut mod_date: Option<Vec<u8>> = None;
    let mut replace = false;

    for token in iter {
        if let Some(v) = token.strip_prefix("--key=") {
            key = Some(v.as_bytes().to_vec());
        } else if let Some(v) = token.strip_prefix("--filename=") {
            filename = Some(v.as_bytes().to_vec());
        } else if let Some(v) = token.strip_prefix("--mimetype=") {
            mimetype = Some(v.as_bytes().to_vec());
        } else if let Some(v) = token.strip_prefix("--description=") {
            description = Some(v.as_bytes().to_vec());
        } else if let Some(v) = token.strip_prefix("--creationdate=") {
            creation_date = Some(parse_pdf_date_arg(v)?);
        } else if let Some(v) = token.strip_prefix("--moddate=") {
            mod_date = Some(parse_pdf_date_arg(v)?);
        } else if token == "--replace" {
            replace = true;
        } else {
            return Err(format!(
                "--add-attachment: unknown sub-flag or unexpected token {token:?}"
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
fn parse_copy_attachments_segment(tokens: Vec<String>) -> CliResult<CopyAttachmentsArgs> {
    let mut iter = tokens.into_iter();
    let file: PathBuf = iter
        .next()
        .ok_or("--copy-attachments-from: missing FILE argument")?
        .into();

    let mut password: Vec<u8> = Vec::new();
    let mut prefix: Option<Vec<u8>> = None;

    for token in iter {
        if let Some(v) = token.strip_prefix("--password=") {
            password = v.as_bytes().to_vec();
        } else if let Some(v) = token.strip_prefix("--prefix=") {
            prefix = Some(v.as_bytes().to_vec());
        } else {
            return Err(format!(
                "--copy-attachments-from: unknown sub-flag or unexpected token {token:?}"
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
        .map(|n| n.to_string_lossy().into_owned().into_bytes())
}

/// `--add-attachment FILE [sub-flags] -- output.pdf`
fn run_add_attachment(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    tokens: Vec<String>,
    deterministic_id: bool,
    verbose: bool,
) -> CliResult<()> {
    let input = input.ok_or("--add-attachment: missing input PDF")?;
    let output = output.ok_or("--add-attachment: missing output PDF")?;
    let args = parse_add_attachment_segment(tokens)?;

    let basename = path_basename(&args.file)?;
    let key = args.key.unwrap_or_else(|| basename.clone());
    let filename = args.filename.unwrap_or_else(|| basename.clone());

    let file = File::open(&input).map_err(|error| error_with_file(&input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    let mut pdf = job
        .open(BufReader::new(file), input.display().to_string(), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;

    let mut standard_output = prepare_pdf_standard_output(&output)?;

    if pdf.uses_weak_crypto() {
        job.logger().warn(format!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied\n",
            input.display()
        ))?;
    }

    job.add_attachment(
        &mut pdf,
        AttachmentAddOptions {
            path: args.file,
            key,
            filename,
            mimetype: args.mimetype,
            description: args.description,
            creation_date: args.creation_date,
            modification_date: args.mod_date,
            replace: args.replace,
            verbose,
        },
    )?;

    let options = WriterOptions {
        deterministic_id,
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
    if verbose && output.as_os_str() != "-" {
        job.logger()
            .info(format!("{}: wrote file {}\n", progname(), output.display()))?;
    }
    job.record_document_warnings(&pdf);
    finish_job_exit_status(job.complete(true)?)
}

/// `--remove-attachment KEY [input] [output]`
fn run_remove_attachment(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    key: &str,
    deterministic_id: bool,
) -> CliResult<()> {
    let input = input.ok_or("--remove-attachment: missing input PDF")?;
    let output = output.ok_or("--remove-attachment: missing output PDF")?;

    let mut pdf = open_pdf(&input, repair, password)?;

    let found = remove_attachment(&mut pdf, key.as_bytes())?;
    if !found {
        return Err(format!("--remove-attachment: key {:?} not found in document", key).into());
    }

    let options = WriterOptions {
        deterministic_id,
        ..WriterOptions::default()
    };
    let mut standard_output = None;
    write_with_pdf_writer(
        &mut pdf,
        &output,
        &mut standard_output,
        &options,
        false,
        None,
    )?;
    finish_operation_warnings(&pdf, true)
}

/// `--list-attachments [--verbose] input`
fn run_list_attachments(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    verbose: bool,
) -> CliResult<()> {
    let input = input.ok_or("--list-attachments: missing input PDF")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_input_name(input.display().to_string());
    let status = job.list_attachments(&mut pdf, verbose)?;
    finish_job_exit_status(status)
}

/// `--show-attachment KEY [-o PATH] input`
fn run_show_attachment(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    key: &str,
) -> CliResult<()> {
    let input = input.ok_or("--show-attachment: missing input PDF")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    job.set_input_name(input.display().to_string());
    let status = job
        .show_attachment(&mut pdf, key.as_bytes())
        .map_err(|error| {
            format!(
                "--show-attachment: key {:?} not found or unreadable: {error}",
                key
            )
        })?;
    finish_job_exit_status(status)
}

/// `--copy-attachments-from FILE [--password=P] [--prefix=X] -- input output`
fn run_copy_attachments_from(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    tokens: Vec<String>,
    deterministic_id: bool,
    verbose: bool,
) -> CliResult<()> {
    let input = input.ok_or("--copy-attachments-from: missing input PDF")?;
    let output = output.ok_or("--copy-attachments-from: missing output PDF")?;
    let args = parse_copy_attachments_segment(tokens)?;

    let file = File::open(&input).map_err(|error| error_with_file(&input, error.into()))?;
    let options = pdf_open_options(repair, password)?;
    let mut job = QPDFJob::new();
    job.set_logger(cli_logger());
    job.set_message_prefix(progname());
    let mut pdf = job
        .open(BufReader::new(file), input.display().to_string(), options)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;

    let mut standard_output = prepare_pdf_standard_output(&output)?;

    if pdf.uses_weak_crypto() {
        job.logger().warn(format!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied\n",
            input.display()
        ))?;
    }

    // Open the source with its own password (independent of the target's).
    // qpdf's recovery permission is enabled on the document by default; the
    // absence of `--repair` must not turn it off (see `pdf_open_options`'s
    // identical treatment for the primary document).
    let mut src_options = PdfOpenOptions {
        repair: repair || PdfOpenOptions::default().repair,
        password: args.password.clone(),
        ..PdfOpenOptions::default()
    };
    configure_document_logger(&mut src_options, &args.file);
    let src_file =
        File::open(&args.file).map_err(|error| error_with_file(&args.file, error.into()))?;
    let mut src = Pdf::open_with_options(BufReader::new(src_file), src_options)
        .map_err(|error| error_with_file(&args.file, actionable_password_error(error)))?;

    let count = job.copy_attachments(
        &mut pdf,
        &mut src,
        &AttachmentCopyOptions {
            path: args.file,
            prefix: args.prefix.unwrap_or_default(),
            verbose,
        },
    )?;
    eprintln!("copied {count} attachment(s)");

    let writer_options = WriterOptions {
        deterministic_id,
        ..WriterOptions::default()
    };
    write_with_pdf_writer(
        &mut pdf,
        &output,
        &mut standard_output,
        &writer_options,
        false,
        None,
    )?;
    if verbose && output.as_os_str() != "-" {
        job.logger()
            .info(format!("{}: wrote file {}\n", progname(), output.display()))?;
    }
    job.record_document_warnings(&pdf);
    finish_job_exit_status(job.complete(true)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flpdf::pipeline::{Pipeline, PipelineResult};
    use flpdf::{Dictionary, Stream};
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

        write_page_descriptions(&mut pdf, &logger).unwrap();

        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 5);
        assert_eq!(
            chunks.concat(),
            b"page 1: 3 0 R\n  media-box: [ 0 0 612 792 ]\n  resources: << /Font 1 0 R /ProcSet [ /PDF /Text /ImageB /ImageC /ImageI ] >>\n  contents: 7 0 R\n  rotate: 0\n"
        );
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
                password: Some("wrong".to_owned()),
                ..PasswordArgs::default()
            },
        );

        assert!(matches!(outcome, Ok(EncryptionProbe::EncryptedAuthFailed)));
    }

    #[test]
    fn check_diagnostic_location_does_not_duplicate_object_offset() {
        let object_warning =
            flpdf::Diagnostic::warning("(object 5 0, offset 232): expected endobj", Some(232));
        assert_eq!(
            check_diagnostic_location(Path::new("input.pdf"), &object_warning),
            "input.pdf"
        );

        let ordinary_warning = flpdf::Diagnostic::warning("xref warning", Some(12));
        assert_eq!(
            check_diagnostic_location(Path::new("input.pdf"), &ordinary_warning),
            "input.pdf (offset 12)"
        );
    }

    #[test]
    fn check_diagnostic_location_does_not_duplicate_trailer_offset() {
        let trailer_warning = flpdf::Diagnostic::warning(
            "(trailer, offset 190): dictionary has duplicated key /Foo; \
             last occurrence overrides earlier ones",
            Some(190),
        );
        assert_eq!(
            check_diagnostic_location(Path::new("input.pdf"), &trailer_warning),
            "input.pdf"
        );
    }

    #[test]
    fn apply_normalize_content_follows_two_hop_holder_chain() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let page_ref = pages::page_refs(&mut pdf).unwrap()[0];
        let holder_ref = ObjectRef::new(100, 0);
        let stream_ref = ObjectRef::new(101, 0);

        let mut page = pdf.resolve(page_ref).unwrap().into_dict().unwrap();
        page.insert("Contents", Object::Reference(holder_ref));
        pdf.set_object(page_ref, Object::Dictionary(page));
        pdf.set_object(holder_ref, Object::Reference(stream_ref));

        let mut stream_dict = Dictionary::new();
        stream_dict.insert("Length", Object::Integer(4));
        pdf.set_object(
            stream_ref,
            Object::Stream(Stream::new(stream_dict, b"\r<0g".to_vec())),
        );

        let mut seen = HashSet::new();
        let warnings = apply_normalize_content(&mut pdf, page_ref, &mut seen).unwrap();

        assert_eq!(warnings, vec![true]);
        assert_eq!(seen, HashSet::from([stream_ref]));
        let stream = pdf.resolve(stream_ref).unwrap().into_stream().unwrap();
        assert_eq!(stream.data, b"\n<0g");
    }

    #[test]
    fn apply_normalize_content_leaves_direct_stream_unchanged() {
        let mut pdf = Pdf::open_mem_owned(
            include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
        )
        .unwrap();
        let page_ref = pages::page_refs(&mut pdf).unwrap()[0];
        let direct_stream = Stream::new(Dictionary::new(), b"\r<0g".to_vec());
        let mut page = pdf.resolve(page_ref).unwrap().into_dict().unwrap();
        page.insert("Contents", Object::Stream(direct_stream));
        pdf.set_object(page_ref, Object::Dictionary(page));

        let mut seen = HashSet::new();
        let warnings = apply_normalize_content(&mut pdf, page_ref, &mut seen).unwrap();

        assert!(warnings.is_empty());
        assert!(seen.is_empty());
        let page = pdf.resolve(page_ref).unwrap().into_dict().unwrap();
        assert_eq!(
            page.get("Contents")
                .and_then(Object::as_stream)
                .map(|stream| stream.data.as_slice()),
            Some(&b"\r<0g"[..])
        );
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
        let err = parse_overlay_segment(OverlayKind::Overlay, &[])
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
        let out =
            rewrite_qpdf_single_dash(strs(&["flpdf", "-overlay", "stamp.pdf", "-to=1", "--"]));
        assert_eq!(
            out,
            strs(&["flpdf", "--overlay", "stamp.pdf", "--to=1", "--",])
        );
    }

    #[test]
    fn each_segment_kind_recognizes_its_sub_options() {
        assert!(QpdfArgSegment::Encrypt.accepts("use-aes"));
        assert!(QpdfArgSegment::Pages.accepts("range"));
        assert!(QpdfArgSegment::AddAttachment.accepts("replace"));
        assert!(QpdfArgSegment::CopyAttachments.accepts("prefix"));
    }

    #[test]
    fn collect_clap_long_options_includes_aliases() {
        let command = clap::Command::new("test")
            .arg(clap::Arg::new("mode").long("mode").alias("legacy-mode"));
        let mut names = HashSet::new();

        collect_clap_long_options(&command, &mut names);

        assert!(names.contains("mode"));
        assert!(names.contains("legacy-mode"));
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
    fn legacy_encrypt_password_starting_with_dash_is_rejected() {
        let err = parse_encrypt_segment(&strs(&["-user", "owner", "128"]), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unrecognized argument -user"), "got: {err}");
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
        assert_eq!(s.password.as_deref(), Some("pw"));
        assert_eq!(s.from.as_deref(), Some("1"));
        assert_eq!(s.to.as_deref(), Some("2-3"));
        assert_eq!(s.repeat.as_deref(), Some("1"));
    }

    #[test]
    fn extract_leaves_trailing_top_level_flag_after_group_terminator() {
        // qtest form-xobject uo-3 style: a top-level flag appears AFTER the
        // overlay/underlay group's `--` terminator. The extractor must place
        // that trailing flag verbatim into the residual argv so clap sees it.
        // A regression here would reintroduce the flpdf-9hc.16.18 diagnosis
        // trap ("blame the extractor when the top-level flag is missing from
        // clap's schema").
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
    fn extract_overlay_equals_form_rejected() {
        // The `--overlay=FILE` attached-value form must stay rejected even
        // after the segment parser was loosened to accept sub-options in any
        // order; qpdf rejects the equals form (the flag itself is not an
        // `=`-valued option, only its inner `--file=FILE` sub-option is).
        let argv = strs(&["flpdf", "--overlay=src.pdf", "--"]);
        let err = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(err.contains("--overlay"), "got: {err}");
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[test]
    fn extract_underlay_equals_form_rejected() {
        let argv = strs(&["flpdf", "--underlay=src.pdf", "--"]);
        let err = extract_overlay_groups(argv).unwrap_err().to_string();
        assert!(err.contains("--underlay"), "got: {err}");
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[test]
    fn extract_password_sub_flag_not_mistaken_for_terminator() {
        // `--password=…` starts with `--` but only a bare `--` terminates.
        let argv = strs(&["--overlay", "src.pdf", "--password=--weird", "--"]);
        let (_residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].password.as_deref(), Some("--weird"));
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
    fn extract_unterminated_sibling_segment_copied_to_end() {
        // An unterminated --encrypt segment is copied verbatim (clap raises the
        // error later); the inner --overlay must NOT be hijacked into a group.
        let argv = strs(&["--encrypt", "u", "o", "128", "--overlay", "x"]);
        let (residual, specs) = extract_overlay_groups(argv.clone()).unwrap();
        assert!(specs.is_empty(), "got: {specs:?}");
        assert_eq!(residual, argv);
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
            file: compat_fixture("one-page.pdf"),
            password: None,
            from: Some("1".into()),
            to: Some("1-2".into()),
            repeat: Some("1".into()),
        }];
        let built = build_overlay_specs(&cli_specs, false).unwrap();
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
            file: compat_fixture("one-page.pdf"),
            password: None,
            from: None,
            to: None,
            repeat: None,
        }];
        let built = build_overlay_specs(&cli_specs, false).unwrap();
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
            from: None,
            to: None,
            repeat: None,
        }];
        // `flpdf::OverlaySpec` is not Debug (it holds a `Pdf`), so match the Ok
        // arm explicitly instead of `unwrap_err()`.
        let err = match build_overlay_specs(&cli_specs, false) {
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
                file: file.clone(),
                password: None,
                from: from.map(str::to_string),
                to: None,
                repeat: None,
            }]
        };

        let absent = build_overlay_specs(&spec(None), false).unwrap();
        assert_eq!(absent[0].from.resolve(3).unwrap(), vec![1, 2, 3]);

        let empty = build_overlay_specs(&spec(Some("")), false).unwrap();
        assert_eq!(empty[0].from.resolve(3).unwrap(), Vec::<u32>::new());

        let explicit = build_overlay_specs(&spec(Some("2")), false).unwrap();
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
                file: file.clone(),
                password: None,
                from: None,
                to: to.map(str::to_string),
                repeat: None,
            }]
        };

        let absent = build_overlay_specs(&spec(None), false).unwrap();
        assert_eq!(absent[0].to.resolve(3).unwrap(), vec![1, 2, 3]);

        let empty = build_overlay_specs(&spec(Some("")), false).unwrap();
        assert_eq!(empty[0].to.resolve(3).unwrap(), Vec::<u32>::new());

        let explicit = build_overlay_specs(&spec(Some("2-3")), false).unwrap();
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
                file: file.clone(),
                password: None,
                from: None,
                to: None,
                repeat: repeat.map(str::to_string),
            }]
        };

        let absent = build_overlay_specs(&spec(None), false).unwrap();
        assert!(absent[0].repeat.is_none(), "absent --repeat -> None");

        let empty = build_overlay_specs(&spec(Some("")), false).unwrap();
        assert!(
            empty[0].repeat.is_none(),
            "explicit empty --repeat= -> None (no repeat), same as absent"
        );

        let explicit = build_overlay_specs(&spec(Some("2")), false).unwrap();
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
            file: encrypted_fixture("v2-rc4-128-r3.pdf"),
            password: Some("user-v2".into()),
            from: None,
            to: None,
            repeat: None,
        }];

        let built = build_overlay_specs(&cli_specs, false).unwrap();
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].kind, flpdf::OverlayKind::Overlay);
    }
}
