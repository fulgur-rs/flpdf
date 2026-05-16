use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use flpdf::{
    check_reader_with_options, filters, fonts,
    json_inspect::{
        build_qpdf_json_v2_with_options, filter_json_keys, filter_json_objects, DecodeLevel,
        JsonKey, JsonObjectSelector, StreamDataMode,
    },
    linearization::{
        check_linearization_path, write_linearized, LinearizationCheckError, LinearizationPlan,
        RenumberMap,
    },
    normalize_content_stream, outline, pages,
    pages::coalesce_page_contents,
    parse_pdf_version,
    resources::remove_unreferenced_resources,
    write_pdf_with_options, write_qdf, CompressStreams, Dictionary, NewlineBeforeEndstream, Object,
    ObjectRef, ObjectStreamMode, PasswordMode, Pdf, PdfOpenOptions, RemoveUnreferencedResources,
    Severity, Stream, WriteOptions,
};
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::PathBuf;

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
              "check", "linearize", "static_id", "dump_object",
              "show_info", "show_catalog", "show_metadata", "show_outline",
              "show_fonts", "show_npages", "show_pages", "output",
              "compress_streams", "linearize_pass1", "remove_restrictions",
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
    /// (top-level alias of `flpdf rewrite --static-id`). Testing only.
    #[arg(long = "static-id")]
    static_id: bool,
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
    /// `qpdf --compress-streams=y|n` compatibility flag.  Accepted but
    /// currently a no-op: flpdf does not re-encode stream contents on
    /// rewrite.  Provided so qtest commands parse cleanly.
    #[arg(long = "compress-streams")]
    compress_streams: Option<String>,
    /// `qpdf --linearize-pass1=PATH` compatibility flag.  Accepted; flpdf
    /// writes the pass-1 intermediate file as a copy of the final
    /// linearized output (qpdf writes a distinct intermediate; matching
    /// those bytes is out of scope here — see flpdf-vrn).
    #[arg(long = "linearize-pass1")]
    linearize_pass1: Option<PathBuf>,

    input: Option<PathBuf>,
    output: Option<PathBuf>,
}

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
    #[command(about = "Write a qdf-style raw dump into a file")]
    Qdf(QdfCommand),
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
    #[arg(long = "static-id")]
    static_id: bool,
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
    /// exactly as a plain `rewrite` would reject it. (Full decryption-mode
    /// semantics are tracked separately as `--decrypt`, flpdf-9hc.4.10.)
    #[arg(long = "remove-restrictions")]
    remove_restrictions: bool,
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
    /// Decode every stream through its filter chain and re-emit the document
    /// end-to-end with a single /FlateDecode filter per stream.  The output
    /// contains no /Prev chain.  Cannot be combined with --linearize.
    #[arg(long = "full-rewrite")]
    full_rewrite: bool,
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
    /// ignores this flag (tracked in flpdf-9hc.5.9).
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

