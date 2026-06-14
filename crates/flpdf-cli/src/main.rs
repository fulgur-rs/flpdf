#![forbid(unsafe_code)]

use clap::{ArgGroup, Args as ClapArgs, Parser, Subcommand, ValueEnum};
use flpdf::filespec_helper::ascii_filename_fallback;
use flpdf::{
    acroform_field_prune::prune_acroform_after_subset,
    objr_obj_annot_p::drop_objr_obj_annot_dangling_p, outline_dest_remap::remap_outline_and_dests,
    page_collate::collate, page_combine::CombinedPlan, page_rotate::apply_rotate_to_pages,
    page_split::split_pages, page_tree_rebuild::rebuild_page_tree,
    struct_tree_pg::drop_struct_elem_dangling_pg, subset_prune::prune_after_subset,
    thread_bead_p::drop_thread_bead_dangling_p, InputSpec, PageRange, RotateSpec,
};
use flpdf::{
    acroform_sig_flags, clear_sig_flags, strip_signature_values, SIG_FLAGS_APPEND_ONLY,
    SIG_FLAGS_SIGNATURES_EXIST,
};
use flpdf::{
    check_reader_with_options_and_limits, enumerate_document_annotations, filters,
    flatten_annotations, flatten_rotation_on_pages, fonts, generate_button_field_appearance,
    generate_choice_field_appearance, generate_text_field_appearance,
    json_inspect::{
        build_qpdf_json_v2_with_options, filter_json_keys, filter_json_objects,
        format_json_side_file_path, stream_payload_for_decode_level, DecodeLevel, JsonKey,
        JsonObjectSelector, StreamDataMode as JsonStreamDataMode,
    },
    linearization::{
        check_linearization_path, write_linearized, LinearizationCheckError, LinearizationPlan,
        RenumberMap,
    },
    normalize_content_stream, outline, pages,
    pages::coalesce_page_contents,
    parse_pdf_version,
    resources::remove_unreferenced_resources,
    write_pdf_with_options, AnnotationObjectHelper, CompressStreams, CopyEncryptionSource,
    Dictionary, EncryptMethod, EncryptParams, FlattenMode, FormFieldObjectHelper,
    NewlineBeforeEndstream, Object, ObjectKeyAlg, ObjectRef, ObjectStreamMode, PasswordMode, Pdf,
    PdfOpenOptions, PermissionsConfig, PrintPermission, RemoveUnreferencedResources, Severity,
    Stream, StreamDataMode, WriteOptions,
};
use flpdf::{
    copy_attachments_from, extract_attachment, fix_qdf, format_attachment_list,
    insert_embedded_file, list_attachment_info, remove_attachment, FileParamDates, FileSpecBuilder,
};
use std::fs::File;
use std::io::{BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

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
// error instead of discarding the second operation. (`--verbose` and
// `-o/--show-attachment-to` are sub-modifiers, not operations, so they are
// intentionally NOT members of this group.)
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
    /// Bound each page content stream's decoded output to BYTES during `--check`
    /// (opt-in decompression-bomb guard). Absent = unlimited (qpdf's default).
    /// A stream exceeding the cap is a warning (exit 3), not an error. Only
    /// meaningful with `--check`, so it `requires` that flag rather than being
    /// silently ignored alongside other modes.
    #[arg(long = "decode-memory-limit", value_name = "BYTES", requires = "check")]
    decode_memory_limit: Option<usize>,
    #[arg(long)]
    dump_object: Option<String>,
    #[arg(long)]
    show_info: bool,
    #[arg(long)]
    show_catalog: bool,
    #[arg(long)]
    show_metadata: bool,
    #[arg(long)]
    show_outline: bool,
    #[arg(long)]
    show_fonts: bool,
    #[arg(long)]
    show_npages: bool,
    #[arg(long)]
    show_pages: bool,

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
              "dump_object",
              "show_info", "show_catalog", "show_metadata", "show_outline",
              "show_fonts", "show_npages", "show_pages", "output",
              "compress_streams", "linearize_pass1", "remove_restrictions",
              "decrypt", "encrypt", "copy_encryption_from",
              "add_attachment", "remove_attachment", "list_attachments",
              "show_attachment", "copy_attachments_from",
              "no_original_object_ids", "qdf",
          ],
          help = "Generate JSON v2 output (qpdf --json compatible)")]
    json: Option<String>,

    /// Write JSON output to PATH instead of stdout.
    #[arg(
        long = "json-output",
        value_name = "PATH",
        requires = "json",
        help = "Write JSON to PATH instead of stdout"
    )]
    json_output: Option<PathBuf>,

    /// Limit JSON output to the specified top-level key (repeatable).
    /// Valid keys: acroform, attachments, encrypt, objectinfo, objects,
    /// outlines, pagelabels, pages, qpdf.
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
    /// Defaults to the --json-output path (if given) or \"stream\".
    #[arg(
        long = "json-stream-prefix",
        value_name = "PREFIX",
        requires = "json",
        help = "Prefix for side files with --json-stream-data=file"
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
    // inspection modes (--check / --dump-object / --show-*) before the
    // rewrite branch, so without these conflicts `flpdf --check
    // --remove-restrictions in out` would silently ignore the flag (and the
    // OUTPUT positional). Listing the inspection modes as clap conflicts
    // surfaces the mistake as a usage error instead. (--json already lists
    // remove_restrictions in its own conflicts_with_all; rewrite/linearize/
    // static_id/output are compatible rewrite modifiers and intentionally
    // excluded here.)
    #[arg(long = "remove-restrictions",
          conflicts_with_all = [
              "check", "dump_object", "show_info", "show_catalog",
              "show_metadata", "show_outline", "show_fonts",
              "show_npages", "show_pages",
          ])]
    remove_restrictions: bool,
    /// Strip the `/Encrypt` dictionary from the output (top-level alias of
    /// `flpdf rewrite --decrypt`; qpdf `--decrypt` equivalent). On
    /// encrypted input requires `--password` to authenticate; on plaintext
    /// input it is a no-op pass-through. Silent in both cases (matching
    /// qpdf), unlike `--remove-restrictions` which prints a one-line
    /// diagnostic when an encrypted input was de-restricted.
    ///
    /// Relationship with `--remove-restrictions`: on the current rewrite
    /// path the two flags produce IDENTICAL output bytes because flpdf
    /// always strips `/Encrypt` (and `/P` only exists inside `/Encrypt`).
    /// The flags differ only in intent and diagnostic. They become
    /// distinguishable once `--encrypt` lands and the
    /// rewrite gains the ability to preserve or produce encryption.
    // Same conflict semantics as --remove-restrictions: this is a
    // rewrite-path modifier and must be rejected against the inspection
    // subcommands so `flpdf --check --decrypt in out` is a usage error
    // rather than silently ignoring the flag (and OUTPUT).
    #[arg(long = "decrypt",
          conflicts_with_all = [
              "check", "dump_object", "show_info", "show_catalog",
              "show_metadata", "show_outline", "show_fonts",
              "show_npages", "show_pages",
          ])]
    decrypt: bool,
    /// `qpdf --compress-streams=y|n` compatibility flag.  Accepted but
    /// currently a no-op: flpdf does not re-encode stream contents on
    /// rewrite.  Provided so qtest commands parse cleanly.
    #[arg(long = "compress-streams")]
    compress_streams: Option<String>,
    /// `qpdf --linearize-pass1=PATH` compatibility flag.  Accepted; flpdf
    /// writes the pass-1 intermediate file as a copy of the final
    /// linearized output (qpdf writes a distinct intermediate; matching
    /// those bytes is out of scope here).
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
    ///           [--afrelationship=R] [--replace] --`
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

    /// Print verbose listing for --list-attachments (mirrors qpdf --verbose).
    #[arg(
        long = "verbose",
        requires = "list_attachments",
        help = "Print verbose listing when used with --list-attachments"
    )]
    verbose: bool,

    /// Extract an attachment by key (qpdf --show-attachment compatible).
    ///
    /// KEY is the name-tree key used when the attachment was added.  Without
    /// `-o` the raw bytes are written to stdout.
    #[arg(
        long = "show-attachment",
        value_name = "KEY",
        help = "Extract the embedded file with the given key to stdout or -o PATH \
                (qpdf --show-attachment)"
    )]
    show_attachment: Option<String>,

    /// Write --show-attachment output to this file instead of stdout.
    #[arg(
        short = 'o',
        long = "show-attachment-to",
        value_name = "PATH",
        requires = "show_attachment",
        help = "Write --show-attachment output to PATH instead of stdout"
    )]
    show_attachment_to: Option<PathBuf>,

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
    /// USER-PW / OWNER-PW are the two password strings; KEY-LEN selects
    /// the algorithm (`40` → V=1, `128` → V=2 or V=4, `256` → V=5 R=6).
    /// The writer currently only supports
    /// `KEY-LEN=128` together with `--use-aes=y` (= V=4 AES-128); the
    /// other algorithms have their dict builders shipped but no writer
    /// dispatch yet and are rejected with a clear "not yet supported"
    /// diagnostic. Permission sub-flags (`--print`, `--modify`, etc.) are
    /// likewise rejected for now; the default "all permissions granted"
    /// is used implicitly.
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
        // --linearize / --remove-restrictions / --decrypt overlaps with
        // --encrypt are rejected because they imply contradictory output
        // forms; --check / --dump-object / --show-* are inspection paths
        // that don't produce an output file at all.
        conflicts_with_all = [
            "check", "dump_object", "show_info", "show_catalog",
            "show_metadata", "show_outline", "show_fonts",
            "show_npages", "show_pages",
            "linearize", "remove_restrictions", "decrypt", "qdf",
            "copy_encryption_from",
        ],
        help = "Encrypt output (qpdf --encrypt compatible): \
                USER-PW OWNER-PW KEY-LEN [sub-flags] -- ; \
                walking skeleton supports 128 with --use-aes=y only"
    )]
    encrypt: Vec<String>,

    /// Copy the /Encrypt dictionary from a donor PDF and use its passwords for
    /// output encryption (qpdf --copy-encryption-from equivalent).
    ///
    /// Supply the donor's password via `--encryption-file-password` (empty
    /// string if the donor has no user password).  Only V=4 AES-128 donors are
    /// supported; other schemes are rejected
    /// with a "not yet supported" diagnostic.
    ///
    /// Mutually exclusive with `--encrypt`.
    #[arg(
        long = "copy-encryption-from",
        value_name = "FILE",
        conflicts_with_all = [
            "encrypt",
            "check", "dump_object", "show_info", "show_catalog",
            "show_metadata", "show_outline", "show_fonts",
            "show_npages", "show_pages",
            "linearize", "remove_restrictions", "decrypt", "qdf",
        ],
        help = "Copy /Encrypt from donor PDF (qpdf --copy-encryption-from); \
                pair with --encryption-file-password"
    )]
    copy_encryption_from: Option<PathBuf>,

    /// Password to open the donor PDF specified by `--copy-encryption-from`.
    ///
    /// Omit (or pass an empty string) if the donor has no user password.
    /// This is the *donor's* password, not the output file's password
    /// (the output inherits the donor's passwords exactly).
    #[arg(
        long = "encryption-file-password",
        value_name = "PW",
        requires = "copy_encryption_from",
        help = "User password to open the donor PDF for --copy-encryption-from"
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
    /// Cross-document merge (pages from more than one distinct source file) is
    /// out of scope for this layer — see the SCOPE comment in
    /// `run_page_extraction`.
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
    /// File names are derived by `page_split::split_output_path` (8.7's
    /// contract): a `-first-last` suffix is inserted before the `.pdf`
    /// extension. Compatible with `--pages`.
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

Divergences from qpdf, by design (flpdf-9hc.3.17): flpdf does not recover
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
    /// Bound each page content stream's decoded output to BYTES (opt-in
    /// decompression-bomb guard). Absent = unlimited. Exceeding the cap is a
    /// warning (exit 3), not an error.
    #[arg(long = "decode-memory-limit", value_name = "BYTES")]
    decode_memory_limit: Option<usize>,
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
    /// Emit unfiltered stored bytes instead of decoding.
    #[arg(long)]
    raw: bool,
    /// Write output to this file instead of stdout.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
    #[arg(long)]
    repair: bool,
    #[command(flatten)]
    password: PasswordArgs,
}

#[derive(Debug, ClapArgs)]
struct PagesCommand {
    input: PathBuf,
    #[arg(long)]
    count: bool,
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
    /// `--static-id` and ISO 32000-1 §14.4). Implies `--full-rewrite`. Mutually
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
    /// A plaintext `rewrite` already drops the /Encrypt dictionary, and the
    /// advisory permission bits live only inside /Encrypt /P, so the rewritten
    /// output is inherently unrestricted. This flag adds no new decryption
    /// behaviour beyond what plaintext rewrite already does: it makes the
    /// intent explicit and prints a one-line diagnostic when an encrypted
    /// (restricted) input was de-restricted. It does NOT bypass authentication
    /// — an auth-requiring input without a working --password is rejected
    /// exactly as a plain `rewrite` would reject it.
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
    /// Relationship with `--remove-restrictions`: on the current rewrite
    /// path the two flags produce identical output bytes because flpdf
    /// always drops `/Encrypt` and `/P` only lives inside `/Encrypt`. The
    /// flags differ only in intent and diagnostic (this one is silent).
    /// They become distinguishable once `--encrypt` lands.
    #[arg(long = "decrypt")]
    decrypt: bool,
    /// Encrypt the output (qpdf `--encrypt` compatible). See the top-level
    /// `--encrypt` documentation for the full syntax and the current
    /// restrictions (KEY-LEN=128 + --use-aes=y only, default permissions).
    #[arg(
        long = "encrypt",
        num_args = 3..,
        value_terminator = "--",
        allow_hyphen_values = true,
        value_name = "USER-PW OWNER-PW KEY-LEN [sub-flags]",
        conflicts_with_all = [
            "linearize", "remove_restrictions", "decrypt", "qdf",
            "copy_encryption_from",
        ]
    )]
    encrypt: Vec<String>,
    /// Copy the /Encrypt dictionary from a donor PDF and use its passwords for
    /// output encryption (qpdf --copy-encryption-from equivalent).
    ///
    /// Supply the donor's password via `--encryption-file-password` (empty
    /// string if the donor has no user password).  Only V=4 AES-128 donors are
    /// supported; other schemes are rejected
    /// with a "not yet supported" diagnostic.
    ///
    /// Mutually exclusive with `--encrypt`.
    #[arg(
        long = "copy-encryption-from",
        value_name = "FILE",
        conflicts_with_all = [
            "encrypt",
            "linearize", "remove_restrictions", "decrypt", "qdf",
        ],
        help = "Copy /Encrypt from donor PDF (qpdf --copy-encryption-from); \
                pair with --encryption-file-password"
    )]
    copy_encryption_from: Option<PathBuf>,
    /// Password to open the donor PDF specified by `--copy-encryption-from`.
    ///
    /// Omit (or pass an empty string) if the donor has no user password.
    #[arg(
        long = "encryption-file-password",
        value_name = "PW",
        requires = "copy_encryption_from",
        help = "User password to open the donor PDF for --copy-encryption-from"
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
    /// Decode every stream through its filter chain and re-emit the document
    /// end-to-end with a single /FlateDecode filter per stream.  The output
    /// contains no /Prev chain.  Cannot be combined with --linearize.
    #[arg(long = "full-rewrite")]
    full_rewrite: bool,
    /// Create a PDF in QDF form: uncompressed, normalized,
    /// human-readable/editable; pair with the qdf-fix subcommand after manual
    /// edits (qpdf --qdf equivalent).
    ///
    /// Implies --full-rewrite (the QDF code path lives in the full-rewrite
    /// writer) and forces object streams off. Cannot be combined with
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
    /// Only applies to the full-rewrite path; the incremental write path
    /// ignores this flag.
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
    #[arg(long = "normalize-content", value_enum, default_value_t = CliYesNo::No,
          help = "Normalize page content streams (qpdf default: n)")]
    normalize_content: CliYesNo,

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
    /// (qpdf --newline-before-endstream=y|n).
    ///
    /// `y` (default): always write exactly one `\n` before `endstream`, matching
    /// ISO 32000-1 §7.3.8.1 and qpdf's default behaviour.
    /// `n`: omit the extra newline when the stream payload already ends with `\n`
    /// or `\r`.
    ///
    /// Only affects the full-rewrite path.
    #[arg(long = "newline-before-endstream", value_enum, default_value_t = CliYesNo::Yes,
          help = "Insert newline before endstream keyword (qpdf default: y)")]
    newline_before_endstream: CliYesNo,

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
    /// - `screen`: only annotations that render on screen (not when printed).
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
    /// Form fields whose widgets have no `/AP` `/N` appearance are rendered
    /// from their current value (`/V`) and default appearance (`/DA`). Useful
    /// before `--flatten-annotations` so value-only fields are not dropped.
    /// Requires a full rewrite of the document.
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
    // REPEATABLE and each group is terminated by a bare `--`:
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
    ///          [--repeat=R] --`. Pages are stacked in order of appearance:
    /// first underlays, then the original page, then overlays.
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
    ///          [--repeat=R] --`. Pages are stacked in order of appearance:
    /// first underlays, then the original page, then overlays.
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
/// - `screen`: annotations that render on screen but not when printed.
/// - `print`: annotations flagged for printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliFlattenMode {
    All,
    Screen,
    Print,
}

impl From<CliFlattenMode> for FlattenMode {
    fn from(v: CliFlattenMode) -> Self {
        match v {
            CliFlattenMode::All => FlattenMode::All,
            CliFlattenMode::Screen => FlattenMode::Screen,
            CliFlattenMode::Print => FlattenMode::Print,
        }
    }
}

/// y|n toggle used by --compress-streams, --normalize-content, --newline-before-endstream.
/// Clap variant names are `y` and `n` (lowercase single letter, qpdf-compatible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliYesNo {
    #[clap(name = "y")]
    Yes,
    #[clap(name = "n")]
    No,
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
    // Extract the `--overlay`/`--underlay` groups from the raw argv before clap
    // parses (see `extract_overlay_groups`): clap's derive would flatten the
    // repeated occurrences and lose the per-group boundaries and declaration
    // order that byte-identical composition relies on. The residual argv (with
    // those groups removed) is what clap sees.
    let (residual_args, overlay_specs) = match extract_overlay_groups(std::env::args().collect()) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("flpdf: {error}");
            std::process::exit(2);
        }
    };
    let args = Cli::parse_from(residual_args);

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
    // inherently non-linearized; mirror the `rewrite --full-rewrite
    // --linearize` rejection. (The `Commands::Rewrite` arm performs the
    // equivalent check for the subcommand form.)
    if args.qdf && args.linearize {
        eprintln!("flpdf: --qdf and --linearize cannot be used together");
        std::process::exit(1);
    }

    // Top-level `--qdf` combined with a page operation is rejected here, before
    // the dispatch chain, for the same reason: the `else if
    // page_ops_active(...)` branch (below) wins over the default rewrite
    // branch and does not thread `--qdf` into its `WriteOptions`, so the
    // combination would silently emit a non-QDF document. The page-extraction
    // pipeline produces a normalized (non-QDF) doc by design; reject the
    // combination explicitly rather than ignoring the flag.
    if args.qdf && page_ops_active(&args.page_ops) {
        eprintln!("flpdf: --qdf cannot be combined with --pages/--rotate/--split-pages");
        std::process::exit(1);
    }

    // JSON mode takes the first branch, but this is unambiguous: clap's
    // conflicts_with_all on --json (plus args_conflicts_with_subcommands on
    // Cli) guarantees no other top-level mode or subcommand can be set at
    // the same time, so the ordering here is a formality, not a precedence
    // rule that could silently shadow another requested mode.
    let result = if args.json.is_some() {
        run_json(&args)
    } else if let Some(command) = args.command {
        run_command(command, &overlay_specs)
    } else if let Some(object_ref) = args.dump_object.as_deref() {
        run_dump_object(args.input, args.repair, &args.password, object_ref)
    } else if args.show_info {
        run_show_info(args.input, args.repair, &args.password)
    } else if args.show_catalog {
        run_show_catalog(args.input, args.repair, &args.password)
    } else if args.show_metadata {
        run_show_metadata(args.input, args.repair, &args.password)
    } else if args.show_outline {
        run_show_outline(args.input, args.repair, &args.password)
    } else if args.show_fonts {
        run_show_fonts(args.input, args.repair, &args.password)
    } else if args.show_npages {
        run_show_npages(args.input, args.repair, &args.password)
    } else if args.show_pages {
        run_show_pages(args.input, args.repair, &args.password)
    } else if args.check {
        run_check(
            args.input,
            args.repair,
            &args.password,
            filters::DecodeLimits {
                max_output: args.decode_memory_limit,
            },
        )
    } else if args.list_attachments {
        run_list_attachments(args.input, args.repair, &args.password, args.verbose)
    } else if let Some(key) = args.show_attachment {
        run_show_attachment(
            args.input,
            args.repair,
            &args.password,
            &key,
            args.show_attachment_to,
        )
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
        )
    } else if !args.copy_attachments_from.is_empty() {
        run_copy_attachments_from(
            args.input,
            args.output,
            args.repair,
            &args.password,
            args.copy_attachments_from,
            args.deterministic_id,
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
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
        options.deterministic_id = args.deterministic_id;
        options.static_aes_iv = args.static_aes_iv;
        options.no_original_object_ids = args.no_original_object_ids;
        // Top-level --compress-streams=y|n: parse and wire to WriteOptions.
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
        let result = run_rewrite(
            args.input,
            args.output.clone(),
            args.repair,
            &args.password,
            true,
            args.remove_restrictions,
            args.decrypt,
            false,                              // normalize_content
            false,                              // coalesce_contents
            CliRemoveUnreferencedResources::No, // remove_unreferenced (no-op for linearize path)
            false,                              // generate_appearances (not on top-level surface)
            None,                               // flatten_annotations (not on top-level surface)
            false,                              // flatten_rotation (not on top-level surface)
            &overlay_specs,
            options,
        );
        if result.is_ok() {
            if let (Some(pass1), Some(output)) =
                (args.linearize_pass1.as_ref(), args.output.as_ref())
            {
                // qpdf --linearize-pass1=PATH dumps the pre-back-patched pass1
                // intermediate. flpdf does not currently expose that internal
                // state; copy the final output instead so the path exists and
                // downstream byte-equality checks fail meaningfully rather
                // than the file being absent.
                if let Err(error) = std::fs::copy(output, pass1) {
                    eprintln!("flpdf: failed to write --linearize-pass1 file: {error}");
                    std::process::exit(2);
                }
            }
        }
        result
    } else if page_ops_active(&args.page_ops) {
        // Top-level page-operation path (qpdf-shaped invocation:
        // `flpdf in.pdf --pages . 1-3 -- out.pdf`). Mirrors the `rewrite`
        // subcommand's page-op dispatch below.
        //
        // The page-op pipeline does not thread `WriteOptions.encrypt`
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
        if args.copy_encryption_from.is_some() {
            eprintln!(
                "flpdf: --copy-encryption-from is not applied in the \
                 --pages/--rotate/--split-pages/--collate pipeline; \
                 rerun without --copy-encryption-from or without the page operation"
            );
            std::process::exit(1);
        }
        // The page-op pipeline owns the write through a separate path that does
        // not run the overlay/underlay page-stacking step; reject the
        // combination rather than silently dropping the overlays.
        if !overlay_specs.is_empty() {
            eprintln!(
                "flpdf: --overlay/--underlay is not applied in the \
                 --pages/--rotate/--split-pages/--collate pipeline; \
                 rerun without the page operation"
            );
            std::process::exit(1);
        }
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
        options.deterministic_id = args.deterministic_id;
        options.static_aes_iv = args.static_aes_iv;
        options.no_original_object_ids = args.no_original_object_ids;
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
                    &args.page_ops,
                    CliRemoveUnreferencedResources::Auto,
                    options.clone(),
                )
            } else {
                run_rewrite_with_page_ops(
                    &input,
                    &output,
                    args.repair,
                    &args.password,
                    &args.page_ops,
                    options.clone(),
                )
            }
        };
        match (args.input.clone(), args.output.clone()) {
            (Some(i), Some(o)) => dispatch(i, o),
            _ => Err("page operations require both an input and an output file".into()),
        }
    } else {
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
        options.deterministic_id = args.deterministic_id;
        options.static_aes_iv = args.static_aes_iv;
        options.no_original_object_ids = args.no_original_object_ids;
        // Top-level `--qdf` is an alias of `rewrite --qdf`. The QDF code path
        // lives in the full-rewrite writer, so --qdf must imply
        // full_rewrite=true regardless of any --full-rewrite flag (the
        // top-level surface has none). The library forces ObjStm off under
        // qdf; --object-streams is not accepted on the top-level surface, so
        // no conflict diagnostic is needed here.
        options.qdf = args.qdf;
        if args.qdf {
            options.full_rewrite = true;
        }
        if args.deterministic_id {
            // The deterministic /ID is an MD5 over the rewritten body, which
            // only the full-rewrite writer produces; imply it like --qdf does.
            options.full_rewrite = true;
        }
        // Top-level --compress-streams=y|n: parse and wire to WriteOptions.
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
        // Top-level --encrypt / --copy-encryption-from: wire encryption onto
        // WriteOptions (shared with the `rewrite` surface via
        // apply_encryption_options). Both force full_rewrite because the
        // incremental writer cannot run an encryption pass; parse / donor-open
        // errors exit 2. The page-op pipeline does not thread either option, so
        // the `else if page_ops_active` arm above already rejects them; this is
        // the non-page-op branch, so no further page-op guard is needed here.
        apply_encryption_options(
            &mut options,
            &args.encrypt,
            args.copy_encryption_from.as_deref(),
            args.encryption_file_password.as_deref(),
            args.password.allow_weak_crypto,
        );
        run_rewrite(
            args.input,
            args.output,
            args.repair,
            &args.password,
            false,
            args.remove_restrictions,
            args.decrypt,
            false,                              // normalize_content
            false,                              // coalesce_contents
            CliRemoveUnreferencedResources::No, // remove_unreferenced (top-level alias is no-op)
            false,                              // generate_appearances (not on top-level surface)
            None,                               // flatten_annotations (not on top-level surface)
            false,                              // flatten_rotation (not on top-level surface)
            &overlay_specs,
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
                eprintln!("{}: {}", progname(), exit_err.message);
            }
            std::process::exit(exit_err.code.as_i32());
        }
        // Signed-PDF refusal (default full-rewrite path): emit the actionable
        // diagnostic directly. `Error::Signed`'s own message already explains
        // the refusal and names the affected signature field(s), so printing
        // the `message` field avoids the redundant "signed PDF:" Display prefix
        // (`flpdf: signed PDF: refusing full rewrite of signed PDF ...`) that
        // the generic fallback below would produce. Exit 2 matches qpdf's
        // "error" convention (same code the fallback uses).
        if let Some(flpdf::Error::Signed { message, .. }) = error.downcast_ref::<flpdf::Error>() {
            eprintln!("{}: {message}", progname());
            std::process::exit(2);
        }
        eprintln!("{}: {error}", progname());
        std::process::exit(2);
    }
}