fn main() {
    let args = Cli::parse();

    // JSON mode takes the first branch, but this is unambiguous: clap's
    // conflicts_with_all on --json (plus args_conflicts_with_subcommands on
    // Cli) guarantees no other top-level mode or subcommand can be set at
    // the same time, so the ordering here is a formality, not a precedence
    // rule that could silently shadow another requested mode.
    let result = if args.json.is_some() {
        run_json(&args)
    } else if let Some(command) = args.command {
        run_command(command)
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
        run_check(args.input, args.repair, &args.password)
    } else if args.linearize {
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
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
            false,                              // normalize_content
            false,                              // coalesce_contents
            CliRemoveUnreferencedResources::No, // remove_unreferenced (no-op for linearize path)
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
    } else {
        let mut options = WriteOptions::default();
        options.static_id = args.static_id;
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
        run_rewrite(
            args.input,
            args.output,
            args.repair,
            &args.password,
            false,
            args.remove_restrictions,
            false,                              // normalize_content
            false,                              // coalesce_contents
            CliRemoveUnreferencedResources::No, // remove_unreferenced (top-level alias is no-op)
            options,
        )
    };

    if let Err(error) = result {
        // If the error carries an explicit exit code (e.g. from run_check),
        // honour it.  Unknown/generic errors fall back to exit 2 (qpdf
        // convention for "error", unchanged from before this change).
        if let Some(exit_err) = error.downcast_ref::<CliExitError>() {
            // Only print a message when there is one; the caller may have
            // already printed its own summary (e.g. run_check prints
            // "PDF check succeeded" before returning exit 3 for warnings).
            if !exit_err.message.is_empty() {
                eprintln!("flpdf: {}", exit_err.message);
            }
            std::process::exit(exit_err.code.as_i32());
        }
        eprintln!("flpdf: {error}");
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
        "none" => StreamDataMode::None,
        "inline" => StreamDataMode::Inline,
        "file" => StreamDataMode::File {
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
    let mut v2 = build_qpdf_json_v2_with_options(&mut pdf, DecodeLevel::Generalized, &stream_mode)
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
    if let StreamDataMode::File { ref prefix } = stream_mode {
        let wanted_refs = collect_datafile_object_refs(&v2);
        // Reuse the same `pdf` handle the JSON was built from. Re-opening
        // the input here would risk the file being swapped mid-run, so the
        // JSON body and the side files could capture different snapshots.
        for oref in wanted_refs {
            let obj = pdf.resolve(oref)?;
            if let Object::Stream(stream) = obj {
                // qpdf names side files `<prefix>-nnn` where nnn is the
                // object number zero-padded to at least 3 digits
                // (qpdf manual, --json-stream-prefix). Match that so
                // tooling written against qpdf's layout finds the files.
                let side_path = format!("{prefix}-{:03}", oref.number);
                std::fs::write(&side_path, &stream.data)?;
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

fn run_command(command: Commands) -> CliResult<()> {
    match command {
        Commands::Check(cmd) => run_check(Some(cmd.input), cmd.repair, &cmd.password),
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
            let mut options = WriteOptions::default();
            options.static_id = cmd.static_id;
            options.min_version = cmd.min_version;
            options.force_version = cmd.force_version;
            options.full_rewrite = cmd.full_rewrite;
            options.object_streams = cmd.object_streams.into();
            options.compress_streams = match cmd.compress_streams {
                CliYesNo::Yes => CompressStreams::Yes,
                CliYesNo::No => CompressStreams::No,
            };
            options.newline_before_endstream = match cmd.newline_before_endstream {
                CliYesNo::Yes => NewlineBeforeEndstream::Yes,
                CliYesNo::No => NewlineBeforeEndstream::No,
            };
            let normalize_content = cmd.normalize_content == CliYesNo::Yes;
            let coalesce_contents = cmd.coalesce_contents;
            let remove_unref = cmd.remove_unreferenced_resources;
            run_rewrite(
                Some(cmd.input),
                Some(cmd.output),
                cmd.repair,
                &cmd.password,
                cmd.linearize,
                cmd.remove_restrictions,
                normalize_content,
                coalesce_contents,
                remove_unref,
                options,
            )
        }
    }
}

fn run_check(input: Option<PathBuf>, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let file = File::open(input)?;
    let options = pdf_open_options(repair, password)?;
    let report = check_reader_with_options(BufReader::new(file), options)
        .map_err(actionable_password_error)?;
    for diagnostic in report.diagnostics.entries() {
        let label = match diagnostic.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        eprintln!("{label}: {}", diagnostic.message);
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
        .any(|d| d.severity == Severity::Warning);

    if !report.valid {
        // Errors found — exit 2.
        return Err(Box::new(CliExitError {
            code: ExitCode::Errors,
            message: "PDF check failed".to_string(),
        }));
    }

    if has_warnings {
        // Warnings without errors — exit 3.  The success message has already
        // been printed above; pass an empty message so main() does not emit
        // a redundant "flpdf: ..." line.
        println!("PDF check succeeded");
        return Err(Box::new(CliExitError {
            code: ExitCode::Warnings,
            message: String::new(),
        }));
    }

    // Clean — exit 0.
    println!("PDF check succeeded");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_rewrite(
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    repair: bool,
    password: &PasswordArgs,
    linearize: bool,
    remove_restrictions: bool,
    normalize_content: bool,
    coalesce_contents: bool,
    remove_unref: CliRemoveUnreferencedResources,
    options: WriteOptions,
) -> CliResult<()> {
    let input = input.ok_or("missing input file")?;
    let output = output.ok_or("missing output file")?;

    // SCOPE BOUNDARY (flpdf-9hc.3.18 vs --decrypt / flpdf-9hc.4.10):
    // `--remove-restrictions` adds NO new decryption logic. A plaintext
    // `rewrite` of an authenticated encrypted input already drops the
    // /Encrypt dictionary (crates/flpdf/src/writer.rs trailer.remove
    // /xref_dict.remove "Encrypt"), and the advisory permission bits live
    // only inside /Encrypt /P, so the rewritten output is inherently
    // unrestricted (Pdf::permissions() on it is None). This flag therefore
    // only makes intent explicit and emits a one-line diagnostic. It never
    // bypasses authentication: `open_pdf` below performs the same auth as a
    // plain `rewrite`, so a wrong/missing password is rejected identically
    // and the diagnostic (printed only after a successful write) is never
    // reached. Full decryption-mode semantics remain out of scope here.
    if linearize {
        let mut pdf = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf)?;
        let plan = LinearizationPlan::from_pdf(&mut pdf)?;
        let renumber = RenumberMap::from_plan(&plan);

        // Re-open the PDF so `write_linearized` can seek/read objects independently.
        let mut pdf2 = open_pdf(&input, repair, password)?;
        reject_encrypted_write(&pdf2)?;
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &options)?;
        doc.back_patch()?;

        std::fs::write(&output, &doc.bytes)?;
        // The linearize branch rejects encrypted input outright via
        // reject_encrypted_write above, so an encrypted (restricted) input
        // never reaches here; on unencrypted input there is nothing to
        // de-restrict. Per qpdf-lenient behaviour the diagnostic is omitted.
    } else {
        let mut pdf = open_pdf(&input, repair, password)?;
        // Capture encryption state BEFORE the write: the plaintext path
        // below drops /Encrypt, so this must be sampled while the in-memory
        // model still reflects the input.
        let was_encrypted = pdf.is_encrypted();
        let mut options = options;
        if was_encrypted {
            options.full_rewrite = true;
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
            || remove_unref != CliRemoveUnreferencedResources::No;
        if needs_mutation {
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

        let mut out = File::create(output)?;
        write_pdf_with_options(&mut pdf, &mut out, &options)?;

        if remove_restrictions && was_encrypted {
            eprintln!("flpdf: removed restrictions (encryption and advisory permissions stripped)");
        }
        // Unencrypted input + --remove-restrictions is a no-op rewrite
        // (exit 0, valid output, no diagnostic) — nothing was restricted,
        // matching qpdf's lenient handling of --remove-restrictions on
        // unencrypted files.
    }
    Ok(())
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
    let page_obj = pdf.resolve(page_ref)?;
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
    let resolved = pdf.resolve(stream_ref)?;
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

    let mut out = File::create(output)?;
    write_qdf(&mut pdf, &mut out)?;
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
    let object = pdf.resolve(object_ref)?;

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
    let object = pdf.resolve(object_ref)?;

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

    let bytes = if cmd.raw {
        stream.data
    } else {
        filters::decode_stream_data(&stream.dict, &stream.data)?
    };

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
    let info = pdf.resolve(info_ref)?;

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
    let catalog = pdf.resolve(catalog_ref)?;
    println!("Catalog: {}", object_to_pdf(&catalog));
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
    let catalog = pdf.resolve(catalog_ref)?;

    let Object::Dictionary(catalog) = catalog else {
        return Err(format!("document catalog {} is not a dictionary", catalog_ref).into());
    };

    match catalog.get_ref("Metadata") {
        Some(metadata_ref) => {
            let metadata = pdf.resolve(metadata_ref)?;
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
                    println!("  type: {}", object_to_pdf(&other));
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
        let page = pdf.resolve(*page_ref)?;
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
    /// authenticate (or the document needs the weak-crypto opt-in to open
    /// at all). qpdf can still report "encrypted" / "password required"
    /// without authenticating, so this is a normal classification here,
    /// not an error.
    EncryptedAuthFailed,
}

/// Open `input`, treating `BadPassword` / `WeakCryptoNotAllowed` as
/// "the file is encrypted but we could not authenticate" rather than a
/// hard error. This mirrors qpdf's ability to answer `--is-encrypted` /
/// `--requires-password` for password-protected files without the password.
fn probe_encryption(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<EncryptionProbe> {
    let file = File::open(input)?;
    match Pdf::open_with_options(BufReader::new(file), pdf_open_options(repair, password)?) {
        Ok(pdf) => Ok(EncryptionProbe::Opened {
            encrypted: pdf.is_encrypted(),
        }),
        // A wrong/empty password or a weak-crypto file we declined to open:
        // the document is definitely encrypted, we just have not (or cannot,
        // without --allow-weak-crypto) authenticate it. qpdf treats both as
        // "encrypted, password required".
        Err(flpdf::Error::Encrypted(
            flpdf::EncryptedError::BadPassword | flpdf::EncryptedError::WeakCryptoNotAllowed,
        )) => Ok(EncryptionProbe::EncryptedAuthFailed),
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
/// (which errors when it cannot derive the key).
fn run_show_encryption_key(
    input: &PathBuf,
    repair: bool,
    password: &PasswordArgs,
) -> CliResult<()> {
    let pdf = open_pdf(input, repair, password)?;
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
/// divergences from qpdf (no recovered cleartext user password).
fn run_show_encryption(input: &PathBuf, repair: bool, password: &PasswordArgs) -> CliResult<()> {
    // qpdf prints "File is not encrypted" and exits 0 for plaintext files.
    // open_pdf succeeds for plaintext input, so detect that case first.
    let mut pdf = open_pdf(input, repair, password)?;
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
    let file = File::open(input)?;
    let pdf = Pdf::open_with_options(BufReader::new(file), pdf_open_options(repair, password)?)
        .map_err(actionable_password_error)?;

    for diagnostic in pdf.repair_diagnostics().entries() {
        eprintln!("warning: {}", diagnostic.message);
    }
    if pdf.uses_weak_crypto() {
        eprintln!(
            "warning: encrypted PDF uses weak crypto; processing because --allow-weak-crypto was supplied"
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