fn run_json(cli: &Cli) -> CliResult<()> {
    // 1. Validate --json-key values before doing any I/O.
    let mut json_keys: Vec<JsonKey> = Vec::new();
    for raw in &cli.json_key {
        match JsonKey::from_str(raw.as_str()) {
            Some(k) => json_keys.push(k),
            None => {
                let names = JsonKey::ALL_NAMES.join(",");
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
    let stream_data_raw = cli.json_stream_data.as_deref().unwrap_or("none");

    let prefix_default = || -> String {
        cli.json_stream_prefix
            .clone()
            .or_else(|| {
                cli.json_output
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "stream".to_string())
    };

    let stream_mode = match stream_data_raw {
        "none" => JsonStreamDataMode::None,
        "inline" => JsonStreamDataMode::Inline,
        "file" => JsonStreamDataMode::File {
            prefix: prefix_default(),
        },
        other => {
            eprintln!("flpdf: --json-stream-data must be none, inline, or file; got: {other}");
            std::process::exit(2);
        }
    };

    // 4. Open PDF.
    let input = cli.input.as_ref().ok_or("missing input file")?;
    let mut pdf = open_pdf(input, cli.repair, &cli.password)?;

    // 5. Build JSON.
    //
    // `decode_level` governs both the inline `data` payloads (applied inside
    // build_qpdf_json_v2_with_options) and the file-mode side files written in
    // step 9 below — the two must agree, so they share this single value.
    let decode_level = DecodeLevel::Generalized;
    let mut v2 = build_qpdf_json_v2_with_options(&mut pdf, decode_level, &stream_mode)
        .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;

    // 6. Apply --json-key filter.
    if !json_keys.is_empty() {
        v2 = filter_json_keys(v2, &json_keys);
    }

    // 7. Apply --json-object filter.
    if !json_objects.is_empty() {
        v2 = filter_json_objects(v2, &json_objects);
    }

    // 8. Write JSON to output destination.
    if let Some(ref out_path) = cli.json_output {
        let mut file = File::create(out_path)?;
        flpdf::json::write(&v2, &mut file)?;
    } else {
        let stdout = std::io::stdout();
        let mut locked = stdout.lock();
        flpdf::json::write(&v2, &mut locked)?;
        locked.flush()?;
    }

    // 9. Write side files for stream-data=file mode — only for streams
    // that actually survived --json-key / --json-object filtering. Walk
    // the final JSON and collect every "datafile" value emitted in the
    // qpdf objects map, then write exactly those streams to disk. Without
    // this scoping, --json-key=pages --json-stream-data=file would dump
    // every stream to the filesystem even though the qpdf section was
    // filtered out of the output.
    if let JsonStreamDataMode::File { ref prefix } = stream_mode {
        let wanted_refs = collect_datafile_object_refs(&v2);
        // Reuse the same `pdf` handle the JSON was built from. Re-opening
        // the input here would risk the file being swapped mid-run, so the
        // JSON body and the side files could capture different snapshots.
        for oref in wanted_refs {
            let obj = pdf.resolve_borrowed(oref)?;
            if let Object::Stream(stream) = obj {
                // Side-file name must match the JSON `datafile` value;
                // both come from the same helper to avoid divergence.
                let side_path = format_json_side_file_path(prefix, oref.number);
                // Apply the same DecodeLevel the JSON body was built with so
                // the side file matches what inline mode would emit.
                let payload = stream_payload_for_decode_level(stream, decode_level);
                std::fs::write(&side_path, &*payload)?;
            }
        }
    }

    Ok(())
}

/// Walk the qpdf JSON v2 output and collect every `(obj_num, generation)`
/// whose stream entry carries a `datafile` field. Used to scope side-file
/// writes to exactly the streams that survived --json-key/--json-object
/// filtering, so the CLI never writes streams the JSON output doesn't
/// reference.
fn collect_datafile_object_refs(v2: &flpdf::json::JsonValue) -> Vec<ObjectRef> {
    use flpdf::json::JsonValue;
    let mut out = Vec::new();
    let JsonValue::Object(top) = v2 else {
        return out;
    };
    let Some((_, qpdf_value)) = top.iter().find(|(k, _)| k == "qpdf") else {
        return out;
    };
    let JsonValue::Array(qpdf_arr) = qpdf_value else {
        return out;
    };
    // Expected shape: [metadata, objects_map].
    let Some(JsonValue::Object(objects_map)) = qpdf_arr.get(1) else {
        return out;
    };
    for (key, entry) in objects_map {
        // Skip the "trailer" entry — it has no object number.
        let Some(rest) = key.strip_prefix("obj:") else {
            continue;
        };
        let rest = rest.trim_end_matches(" R");
        let mut parts = rest.split_whitespace();
        let Some(num) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(gen) = parts.next().and_then(|s| s.parse::<u16>().ok()) else {
            continue;
        };
        // Only collect if the stream entry has a datafile field.
        let JsonValue::Object(entry_pairs) = entry else {
            continue;
        };
        let Some((_, stream_val)) = entry_pairs.iter().find(|(k, _)| k == "stream") else {
            continue;
        };
        let JsonValue::Object(stream_pairs) = stream_val else {
            continue;
        };
        if stream_pairs.iter().any(|(k, _)| k == "datafile") {
            out.push(ObjectRef::new(num, gen));
        }
    }
    out
}

fn run_command(command: Commands, overlay_specs: &[OverlaySpec]) -> CliResult<()> {
    match command {
        Commands::Check(cmd) => run_check(
            Some(cmd.input),
            cmd.repair,
            &cmd.password,
            filters::DecodeLimits {
                max_output: cmd.decode_memory_limit,
            },
        ),
        Commands::CheckLinearization(cmd) => match check_linearization_path(&cmd.input) {
            Ok(()) => {
                println!("linearization OK");
                Ok(())
            }
            Err(LinearizationCheckError::NotLinearized) => {
                eprintln!(
                    "flpdf: not a linearized PDF: the first object in the file has no /Linearized key"
                );
                std::process::exit(1);
            }
            Err(LinearizationCheckError::InvalidParam { message }) => {
                eprintln!("flpdf: linearization check failed: {message}");
                std::process::exit(1);
            }
            Err(LinearizationCheckError::Io(e)) => Err(e.to_string().into()),
        },
        Commands::DumpObject(cmd) => {
            run_dump_object(Some(cmd.input), cmd.repair, &cmd.password, &cmd.object_ref)
        }
        Commands::Pages(cmd) => {
            if cmd.count {
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
            if cmd.full_rewrite && cmd.linearize {
                eprintln!("flpdf: --full-rewrite and --linearize cannot be used together");
                std::process::exit(1);
            }
            // QDF is inherently non-linearized; reject the combination with a
            // fatal diagnostic, mirroring the --full-rewrite/--linearize
            // rejection above. (The top-level `--qdf --linearize` form is
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
            let mut options = WriteOptions::default();
            options.static_id = cmd.static_id;
            options.deterministic_id = cmd.deterministic_id;
            options.static_aes_iv = cmd.static_aes_iv;
            options.min_version = cmd.min_version;
            options.force_version = cmd.force_version;
            options.no_original_object_ids = cmd.no_original_object_ids;
            // `--qdf` enables QDF and, because the QDF code path lives in the
            // full-rewrite writer, forces full_rewrite=true regardless of
            // whether --full-rewrite was passed. `--deterministic-id` likewise
            // needs the full-rewrite body to hash, so it implies full_rewrite.
            options.qdf = cmd.qdf;
            options.full_rewrite = cmd.full_rewrite || cmd.qdf || cmd.deterministic_id;
            options.object_streams = cmd.object_streams.into();
            options.compress_streams = match cmd.compress_streams {
                CliYesNo::Yes => CompressStreams::Yes,
                CliYesNo::No => CompressStreams::No,
            };
            options.newline_before_endstream = match cmd.newline_before_endstream {
                CliYesNo::Yes => NewlineBeforeEndstream::Yes,
                CliYesNo::No => NewlineBeforeEndstream::No,
            };
            // --stream-data overrides --compress-streams when set. The
            // policy is only applied by the full-rewrite path; without this
            // promotion the flag would be silently dropped on invocations
            // that would otherwise take the incremental path (e.g. with
            // --remove-unreferenced-resources=no on unencrypted input).
            // Mirrors the same auto-promotion done for --qdf, --min-version,
            // --force-version, and the content-mutation flags below.
            options.stream_data = cmd.stream_data.map(Into::into);
            if options.stream_data.is_some() {
                options.full_rewrite = true;
            }
            // --recompress-flate only has an effect in the full-rewrite writer
            // (it re-encodes lone /FlateDecode streams there). Promote to a full
            // rewrite so the flag is not silently dropped on invocations that
            // would otherwise take the incremental path (e.g. with
            // --remove-unreferenced-resources=no on unencrypted input), matching
            // the --stream-data promotion above.
            options.recompress_flate = cmd.recompress_flate;
            if cmd.recompress_flate {
                options.full_rewrite = true;
            }
            // `rewrite --encrypt` / `--copy-encryption-from`: wire encryption
            // onto WriteOptions (shared with the top-level surface via
            // apply_encryption_options). Both force full_rewrite — the
            // incremental write path cannot run an encryption pass.
            apply_encryption_options(
                &mut options,
                &cmd.encrypt,
                cmd.copy_encryption_from.as_deref(),
                cmd.encryption_file_password.as_deref(),
                cmd.password.allow_weak_crypto,
            );
            let normalize_content = cmd.normalize_content == CliYesNo::Yes;
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
                // The page-op pipeline owns the write through a separate path
                // that does not run the overlay/underlay page-stacking step;
                // reject the combination rather than silently dropping it.
                if !overlay_specs.is_empty() {
                    eprintln!(
                        "flpdf: --overlay/--underlay is not applied in the \
                         --pages/--rotate/--split-pages/--collate pipeline; \
                         rerun without the page operation"
                    );
                    std::process::exit(1);
                }
                // The page-operation pipeline owns the write and does not run
                // the rewrite-only passes. Silently dropping them would make
                // `rewrite --rotate=90 --normalize-content=y ...` partially
                // succeed; reject the unsupported combination loudly instead.
                //
                // --decrypt is rejected for the same reason: the page-ops
                // pipeline already rejects encrypted inputs (so a useful
                // --decrypt + page-ops combination is impossible), and on
                // plaintext input --decrypt is a silent no-op anyway —
                // rejecting upfront surfaces the unsupported combination
                // instead of leaving the user wondering whether decryption
                // happened.
                if normalize_content
                    || coalesce_contents
                    || cmd.remove_restrictions
                    || cmd.decrypt
                    || !cmd.encrypt.is_empty()
                    || cmd.copy_encryption_from.is_some()
                    || cmd.generate_appearances
                    || cmd.flatten_annotations.is_some()
                    || cmd.flatten_rotation
                {
                    eprintln!(
                        "flpdf: --normalize-content / --coalesce-contents / \
                         --remove-restrictions / --decrypt / --encrypt / \
                         --copy-encryption-from / --flatten-annotations / \
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
                        &cmd.page_ops,
                        remove_unref,
                        options,
                    )
                } else {
                    run_rewrite_with_page_ops(
                        &cmd.input,
                        &cmd.output,
                        cmd.repair,
                        &cmd.password,
                        &cmd.page_ops,
                        options,
                    )
                };
            }

            run_rewrite(
                Some(cmd.input),
                Some(cmd.output),
                cmd.repair,
                &cmd.password,
                cmd.linearize,
                cmd.remove_restrictions,
                cmd.decrypt,
                normalize_content,
                coalesce_contents,
                remove_unref,
                cmd.generate_appearances,
                cmd.flatten_annotations,
                cmd.flatten_rotation,
                overlay_specs,
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
    let report = check_reader_with_options_and_limits(BufReader::new(file), options, decode_limits)
        .map_err(|error| error_with_file(&input, actionable_password_error(error)))?;
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
        let location = diagnostic_location(&input, diagnostic.offset);
        match diagnostic.severity {
            Severity::Warning => eprintln!("WARNING: {location}: {}", diagnostic.message),
            Severity::Error => {
                eprintln!("{}: {location}: {}", progname(), diagnostic.message)
            }
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
        print_check_block(&input, summary);
    }

    if has_warnings {
        // Warnings without errors — exit 3. qpdf still prints the block above,
        // but omits the trailing "No syntax ..." note. Pass an empty message so
        // main() does not emit a redundant "flpdf: ..." line.
        // qpdf 11.9.0 ends the warning-bearing run with this stderr summary.
        eprintln!("{}: operation succeeded with warnings", progname());
        return Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        }));
    }

    // Clean — exit 0. qpdf closes a clean check with this two-line note; the
    // subject mirrors progname() so it is byte-identical under FLPDF_PROGNAME=qpdf.
    println!("No syntax or stream encoding errors found; the file may still contain");
    println!("errors that {} cannot detect", progname());
    Ok(())
}

/// Print qpdf's `--check` document summary block to stdout.
///
/// Mirrors qpdf 11.9.0's stdout for a successfully-opened document: the
/// `checking <file>` banner, header version, encryption status and
/// linearization status. `<file>` is echoed verbatim as supplied on the command
/// line (qpdf prints the argument, not a canonicalised path).
fn print_check_block(input: &Path, summary: &flpdf::CheckSummary) {
    println!("checking {}", input.display());
    // qpdf appends "extension level N" to the version when the catalog declares
    // an Adobe extension level (`/Extensions /ADBE /ExtensionLevel`).
    match summary.extension_level {
        Some(level) => println!("PDF Version: {} extension level {level}", summary.version),
        None => println!("PDF Version: {}", summary.version),
    }
    // Interim: encrypted files emit a single line. The detailed qpdf
    // `R = / P = / permission / method` block is tracked in flpdf-oox1.
    println!(
        "{}",
        if summary.encrypted {
            "File is encrypted"
        } else {
            "File is not encrypted"
        }
    );
    // The linearization status reflects the structural detector (object (1,0)
    // only). qpdf-accurate detection — plus the entangled warning / exit-code /
    // trailing-line behaviour — is tracked in flpdf-u1ro.
    println!(
        "{}",
        if summary.linearized {
            "File is linearized"
        } else {
            "File is not linearized"
        }
    );
}

/// Wire `--encrypt` / `--copy-encryption-from` onto `options`, shared by the
/// top-level and `rewrite` surfaces so the two stay in lock-step. Both options
/// force `full_rewrite` because the incremental write path cannot run an
/// encryption pass. A `--encrypt` parse error or a `--copy-encryption-from`
/// donor-open/validation error prints a `flpdf:`-prefixed diagnostic and exits
/// 2, matching the surrounding option parsers. The two options are mutually
/// exclusive at the CLI layer (clap `conflicts_with`), so at most one branch
/// fires.
fn apply_encryption_options(
    options: &mut WriteOptions,
    encrypt: &[String],
    copy_encryption_from: Option<&std::path::Path>,
    encryption_file_password: Option<&str>,
    allow_weak_crypto: bool,
) {
    if !encrypt.is_empty() {
        match parse_encrypt_segment(encrypt, allow_weak_crypto) {
            Ok(params) => {
                options.encrypt = Some(params);
                options.full_rewrite = true;
            }
            Err(e) => {
                eprintln!("flpdf: {e}");
                std::process::exit(2);
            }
        }
    }
    if let Some(donor_path) = copy_encryption_from {
        match build_copy_encryption_source(donor_path, encryption_file_password) {
            Ok(src) => {
                options.copy_encryption = Some(src);
                options.full_rewrite = true;
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
/// (`--copy-encryption-from`).
///
/// Returns a [`CopyEncryptionSource`] ready to be stored in
/// [`WriteOptions::copy_encryption`] or an error string suitable for printing
/// to stderr before `exit(2)`.
///
/// Only V=4 AES-128 donors are accepted.  Other encryption schemes are
/// rejected with a "not yet supported" message.
fn build_copy_encryption_source(
    path: &std::path::Path,
    password: Option<&str>,
) -> CliResult<CopyEncryptionSource> {
    let file = File::open(path)
        .map_err(|e| format!("--copy-encryption-from: cannot open {:?}: {e}", path))?;
    let reader = BufReader::new(file);

    let pw_bytes: Vec<u8> = password.unwrap_or("").as_bytes().to_vec();
    let opts = PdfOpenOptions {
        password: pw_bytes,
        repair: true,
        ..PdfOpenOptions::default()
    };
    let mut donor = Pdf::open_with_options(reader, opts)
        .map_err(|e| format!("--copy-encryption-from: failed to open {:?}: {e}", path))?;

    // Validate the donor is encrypted.
    let info = donor
        .encryption_info()
        .map_err(|e| format!("--copy-encryption-from: failed to read encryption info: {e}"))?
        .ok_or_else(|| format!("--copy-encryption-from: donor {:?} is not encrypted", path))?;

    // Walking-skeleton scope: only V=4 AES-128 (StmF=AESV2 / StrF=AESV2).
    // Note: encryption_info uses qpdf_name() which returns "AESv2" (lowercase v).
    let is_v4_aes128 = info.v == 4
        && info.length_bits == 128
        && info.stream_method == "AESv2"
        && info.string_method == "AESv2";
    if !is_v4_aes128 {
        return Err(format!(
            "--copy-encryption-from: donor {:?} uses V={} length={} \
             stream={} string={} — not yet supported \
             (flpdf-9hc.4.9 follow-up; only V=4 AES-128 donors are accepted)",
            path, info.v, info.length_bits, info.stream_method, info.string_method,
        )
        .into());
    }

    // Recover the donor's file key.  The error message guides the user to
    // supply the correct password via --encryption-file-password.
    let file_key: Vec<u8> = donor
        .encryption_file_key()
        .ok_or_else(|| {
            format!(
                "--copy-encryption-from: failed to recover donor file key for {:?} \
                 (wrong --encryption-file-password?)",
                path
            )
        })?
        .to_vec();

    // Extract the /Encrypt ObjectRef from the donor trailer, then resolve it.
    // Pull the ref while holding the trailer borrow, then drop that borrow
    // before calling resolve() which needs &mut self.
    let encrypt_ref = donor.trailer().get_ref("Encrypt").ok_or_else(|| {
        format!(
            "--copy-encryption-from: donor {:?} has no /Encrypt in trailer",
            path
        )
    })?;

    let encrypt_obj = donor.resolve_borrowed(encrypt_ref).map_err(|e| {
        format!(
            "--copy-encryption-from: failed to resolve /Encrypt in {:?}: {e}",
            path
        )
    })?;

    let encrypt_dict = match encrypt_obj {
        Object::Dictionary(d) => d.clone(),
        other => {
            return Err(format!(
                "--copy-encryption-from: /Encrypt in {:?} is not a dictionary (got {:?})",
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
                return Err(format!(
                    "--copy-encryption-from: donor {:?} /ID[0] is not a string",
                    path
                )
                .into())
            }
        },
        _ => {
            return Err(format!(
                "--copy-encryption-from: donor {:?} has no /ID array in trailer",
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
                 permission grammar needs a 128- or 256-bit key (40-bit permission \
                 flags are flpdf-9hc.4.9.5 follow-up)"
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
    linearize: bool,
    remove_restrictions: bool,
    decrypt: bool,
    normalize_content: bool,
    coalesce_contents: bool,
    remove_unref: CliRemoveUnreferencedResources,
    generate_appearances: bool,
    flatten_annotations_mode: Option<CliFlattenMode>,
    flatten_rotation: bool,
    overlay_specs: &[OverlaySpec],
    options: WriteOptions,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;

    // Overlay/underlay stacking mutates page dictionaries and adds objects that
    // only surface on the full-rewrite path; the linearize path computes its
    // object plan up front (before any such mutation), so the combination is
    // rejected upfront. All overlay goldens use the plain full-rewrite path.
    if linearize && !overlay_specs.is_empty() {
        return Err("--overlay/--underlay cannot be combined with --linearize".into());
    }

    // SCOPE BOUNDARY (flpdf-9hc.3.18 vs flpdf-9hc.4.10):
    // `--remove-restrictions` and `--decrypt` both add NO new decryption
    // logic. A plaintext `rewrite` of an authenticated encrypted input
    // already drops the /Encrypt dictionary (crates/flpdf/src/writer.rs
    // trailer.remove / xref_dict.remove "Encrypt"), and the advisory
    // permission bits live only inside /Encrypt /P, so the rewritten
    // output is inherently unrestricted (Pdf::permissions() on it is
    // None). On the current rewrite path the two flags therefore produce
    // identical output bytes; they differ only in intent and diagnostic:
    //
    // - `--remove-restrictions` prints a one-line diagnostic when an
    //   encrypted input was de-restricted (flpdf-9hc.3.18).
    // - `--decrypt` is silent in both cases, matching qpdf `--decrypt`
    //   (flpdf-9hc.4.10).
    //
    // Neither flag bypasses authentication: `open_pdf` below performs the
    // same auth as a plain `rewrite`, so a wrong/missing password is
    // rejected identically and any diagnostic (printed only after a
    // successful write) is never reached. `decrypt` is currently a flag-
    // only parameter; it becomes behaviorally meaningful once `--encrypt`
    // (flpdf-9hc.4.9) lands and the rewrite gains the ability to preserve
    // or produce encryption — at that point `--decrypt` will override
    // `--encrypt` back to plaintext.
    let _ = decrypt;
    if linearize {
        let mut pdf = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf)?;
        let plan = LinearizationPlan::from_pdf(&mut pdf)?;
        let renumber = RenumberMap::from_plan(&plan);

        // Re-open the PDF so `write_linearized` can seek/read objects independently.
        let mut pdf2 = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf2)?;
        let had_signatures = if remove_restrictions {
            pdf_has_signature_evidence(&mut pdf2)?
        } else {
            false
        };
        let mut options = options;
        if had_signatures {
            options.full_rewrite = true;
            clear_sig_flags(&mut pdf2)?;
            strip_signature_values(&mut pdf2)?;
        }
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &options)?;
        doc.back_patch()?;

        std::fs::write(&output, &doc.bytes)?;
        if had_signatures {
            eprintln!("flpdf: warning: removed signatures; signatures are now invalidated");
        }
        // The linearize branch rejects encrypted input outright via
        // reject_encrypted_write above, so an encrypted (restricted) input
        // never reaches here; on unsigned unencrypted input there is nothing
        // to de-restrict. Per qpdf-lenient behaviour that diagnostic is
        // omitted.
    } else {
        let mut pdf = open_pdf(&input, repair, password)?;
        // Capture encryption state BEFORE the write: the plaintext path
        // below drops /Encrypt, so this must be sampled while the in-memory
        // model still reflects the input.
        let was_encrypted = pdf.is_encrypted();
        let had_signatures = if remove_restrictions {
            pdf_has_signature_evidence(&mut pdf)?
        } else {
            false
        };
        let mut options = options;
        if was_encrypted {
            options.full_rewrite = true;
        }
        if had_signatures {
            options.full_rewrite = true;
            // --remove-restrictions intentionally invalidates signatures, so
            // opt in to the writer's signed full-rewrite path explicitly.
            // strip_signature_values removes each field's /V but preserves the
            // /FT /Sig field dictionary, which signature_rewrite_impact still
            // counts as signed in FullRewrite mode — so without this flag the
            // writer would refuse with Error::Signed even after stripping.
            options.allow_signed_full_rewrite = true;
            clear_sig_flags(&mut pdf)?;
            strip_signature_values(&mut pdf)?;
        }

        // ── Content mutation pass ─────────────────────────────────────────────
        //
        // The mutations below operate on the in-memory Pdf model (via set_object).
        // They are only visible in the output when the full-rewrite path is used
        // (the incremental-update path copies source bytes verbatim and silently
        // ignores in-memory mutations). Therefore we force full_rewrite = true
        // whenever any content-mutating flag is active.
        //
        // Application order (semantically motivated):
        //   1. coalesce_page_contents  — merge /Contents arrays so subsequent
        //      passes always see a single stream per page.
        //   2. normalize_content_stream — re-tokenize the (now-unified) stream
        //      to canonical whitespace form.
        //   3. remove_unreferenced_resources — scan the final content stream to
        //      decide which /Resources entries are reachable, then prune the rest.
        //   4. write (compress_streams / newline_before_endstream) — byte-emission
        //      policies applied by the writer, not the pre-processing step.
        // INTENTIONAL DEFAULT (flpdf-9hc.12.7 acceptance: "defaults match
        // qpdf documented behavior"). qpdf's defaults are
        // `--remove-unreferenced-resources=auto` and `--compress-streams=y`,
        // and qpdf always performs a full rewrite. flpdf mirrors this:
        // because `remove_unref` defaults to `auto` (≠ `no`), a plain
        // `flpdf rewrite IN OUT` takes the full-rewrite path and applies the
        // safe `auto` pruning + default FlateDecode compression — exactly
        // what plain `qpdf IN OUT` does. This is a deliberate, documented
        // behavior (not an accidental regression); pass
        // `--remove-unreferenced-resources=no` to opt out of pruning. The
        // observable effect (qpdf-structural parity, smaller output) is
        // captured by tests/golden/compat-matrix.md and asserted by the
        // `rewrite_default_is_qpdf_equivalent_full_rewrite` CLI test.
        let needs_mutation = coalesce_contents
            || normalize_content
            || remove_unref != CliRemoveUnreferencedResources::No
            || generate_appearances
            || flatten_annotations_mode.is_some()
            || flatten_rotation
            || !overlay_specs.is_empty();
        if needs_mutation {
            options.full_rewrite = true;
        }

        // --min-version / --force-version rewrite the `%PDF-x.y` header line,
        // which only the new-generation write paths emit. The incremental-update
        // path (`write_pdf`) copies the source header verbatim and would
        // silently drop the requested version. qpdf always full-rewrites, so
        // every `qpdf --force-version`/`--min-version` invocation honors the
        // flag; mirror that by promoting to full_rewrite whenever a version
        // setter is active and we would otherwise take the incremental path
        // (e.g. `rewrite --remove-unreferenced-resources=no --force-version=1.4`
        // on unencrypted input). flpdf-9hc.13.1.
        if options.min_version.is_some() || options.force_version.is_some() {
            options.full_rewrite = true;
        }

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
        if normalize_content {
            let page_refs = pages::page_refs(&mut pdf)?;
            for page_ref in page_refs {
                apply_normalize_content(&mut pdf, page_ref)?;
            }
        }

        // Step 3: remove unreferenced /Resources entries.
        if remove_unref != CliRemoveUnreferencedResources::No {
            remove_unreferenced_resources(&mut pdf, remove_unref.into())?;
        }

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
            flatten_annotations(&mut pdf, mode.into())?;
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
        // here; the new objects only surface because full_rewrite was forced on
        // above.
        if !overlay_specs.is_empty() {
            let mut built = build_overlay_specs(overlay_specs, repair)?;
            flpdf::apply_overlay_specs(&mut pdf, &mut built)?;
        }

        let mut out = File::create(output)?;
        write_pdf_with_options(&mut pdf, &mut out, &options)?;

        if remove_restrictions && was_encrypted {
            eprintln!("flpdf: removed restrictions (encryption and advisory permissions stripped)");
        }
        if had_signatures {
            eprintln!("flpdf: warning: removed signatures; signatures are now invalidated");
        }
        // Unencrypted input + --remove-restrictions is a no-op rewrite
        // (exit 0, valid output, no diagnostic) — nothing was restricted,
        // matching qpdf's lenient handling of --remove-restrictions on
        // unencrypted files.
    }
    Ok(())
}

/// Generate `/AP` `/N` appearance streams for widget annotations that lack one
/// (`--generate-appearances`).
///
/// Walks every page's `/Annots`, keeps only Widget annotations whose `/AP` `/N`
/// is missing, and renders an appearance from the field's `/FT` (`Tx` → text,
/// `Btn` → button, `Ch` → choice). Widgets that already carry an `/AP` `/N` are
/// left untouched, matching qpdf which only fills in *missing* appearances.
///
/// Review-pattern compliance:
/// - #2 (indirect references): `/FT` is read via [`FormFieldObjectHelper::field_type`]
///   and `/AP` via [`AnnotationObjectHelper::appearance`], both of which resolve
///   references and the inheritable field tree internally.
/// - #4 (graph traversal): targets are limited to the known `/Annots` positions
///   surfaced by [`enumerate_document_annotations`] rather than a brute-force
///   scan of all live objects.
///
/// The candidate `ObjectRef`s are collected up front into an owned `Vec` so the
/// per-widget mutation loop holds a single `&mut pdf` borrow at a time.
fn generate_missing_appearances<R: Read + Seek>(pdf: &mut Pdf<R>) -> CliResult<()> {
    // Collect candidate widget refs first (the enumeration borrows `pdf`).
    let mut candidates: Vec<ObjectRef> = Vec::new();
    for (_page_ref, annots) in enumerate_document_annotations(pdf)? {
        for annot in annots {
            if annot.is_widget {
                candidates.push(annot.annot_ref);
            }
        }
    }

    for widget_ref in candidates {
        // Skip widgets that already have a normal appearance (/AP /N). qpdf
        // only synthesizes appearances for fields that lack them.
        let has_normal = {
            let mut helper = AnnotationObjectHelper::new(widget_ref, pdf);
            // Treat /AP/N == null the same as absent. The flattening pass
            // (resolve_ap_n) skips a null /N, so counting it as "has normal"
            // here would skip generation too — silently dropping the widget's
            // value from both passes.
            helper
                .appearance()?
                .is_some_and(|ap| ap.get("N").is_some_and(|val| !matches!(val, Object::Null)))
        };
        if has_normal {
            continue;
        }

        // Dispatch on the inherited /FT (resolved through the field tree). The
        // generate_* helpers each re-verify /FT and return None on a mismatch,
        // so this is a fast-path filter that avoids three speculative calls.
        let field_type = {
            let mut helper = FormFieldObjectHelper::new(widget_ref, pdf);
            helper.field_type()?
        };
        match field_type.as_deref() {
            Some(b"Tx") => {
                generate_text_field_appearance(pdf, widget_ref)?;
            }
            Some(b"Btn") => {
                generate_button_field_appearance(pdf, widget_ref)?;
            }
            Some(b"Ch") => {
                generate_choice_field_appearance(pdf, widget_ref)?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn pdf_has_signature_evidence<R: Read + Seek>(pdf: &mut Pdf<R>) -> CliResult<bool> {
    let has_sig_flags = acroform_sig_flags(pdf)?
        .is_some_and(|flags| flags & (SIG_FLAGS_SIGNATURES_EXIST | SIG_FLAGS_APPEND_ONLY) != 0);
    Ok(has_sig_flags || !pdf.signatures()?.is_empty())
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
/// the primary input path. Also returns the set of distinct resolved paths so
/// the caller can enforce the single-document scope boundary.
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
/// Grammar: `[--file=]FILE [--password=PW] [--to=R] [--from=R] [--repeat=R]`
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
            if file.is_none() {
                return Err(format!(
                    "{flag}: --password= must follow the source file in the segment"
                )
                .into());
            }
            if password.is_some() {
                return Err(format!("{flag}: duplicate --password= in segment").into());
            }
            password = Some(pw.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--to=") {
            if file.is_none() {
                return Err(
                    format!("{flag}: --to= must follow the source file in the segment").into(),
                );
            }
            if to.is_some() {
                return Err(format!("{flag}: duplicate --to= in segment").into());
            }
            PageRange::parse(r)
                .map_err(|e| format!("{flag}: invalid --to= page range {r:?}: {e}"))?;
            to = Some(r.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--from=") {
            if file.is_none() {
                return Err(
                    format!("{flag}: --from= must follow the source file in the segment").into(),
                );
            }
            if from.is_some() {
                return Err(format!("{flag}: duplicate --from= in segment").into());
            }
            PageRange::parse(r)
                .map_err(|e| format!("{flag}: invalid --from= page range {r:?}: {e}"))?;
            from = Some(r.to_string());
            continue;
        }
        if let Some(r) = tok.strip_prefix("--repeat=") {
            if file.is_none() {
                return Err(format!(
                    "{flag}: --repeat= must follow the source file in the segment"
                )
                .into());
            }
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
/// # Errors
///
/// Returns an error if a group is not terminated by a `--` token, or if
/// [`parse_overlay_segment`] rejects the captured tokens (missing/duplicate
/// file, invalid page range, unknown sub-flag, …).
fn extract_overlay_groups(args: Vec<String>) -> CliResult<(Vec<String>, Vec<OverlaySpec>)> {
    let mut residual: Vec<String> = Vec::with_capacity(args.len());
    let mut specs: Vec<OverlaySpec> = Vec::new();

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
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
/// file is surfaced as a CLI error. Default page-range semantics match qpdf:
/// `--from`/`--to` default to all source/destination pages (an empty range
/// string), and `--repeat` is absent by default (`None`, i.e. no repetition).
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
        let options = PdfOpenOptions {
            repair,
            password: spec
                .password
                .as_ref()
                .map(|p| p.as_bytes().to_vec())
                .unwrap_or_default(),
            ..Default::default()
        };
        let source = Pdf::open_with_options(BufReader::new(file), options)
            .map_err(|error| error_with_file(&path, actionable_password_error(error)))?;

        let kind = match spec.kind {
            OverlayKind::Overlay => flpdf::OverlayKind::Overlay,
            OverlayKind::Underlay => flpdf::OverlayKind::Underlay,
        };
        let from = PageRange::parse(spec.from.as_deref().unwrap_or(""))?;
        let to = PageRange::parse(spec.to.as_deref().unwrap_or(""))?;
        let repeat = match &spec.repeat {
            Some(r) => Some(PageRange::parse(r)?),
            None => None,
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

/// Run the `--pages` extraction pipeline.
///
/// Pipeline order is fixed as follows:
///   1. page_combine / page_collate → selected ObjectRef list
///   2. page_tree_rebuild::rebuild_page_tree → RebuildResult
///   3. apply_rotate_to_pages (on the rebuilt OUTPUT leaves; qpdf-observed)
///   4. outline_dest_remap::remap_outline_and_dests
///   5. struct_tree_pg::drop_struct_elem_dangling_pg
///   6. thread_bead_p::drop_thread_bead_dangling_p
///      6.5. objr_obj_annot_p::drop_objr_obj_annot_dangling_p
///   7. subset_prune::prune_after_subset (Auto/Yes/No)
///   8. acroform_field_prune::prune_acroform_after_subset
///   9. write (or split_pages when --split-pages is set)
///
/// SCOPE BOUNDARY (single document only):
/// `rebuild_page_tree` and the post-rebuild passes operate on ONE [`Pdf`].
/// Cross-document page merge (selecting pages from more than one distinct
/// source file) — and the cross-doc AcroForm field-collision renaming it
/// would require — is explicitly out of scope here. When the resolved
/// `--pages` specs reference more than one distinct file we surface an
/// actionable [`Error::Unsupported`] instead of silently producing wrong
/// output or swallowing the limitation.
#[allow(clippy::too_many_arguments)]
fn run_page_extraction(
    primary_input: &std::path::Path,
    output: &std::path::Path,
    repair: bool,
    password: &PasswordArgs,
    page_ops: &PageOpArgs,
    remove_unref: CliRemoveUnreferencedResources,
    options: WriteOptions,
) -> CliResult<()> {
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

    // CombinedPlan::from_specs (below) opens each input using only the
    // segment-local InputSpec password. For `--pages . ...` on an encrypted
    // primary input where the user supplied the top-level `--password`, the
    // spec carries no password and the planning open would fail before the
    // later open_pdf(..., &src_pw) path applies it. Backfill the top-level
    // password into specs that lack their own so planning and the rebuild
    // open use the same credential. (Single-document scope is enforced just
    // below, so every spec resolves to the primary input.)
    if let Some(top_pw) = &password.password {
        for spec in &mut inputs {
            if spec.password.is_none() {
                spec.password = Some(top_pw.clone());
            }
        }
    }

    // ── Single-document scope enforcement ────────────────────────────────
    // Compare *canonicalized* source paths so the same file spelled
    // differently (`tests/x.pdf` vs `./tests/x.pdf`, the `.` shorthand,
    // relative/symlinked equivalents) is correctly treated as one document.
    // The original path is preserved for opening/reporting.
    let mut distinct: Vec<std::path::PathBuf> = Vec::new();
    for spec in &inputs {
        // Source inputs must exist to be opened; if canonicalization fails
        // fall back to the literal path (the open will surface a clear error).
        let key = std::fs::canonicalize(&spec.path).unwrap_or_else(|_| spec.path.clone());
        if !distinct.contains(&key) {
            distinct.push(key);
        }
    }
    if distinct.len() > 1 {
        return Err(format!(
            "--pages: cross-document page merge is not supported at this layer \
             (got {} distinct source files: {}). Single-document extraction \
             (repeats of the same file or '.') is supported; cross-doc merge \
             and AcroForm field-collision handling are tracked in a separate issue.",
            distinct.len(),
            distinct
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }

    // CombinedPlan::from_specs opens each file itself; its per-input
    // PagePlans carry source ObjectRefs that are stable across a re-open of
    // identical bytes. We use it only for selection/collation planning, then
    // open the (single) resolved source ourselves for the rebuild passes.
    let plan = CombinedPlan::from_specs(inputs.clone())?;

    let combined_pages = match page_ops.collate.as_deref() {
        Some(raw) => {
            let n = parse_collate_n(raw)?;
            collate(&plan, n)?
        }
        None => plan.flat_pages().to_vec(),
    };

    // All combined pages belong to source_index files that all resolve to the
    // same path (enforced above), so their page_refs are valid against a
    // freshly-opened handle of that file.
    let source_path = &inputs[0].path;
    let source_password = inputs[0].password.clone();
    let mut src_pw = password.clone();
    if let Some(pw) = source_password {
        src_pw.password = Some(pw);
        src_pw.password_file = None;
    }
    let mut pdf = open_pdf(&source_path.to_path_buf(), repair, &src_pw)?;
    reject_encrypted_write(&pdf)?;

    let selected: Vec<ObjectRef> = combined_pages.iter().map(|cp| cp.page.page_ref).collect();
    if selected.is_empty() {
        return Err("--pages: page selection is empty".into());
    }

    // Step 2: rebuild the page tree from the selected leaves.
    let result = rebuild_page_tree(&mut pdf, &selected)?;

    // Step 3: --rotate, applied in argument order. In --pages mode the
    // range indexes the OUTPUT page list (result.new_kids), matching the
    // qpdf 11.9.0 observation documented at the top of this section.
    // Ordering note: rotate runs after rebuild but before the remap/prune
    // passes. This is order-independent w.r.t. those passes — rotate only
    // mutates each leaf's /Rotate, never the /Outlines, /Names, /AcroForm,
    // or orphaned-resource graph that steps 4–6 operate on.
    apply_rotate_specs(&mut pdf, &page_ops.rotate, &result.new_kids)?;

    // Step 4: outline / named-destination remap-or-drop (8.10).
    remap_outline_and_dests(&mut pdf, &result)?;

    // Step 5: struct-tree /Pg drop (the structural-reference drop family —
    // must run before the prune so the now-unreferenced page is swept).
    let objr_obj_targets = drop_struct_elem_dangling_pg(&mut pdf, &result)?;

    // Step 6: article-thread bead /P drop (same structural-reference drop
    // family). A bead whose /P targets a removed page has the /P dropped (the
    // bead and its ring are kept); must also run before the prune so the
    // now-unreferenced page is swept.
    drop_thread_bead_dangling_p(&mut pdf, &result)?;

    // Step 6.5: drop the dangling /P on annotations kept alive only through a
    // struct-tree OBJR /Obj (same structural-reference drop family). The OBJR
    // /Obj targets were collected by Step 5's struct-tree walk. Must run before
    // the prune so the now-unreferenced removed page is swept.
    drop_objr_obj_annot_dangling_p(&mut pdf, &result, &objr_obj_targets)?;

    // Step 7: unreferenced-resource prune + xref GC (8.9).
    prune_after_subset(&mut pdf, remove_unref.into())?;

    // Step 8: AcroForm field/widget prune (8.11). The single-document API
    // boundary makes the cross-doc field-collision case unreachable here;
    // any Unsupported it returns is propagated, not swallowed.
    prune_acroform_after_subset(&mut pdf, &result)?;

    // Step 9: serialize. Extraction always implies a full document rewrite.
    let mut options = options;
    options.full_rewrite = true;
    let mut bytes: Vec<u8> = Vec::new();
    write_pdf_with_options(&mut pdf, &mut bytes, &options)?;

    if let Some(raw) = page_ops.split_pages.as_deref() {
        let n = parse_split_n(raw)?;
        split_pages(&bytes, n, output, options.deterministic_id)?;
    } else {
        std::fs::write(output, &bytes)?;
    }
    Ok(())
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

/// Apply `--rotate` / `--split-pages` to a plain (no `--pages`) rewrite.
///
/// qpdf accepts these without `--pages` (exit 0). `--rotate` mutates the
/// source document's pages directly (no page-tree rebuild); `--split-pages`
/// chunks the rewritten output. `--collate` without `--pages` is a no-op,
/// matching qpdf.
fn run_rewrite_with_page_ops(
    input: &std::path::Path,
    output: &std::path::Path,
    repair: bool,
    password: &PasswordArgs,
    page_ops: &PageOpArgs,
    options: WriteOptions,
) -> CliResult<()> {
    if page_ops.empty {
        return Err(
            "--empty is accepted by qpdf but not implemented in flpdf at this layer \
             (tracked separately); rerun without --empty"
                .into(),
        );
    }
    let mut pdf = open_pdf(&input.to_path_buf(), repair, password)?;
    reject_encrypted_write(&pdf)?;

    if !page_ops.rotate.is_empty() {
        let page_refs = pages::page_refs(&mut pdf)?;
        apply_rotate_specs(&mut pdf, &page_ops.rotate, &page_refs)?;
    }

    // Force a full rewrite so the in-memory /Rotate mutations are emitted
    // (the incremental write path copies source bytes verbatim).
    let mut options = options;
    options.full_rewrite = true;
    let mut bytes: Vec<u8> = Vec::new();
    write_pdf_with_options(&mut pdf, &mut bytes, &options)?;

    if let Some(raw) = page_ops.split_pages.as_deref() {
        let n = parse_split_n(raw)?;
        split_pages(&bytes, n, output, options.deterministic_id)?;
    } else {
        std::fs::write(output, &bytes)?;
    }
    Ok(())
}

/// True when any page-operation flag that requires the page-op code paths is
/// set. `--collate` alone (no `--pages`) is a documented no-op and does NOT
/// trigger this on its own.
fn page_ops_active(p: &PageOpArgs) -> bool {
    !p.pages.is_empty() || !p.rotate.is_empty() || p.split_pages.is_some() || p.empty
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
) -> CliResult<()> {
    // Resolve the page dictionary to find its /Contents value.
    let page_obj = pdf.resolve_borrowed(page_ref)?;
    let Object::Dictionary(page_dict) = page_obj else {
        return Ok(()); // Not a page dict — skip silently.
    };

    let contents = match page_dict.get("Contents").cloned() {
        None => return Ok(()), // Empty page — nothing to normalize.
        Some(c) => c,
    };

    match contents {
        Object::Reference(stream_ref) => {
            normalize_and_store_stream(pdf, stream_ref)?;
        }
        Object::Array(elems) => {
            // Multiple content streams — normalize each one in-place.
            // (If --coalesce-contents was also given, this is already a
            // single stream; but we handle the array case for safety.)
            for elem in elems {
                if let Object::Reference(r) = elem {
                    normalize_and_store_stream(pdf, r)?;
                }
                // Direct-stream elements are unusual in real PDFs; skip them
                // (they have no separate object to patch).
            }
        }
        Object::Stream(_) => {
            // Direct (inline) stream on the page dict itself — no separate
            // object ref to patch; skip silently.
        }
        _ => {}
    }
    Ok(())
}

/// Normalize the raw decoded bytes of the indirect stream at `stream_ref`
/// and write the result back into `pdf` via [`Pdf::set_object`].
fn normalize_and_store_stream<R: std::io::Read + std::io::Seek>(
    pdf: &mut Pdf<R>,
    stream_ref: ObjectRef,
) -> CliResult<()> {
    let resolved = pdf.resolve_borrowed(stream_ref)?;
    let Object::Stream(stream) = resolved else {
        return Ok(()); // Not a stream — skip.
    };

    // Decode the stored bytes through the declared filter pipeline.
    let decoded = filters::decode_stream_data(&stream.dict, &stream.data)?;

    // Normalize the decoded content stream bytes.
    let normalized = normalize_content_stream(&decoded)?;

    // Build a new stream dict with the updated /Length.
    let mut new_dict: Dictionary = stream.dict.clone();
    // Remove filter / encode-form keys: the normalized bytes are raw (no filter).
    for key in &["Filter", "DecodeParms", "DP", "DL"] {
        new_dict.remove(key);
    }
    new_dict.insert("Length", Object::Integer(normalized.len() as i64));

    let new_stream = Stream::new(new_dict, normalized);
    pdf.set_object(stream_ref, Object::Stream(new_stream));
    Ok(())
}

fn run_qdf(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    reject_encrypted_write(&pdf)?;

    // The `qdf` subcommand is an alias of `rewrite --qdf` (epic flpdf-9hc.6
    // architecture decision): produce canonical QDF via the full-rewrite
    // path rather than the old standalone `write_qdf` raw dump. The QDF code
    // path lives in write_pdf_full_rewrite, so full_rewrite must be true.
    let mut options = WriteOptions::default();
    options.qdf = true;
    options.full_rewrite = true;
    let mut out = File::create(output)?;
    write_pdf_with_options(&mut pdf, &mut out, &options)?;
    Ok(())
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

fn reject_encrypted_write<R: std::io::Read + std::io::Seek>(pdf: &Pdf<R>) -> CliResult<()> {
    if pdf.is_encrypted() {
        return Err("encrypted PDF output is not supported for this mode; use plain rewrite to produce decrypted plaintext".into());
    }
    Ok(())
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
    println!("{}", String::from_utf8_lossy(&out));
    Ok(())
}

fn run_show_stream(cmd: ShowStreamCommand) -> CliResult<()> {
    let object_ref = ObjectRef::parse(&cmd.object_ref)?;
    let mut pdf = open_pdf(&cmd.input, cmd.repair, &cmd.password)?;
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

    if cmd.raw {
        if let Some(path) = cmd.out {
            std::fs::write(path, &stream.data)?;
        } else {
            std::io::stdout().write_all(&stream.data)?;
            std::io::stdout().flush()?;
        }
        return Ok(());
    }

    // For a single passthrough codec (DCTDecode, JBIG2Decode, JPXDecode,
    // CCITTFaxDecode) emit a human-readable marker instead of dumping binary.
    // The codec may be stored either as a direct name (`/Filter /DCTDecode`) or
    // as a single-element array (`/Filter [/DCTDecode]`); both are equivalent
    // per PDF spec. Multi-element filter chains fall through to the decode path
    // (scope: flpdf-9hc.7.5).
    let passthrough_label = stream.dict.get("Filter").and_then(|filter| {
        let name = filter.as_name().or_else(|| match filter.as_array() {
            Some([single]) => single.as_name(),
            _ => None,
        })?;
        filters::passthrough_codec_label(name)
    });
    if let Some(label) = passthrough_label {
        // These codecs are not decodable. With `--out`, write the raw stored
        // bytes (the only available representation — e.g. the embedded JPEG for
        // DCTDecode) and report the marker on stderr. Without `--out`, print the
        // marker to stdout instead of dumping binary to the terminal.
        if let Some(path) = cmd.out {
            std::fs::write(path, &stream.data)?;
            eprintln!("<binary, {} bytes, codec {}>", stream.data.len(), label);
        } else {
            println!("<binary, {} bytes, codec {}>", stream.data.len(), label);
        }
        return Ok(());
    }

    let bytes = filters::decode_stream_data(&stream.dict, &stream.data)?;
    if let Some(path) = cmd.out {
        std::fs::write(path, bytes)?;
    } else {
        std::io::stdout().write_all(&bytes)?;
        std::io::stdout().flush()?;
    }
    Ok(())
}

fn run_show_info(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let info_ref = pdf
        .trailer()
        .get_ref("Info")
        .ok_or("document info dictionary not found")?;
    let info = pdf.resolve_borrowed(info_ref)?;

    let Object::Dictionary(dict) = info else {
        return Err(format!("info object {} is not a dictionary", info_ref).into());
    };

    println!("Info:");
    for (key, value) in dict.iter() {
        let key = String::from_utf8_lossy(key);
        println!("  {} = {}", key, object_to_pdf(value));
    }
    Ok(())
}

fn run_show_catalog(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve_borrowed(catalog_ref)?;
    println!("Catalog: {}", object_to_pdf(catalog));
    Ok(())
}

fn run_show_metadata(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let catalog_ref = pdf.root_ref().ok_or("document catalog missing")?;
    let catalog = pdf.resolve_borrowed(catalog_ref)?;

    let Object::Dictionary(catalog) = catalog else {
        return Err(format!("document catalog {} is not a dictionary", catalog_ref).into());
    };

    match catalog.get_ref("Metadata") {
        Some(metadata_ref) => {
            let metadata = pdf.resolve_borrowed(metadata_ref)?;
            match metadata {
                Object::Stream(stream) => {
                    let kind = stream
                        .dict
                        .get("Subtype")
                        .map(object_to_pdf)
                        .unwrap_or_else(|| String::from("Unknown"));
                    println!("Metadata: stream ({}) {}", metadata_ref, kind);
                    println!("  length: {}", stream.data.len());
                    const MAX_METADATA_PREVIEW_BYTES: usize = 1000;
                    let preview_len = stream.data.len().min(MAX_METADATA_PREVIEW_BYTES);
                    let preview = String::from_utf8_lossy(&stream.data[..preview_len]);
                    if stream.data.len() > MAX_METADATA_PREVIEW_BYTES {
                        println!(
                            "  preview: {} (truncated, total {} bytes)",
                            preview,
                            stream.data.len()
                        );
                    } else {
                        println!("  preview: {}", preview);
                    }
                }
                Object::Dictionary(dict) => {
                    println!("Metadata: dictionary {}", metadata_ref);
                    for (key, value) in dict.iter() {
                        let key = String::from_utf8_lossy(key);
                        println!("  {}: {}", key, object_to_pdf(value));
                    }
                }
                other => {
                    println!("Metadata: non-stream {}", metadata_ref);
                    println!("  type: {}", object_to_pdf(other));
                }
            }
        }
        None => println!("Metadata: <missing>"),
    }

    Ok(())
}

fn run_show_outline(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;

    let items = match outline::outline_items(&mut pdf) {
        Ok(items) => items,
        Err(error) => {
            eprintln!("Warning: {error}");
            Vec::new()
        }
    };

    println!("Outline:");
    if items.is_empty() {
        println!("  <empty>");
        return Ok(());
    }

    for (index, item) in items.iter().enumerate() {
        println!("{}{}: {}", "  ".repeat(item.depth), index + 1, item.title);
    }
    Ok(())
}

fn run_show_fonts(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;

    let font_refs = match fonts::font_entries(&mut pdf) {
        Ok(font_refs) => font_refs,
        Err(error) => {
            eprintln!("Warning: {error}");
            Default::default()
        }
    };

    println!("Fonts:");
    if font_refs.is_empty() {
        println!("  <none>");
        return Ok(());
    }

    for (name, font_obj) in font_refs {
        let font_name = String::from_utf8_lossy(&name);
        if let Object::Dictionary(dict) = font_obj {
            println!("  /{}", font_name);
            if let Some(font_type) = dict.get("Type") {
                println!("    type: {}", object_to_pdf(font_type));
            }
            if let Some(subtype) = dict.get("Subtype") {
                println!("    subtype: {}", object_to_pdf(subtype));
            }
            if let Some(base_font) = dict.get("BaseFont") {
                println!("    base_font: {}", object_to_pdf(base_font));
            }
        } else {
            println!("  /{}", font_name);
            println!("    type: <invalid>");
            println!("    subtype: <invalid>");
            println!("    base_font: <invalid>");
        }
    }

    Ok(())
}

fn run_show_npages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let pages = pages::page_refs(&mut pdf)?;
    println!("{}", pages.len());
    Ok(())
}

fn run_show_pages(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let page_refs = pages::page_refs(&mut pdf)?;
    for (index, page_ref) in page_refs.iter().enumerate() {
        let page = pdf.resolve_borrowed(*page_ref)?;
        let Object::Dictionary(dict) = page else {
            continue;
        };

        println!("page {}: {}", index + 1, page_ref);
        if let Some(media_box) = dict.get("MediaBox") {
            println!("  media-box: {}", object_to_pdf(media_box));
        }
        if let Some(resources) = dict.get("Resources") {
            println!("  resources: {}", object_to_pdf(resources));
        }
        if let Some(contents) = dict.get("Contents") {
            println!("  contents: {}", object_to_pdf(contents));
        }
        if let Some(rotate) = dict.get("Rotate") {
            println!("  rotate: {}", object_to_pdf(rotate));
        }
    }

    Ok(())
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
    match Pdf::open_with_options(BufReader::new(file), options) {
        Ok(pdf) => Ok(EncryptionProbe::Opened {
            encrypted: pdf.is_encrypted(),
        }),
        // A wrong/empty password: the document is definitely encrypted, we
        // just have not authenticated it. qpdf treats this as "encrypted,
        // password required".
        Err(flpdf::Error::Encrypted(flpdf::EncryptedError::BadPassword)) => {
            Ok(EncryptionProbe::EncryptedAuthFailed)
        }
        Err(other) => Err(other.into()),
    }
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
            println!("{}", hex_lower(key));
            Ok(())
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
        println!("File is not encrypted");
        return Ok(());
    };

    // ── flpdf-specific leading lines (placed BEFORE the qpdf block so a
    //    qpdf-compatible grep still matches the qpdf lines verbatim) ──
    println!("V = {}", info.v);
    println!("Length = {}", info.length_bits);
    println!("Filter = {}", info.filter);
    println!(
        "EncryptMetadata = {}",
        if info.encrypt_metadata {
            "true"
        } else {
            "false"
        }
    );
    let mut cf_names: Vec<_> = info.named_crypt_filters.clone();
    cf_names.sort();
    for (name, method) in &cf_names {
        println!("CF /{name} = {method}");
    }

    // ── Verbatim qpdf `--show-encryption` lines (source:
    //    qpdf libqpdf/QPDFJob.cc QPDFJob::showEncryption) ──
    println!("R = {}", info.r);
    println!("P = {}", info.permissions.raw());
    // qpdf prints `User password = <recovered cleartext>` here; flpdf does
    // not recover the cleartext user password (documented divergence), so
    // that line is intentionally omitted.
    if pdf.owner_password_matched() {
        println!("Supplied password is owner password");
    }
    if pdf.user_password_matched() {
        println!("Supplied password is user password");
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
    println!("extract for accessibility: {}", show(allow_accessibility));
    println!("extract for any purpose: {}", show(allow_extract_all));
    println!("print low resolution: {}", show(allow_print_low));
    println!("print high resolution: {}", show(allow_print_high));
    println!("modify document assembly: {}", show(allow_modify_assembly));
    println!("modify forms: {}", show(allow_modify_form));
    println!("modify annotations: {}", show(allow_modify_annotation));
    println!("modify other: {}", show(allow_modify_other));
    println!("modify anything: {}", show(allow_modify_all));
    if info.v >= 4 {
        println!("stream encryption method: {}", info.stream_method);
        println!("string encryption method: {}", info.string_method);
        // qpdf prints the embedded-file ("file") method. When the document
        // declares no /EFF, qpdf falls back to the stream method.
        let file_method = info.eff_method.unwrap_or(info.stream_method);
        println!("file encryption method: {file_method}");
    }
    Ok(())
}

/// Lowercase hex encoding (qpdf `--show-encryption-key` format).
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn open_pdf(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<Pdf<BufReader<File>>> {
    open_pdf_impl(input, repair, password, false)
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
    open_pdf_impl(input, repair, password, true)
}

fn open_pdf_impl(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
    force_allow_weak_crypto: bool,
) -> CliResult<Pdf<BufReader<File>>> {
    let file = File::open(input).map_err(|error| error_with_file(input, error.into()))?;
    let mut options = pdf_open_options(repair, password)?;
    if force_allow_weak_crypto {
        options.allow_weak_crypto = true;
    }
    let pdf = Pdf::open_with_options(BufReader::new(file), options)
        .map_err(|error| error_with_file(input, actionable_password_error(error)))?;

    for diagnostic in pdf.repair_diagnostics().entries() {
        let location = diagnostic_location(input, diagnostic.offset);
        eprintln!("WARNING: {location}: {}", diagnostic.message);
    }
    // Skip the weak-crypto warning on the forced (inspection) path: the user
    // supplied no `--allow-weak-crypto` flag to acknowledge, and qpdf emits no
    // such warning for `--show-encryption[-key]`. On the normal path a weak
    // file only opens when the user did pass the flag, so the warning is apt.
    if pdf.uses_weak_crypto() && !force_allow_weak_crypto {
        eprintln!(
            "WARNING: {}: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied",
            input.display()
        );
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
        repair,
        password,
        password_mode,
        allow_weak_crypto,
        password_is_hex_key,
    })
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

/// Prefix a fatal error with the input path so main() renders the observed
/// qpdf shape `<progname>: <file>: <msg>` for open failures.
///
/// This type-erases the error; do not downcast the result.
fn error_with_file(input: &Path, error: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    format!("{}: {error}", input.display()).into()
}

fn actionable_password_error(error: flpdf::Error) -> Box<dyn std::error::Error> {
    if matches!(
        error,
        flpdf::Error::Encrypted(flpdf::EncryptedError::BadPassword)
    ) {
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

/// Parse a PDF date string of the form `D:YYYYMMDDHHmmSS` (with optional
/// trailing timezone) into `(year, month, day, hour, minute, second)`.
///
/// Only the local-time components are extracted; timezone info is ignored.
/// The `D:` prefix is required.
fn parse_pdf_date_arg(s: &str) -> CliResult<(u16, u8, u8, u8, u8, u8)> {
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
    let year: u16 = s[0..4]
        .parse()
        .map_err(|_| format!("invalid year in PDF date D:{s}"))?;
    let month: u8 = s[4..6]
        .parse()
        .map_err(|_| format!("invalid month in PDF date D:{s}"))?;
    let day: u8 = s[6..8]
        .parse()
        .map_err(|_| format!("invalid day in PDF date D:{s}"))?;
    let hour: u8 = s[8..10]
        .parse()
        .map_err(|_| format!("invalid hour in PDF date D:{s}"))?;
    let minute: u8 = s[10..12]
        .parse()
        .map_err(|_| format!("invalid minute in PDF date D:{s}"))?;
    let second: u8 = s[12..14]
        .parse()
        .map_err(|_| format!("invalid second in PDF date D:{s}"))?;
    Ok((year, month, day, hour, minute, second))
}

/// Parsed sub-flags for the `--add-attachment FILE [sub-flags] --` segment.
struct AddAttachmentArgs {
    /// Path to the file whose bytes will be embedded.
    file: PathBuf,
    /// Name-tree key (default: basename of `file`).
    key: Option<Vec<u8>>,
    /// Filename stored in `/UF`; `/F` uses an ASCII-safe fallback.
    filename: Option<Vec<u8>>,
    /// MIME type for `/EmbeddedFile /Subtype`.
    mimetype: Option<Vec<u8>>,
    /// Human-readable description for `/Filespec /Desc`.
    description: Option<Vec<u8>>,
    /// `/AFRelationship` name.
    af_relationship: Option<Vec<u8>>,
    /// `/Params /CreationDate` as `(year, month, day, hour, minute, second)`.
    creation_date: Option<(u16, u8, u8, u8, u8, u8)>,
    /// `/Params /ModDate` as `(year, month, day, hour, minute, second)`.
    mod_date: Option<(u16, u8, u8, u8, u8, u8)>,
    /// When true, replace an existing attachment with the same key.
    replace: bool,
}

/// Parse the raw token Vec captured by `--add-attachment … --` into
/// [`AddAttachmentArgs`].
///
/// Expected token order: FILE [--key=K] [--filename=F] [--mimetype=M]
/// [--description=D] [--creationdate=D] [--moddate=D] [--afrelationship=R]
/// [--replace]
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
    let mut af_relationship: Option<Vec<u8>> = None;
    let mut creation_date: Option<(u16, u8, u8, u8, u8, u8)> = None;
    let mut mod_date: Option<(u16, u8, u8, u8, u8, u8)> = None;
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
        } else if let Some(v) = token.strip_prefix("--afrelationship=") {
            af_relationship = Some(v.as_bytes().to_vec());
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
        af_relationship,
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
) -> CliResult<()> {
    let input = input.ok_or("--add-attachment: missing input PDF")?;
    let output = output.ok_or("--add-attachment: missing output PDF")?;
    let args = parse_add_attachment_segment(tokens)?;

    let payload = std::fs::read(&args.file)
        .map_err(|e| format!("--add-attachment: cannot read {:?}: {e}", args.file))?;

    let basename = path_basename(&args.file)?;
    let key = args.key.unwrap_or_else(|| basename.clone());
    let filename = args.filename.unwrap_or_else(|| basename.clone());
    let filename = String::from_utf8(filename).map_err(|_| {
        "--add-attachment: filename must be valid UTF-8 so it can be encoded as /UF"
    })?;

    let mut pdf = open_pdf(&input, repair, password)?;
    reject_encrypted_write(&pdf)?;

    // Duplicate-key handling.
    if !args.replace {
        let existing = list_attachment_info(&mut pdf)?;
        if existing.iter().any(|a| a.key == key) {
            return Err(format!(
                "--add-attachment: key {:?} already exists; use --replace to overwrite",
                String::from_utf8_lossy(&key)
            )
            .into());
        }
    } else {
        // Remove the existing entry so insert_embedded_file can start clean.
        remove_attachment(&mut pdf, &key)?;
    }

    let dates = FileParamDates {
        creation: args.creation_date,
        modification: args.mod_date,
    };

    let mut builder = FileSpecBuilder::new(ascii_filename_fallback(&filename), payload)
        .uf_filename(&filename)
        .compress(true)
        .dates(dates);
    if let Some(mime) = args.mimetype {
        builder = builder.mimetype(mime);
    }
    if let Some(desc) = args.description {
        builder = builder.description(desc);
    }
    if let Some(rel) = args.af_relationship {
        builder = builder.af_relationship(rel);
    }
    let filespec_ref = builder.build(&mut pdf)?;
    insert_embedded_file(&mut pdf, &key, filespec_ref)?;

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.deterministic_id = deterministic_id;
    let mut out = File::create(&output)?;
    write_pdf_with_options(&mut pdf, &mut out, &options)?;
    Ok(())
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
    reject_encrypted_write(&pdf)?;

    let found = remove_attachment(&mut pdf, key.as_bytes())?;
    if !found {
        return Err(format!("--remove-attachment: key {:?} not found in document", key).into());
    }

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.deterministic_id = deterministic_id;
    let mut out = File::create(&output)?;
    write_pdf_with_options(&mut pdf, &mut out, &options)?;
    Ok(())
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
    let entries = list_attachment_info(&mut pdf)?;
    let listing = format_attachment_list(&entries, verbose);
    print!("{listing}");
    Ok(())
}

/// `--show-attachment KEY [-o PATH] input`
fn run_show_attachment(
    input: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    key: &str,
    out_path: Option<PathBuf>,
) -> CliResult<()> {
    let input = input.ok_or("--show-attachment: missing input PDF")?;
    let mut pdf = open_pdf(&input, repair, password)?;
    let bytes = extract_attachment(&mut pdf, key.as_bytes()).map_err(|e| {
        format!(
            "--show-attachment: key {:?} not found or unreadable: {e}",
            key
        )
    })?;
    if let Some(path) = out_path {
        std::fs::write(&path, &bytes)
            .map_err(|e| format!("--show-attachment: cannot write to {:?}: {e}", path))?;
    } else {
        std::io::stdout().write_all(&bytes)?;
        std::io::stdout().flush()?;
    }
    Ok(())
}

/// `--copy-attachments-from FILE [--password=P] [--prefix=X] -- input output`
fn run_copy_attachments_from(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    tokens: Vec<String>,
    deterministic_id: bool,
) -> CliResult<()> {
    let input = input.ok_or("--copy-attachments-from: missing input PDF")?;
    let output = output.ok_or("--copy-attachments-from: missing output PDF")?;
    let args = parse_copy_attachments_segment(tokens)?;

    // Open the source with its own password (independent of the target's).
    let src_options = PdfOpenOptions {
        repair,
        password: args.password.clone(),
        ..PdfOpenOptions::default()
    };
    let src_file = File::open(&args.file)
        .map_err(|e| format!("--copy-attachments-from: cannot open {:?}: {e}", args.file))?;
    let mut src = Pdf::open_with_options(BufReader::new(src_file), src_options)
        .map_err(|e| format!("--copy-attachments-from: failed to open source PDF: {e}"))?;

    let mut target = open_pdf(&input, repair, password)?;
    reject_encrypted_write(&target)?;

    let prefix = args.prefix.as_deref();
    let count = copy_attachments_from(&mut target, &mut src, prefix)?;
    eprintln!("copied {count} attachment(s)");

    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.deterministic_id = deterministic_id;
    let mut out = File::create(&output)?;
    write_pdf_with_options(&mut target, &mut out, &options)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
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
    fn overlay_missing_file_with_password_error() {
        // --password= before any file token
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["--password=pw"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--overlay"), "got: {err}");
        assert!(err.contains("must follow"), "got: {err}");
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
    fn overlay_option_before_file_to_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["--to=1", "src.pdf"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must follow"), "got: {err}");
    }

    #[test]
    fn overlay_option_before_file_from_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["--from=1", "src.pdf"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must follow"), "got: {err}");
    }

    #[test]
    fn overlay_option_before_file_repeat_error() {
        let err = parse_overlay_segment(OverlayKind::Overlay, &strs(&["--repeat=1", "src.pdf"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must follow"), "got: {err}");
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
    fn extract_password_sub_flag_not_mistaken_for_terminator() {
        // `--password=…` starts with `--` but only a bare `--` terminates.
        let argv = strs(&["--overlay", "src.pdf", "--password=--weird", "--"]);
        let (_residual, specs) = extract_overlay_groups(argv).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].password.as_deref(), Some("--weird"));
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
}
