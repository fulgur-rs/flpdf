use clap::Command;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;

use super::CliResult;

const QPDF_BARE_LONG_OPTIONS: &[&str] = &[
    "add-attachment",
    "allow-weak-crypto",
    "check",
    "check-linearization",
    "coalesce-contents",
    "copy-attachments-from",
    "decrypt",
    "deterministic-id",
    "filtered-stream-data",
    "flatten-rotation",
    "generate-appearances",
    "ignore-xref-streams",
    "is-encrypted",
    "json-input",
    "keep-inline-images",
    "linearize",
    "list-attachments",
    "newline-before-endstream",
    "no-original-object-ids",
    "no-warn",
    "optimize-images",
    "overlay",
    "pages",
    "password-is-hex-key",
    "preserve-unreferenced",
    "preserve-unreferenced-resources",
    "progress",
    "qdf",
    "raw-stream-data",
    "recompress-flate",
    "remove-page-labels",
    "remove-restrictions",
    "replace-input",
    "report-memory-usage",
    "requires-password",
    "set-page-labels",
    "show-linearization",
    "show-npages",
    "show-pages",
    "show-xref",
    "static-aes-iv",
    "static-id",
    "suppress-password-recovery",
    "suppress-recovery",
    "test-json-schema",
    "underlay",
    "verbose",
    "warning-exit-0",
    "empty",
    "with-images",
];

/// qpdf's main option table marks these options as requiring a parameter
/// (`libqpdf/qpdf/auto_job_init.hh:92-126`). `QPDFArgParser::parseArgs`
/// accepts a parameter only when it is attached with `=`; a following argv
/// token remains positional and the parser raises the exact usage error below.
/// Segment tables (`--pages`, `--encrypt`, attachments, overlay/underlay) are
/// parsed separately and therefore never pass through this top-level check.
const QPDF_REQUIRED_PARAMETER_OPTIONS: &[(&str, &str)] = &[
    ("compression-level", "level"),
    ("copy-encryption", "file"),
    ("encryption-file-password", "password"),
    ("force-version", "version"),
    ("ii-min-bytes", "minimum"),
    ("job-json-file", "file"),
    ("json-object", "trailer"),
    ("keep-files-open-threshold", "count"),
    ("linearize-pass1", "filename"),
    ("min-version", "version"),
    ("oi-min-area", "minimum"),
    ("oi-min-height", "minimum"),
    ("oi-min-width", "minimum"),
    ("password", "password"),
    ("password-file", "password"),
    ("remove-attachment", "attachment"),
    ("rotate", "[+|-]angle"),
    ("show-attachment", "attachment"),
    ("show-object", "trailer"),
    ("json-stream-prefix", "stream-file-prefix"),
    ("update-from-json", "qpdf-json file"),
    ("compress-streams", "{n,y}"),
    ("decode-level", "{all,generalized,none,specialized}"),
    ("flatten-annotations", "{all,print,screen}"),
    (
        "json-key",
        "{acroform,attachments,encrypt,objectinfo,objects,outlines,pagelabels,pages,qpdf}",
    ),
    ("json-stream-data", "{file,inline,none}"),
    ("keep-files-open", "{n,y}"),
    ("normalize-content", "{n,y}"),
    ("object-streams", "{disable,generate,preserve}"),
    ("password-mode", "{auto,bytes,hex-bytes,unicode}"),
    ("remove-unreferenced-resources", "{auto,no,yes}"),
    ("stream-data", "{compress,preserve,uncompress}"),
];

/// One argv token with qpdf's raw byte representation and an OS-facing
/// projection. Windows `OsString` cannot represent arbitrary bytes, so the
/// projection is only for clap/path APIs; byte-oriented qpdf values must use
/// [`Self::as_bytes`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawArg {
    bytes: Vec<u8>,
    os: OsString,
}

impl RawArg {
    fn from_os(os: OsString) -> Self {
        let bytes = os_bytes(&os);
        Self { bytes, os }
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        let os = os_string_from_bytes(&bytes);
        Self { bytes, os }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn as_os_str(&self) -> &OsStr {
        &self.os
    }

    pub(crate) fn into_os_string(self) -> OsString {
        self.os
    }
}

impl AsRef<OsStr> for RawArg {
    fn as_ref(&self) -> &OsStr {
        self.as_os_str()
    }
}

impl From<RawArg> for OsString {
    fn from(value: RawArg) -> Self {
        value.into_os_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedArgs {
    pub(crate) residual_args: Vec<OsString>,
    pub(crate) raw_residual_args: Vec<RawArg>,
    pub(crate) named_segments: Vec<NamedSegment>,
    pub(crate) raw_named_segments: Vec<RawNamedSegment>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NamedSegment {
    pub(crate) option: String,
    pub(crate) tokens: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawNamedSegment {
    pub(crate) option: String,
    pub(crate) tokens: Vec<RawArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentKind {
    Encrypt,
    Pages,
    AddAttachment,
    CopyAttachments,
    Overlay,
}

impl SegmentKind {
    fn from_option(option: &str) -> Option<Self> {
        match option {
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

    fn retain_in_residual(self, first_add_attachment: bool) -> bool {
        match self {
            Self::Overlay => false,
            Self::AddAttachment => first_add_attachment,
            Self::Encrypt | Self::Pages | Self::CopyAttachments => true,
        }
    }
}

pub(crate) struct ArgParser {
    known_long_options: HashSet<String>,
    bare_long_options: HashSet<String>,
}

impl ArgParser {
    pub(crate) fn from_command(command: Command) -> Self {
        let mut known_long_options = HashSet::new();
        collect_long_options(&command, &mut known_long_options);
        known_long_options.extend([
            "ignore-xref-streams".to_owned(),
            "object-streams".to_owned(),
            "stream-data".to_owned(),
        ]);

        let bare_long_options = QPDF_BARE_LONG_OPTIONS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();

        Self {
            known_long_options,
            bare_long_options,
        }
    }

    #[cfg(test)]
    pub(crate) fn parse(&self, args: Vec<String>) -> CliResult<ParsedArgs> {
        self.parse_os(args.into_iter().map(OsString::from).collect())
    }

    pub(crate) fn parse_os(&self, args: Vec<OsString>) -> CliResult<ParsedArgs> {
        let args = expand_arg_files(args.into_iter().map(RawArg::from_os).collect())?;
        let mut iter = args.into_iter();
        let Some(program) = iter.next() else {
            return Err("qpdf argument vector is empty".into());
        };
        let mut residual_args = vec![program];
        let mut named_segments = Vec::new();
        let mut first_add_attachment = true;

        while let Some(arg) = iter.next() {
            if arg.as_bytes() == b"--" {
                residual_args.push(arg);
                residual_args.extend(iter);
                break;
            }

            let canonical = self.canonical_top_level_option(arg);
            let Some(option) = option_name(canonical.as_os_str()) else {
                residual_args.push(canonical);
                continue;
            };
            if let Some(parameter_name) = required_parameter_name(&option) {
                if !has_attached_parameter(canonical.as_bytes()) {
                    return Err(
                        format!("--{option} must be given as --{option}={parameter_name}").into(),
                    );
                }
            }
            let Some(kind) = SegmentKind::from_option(&option) else {
                residual_args.push(canonical);
                continue;
            };

            let mut tokens: Vec<RawArg> = Vec::new();
            let mut terminated = false;
            for token in iter.by_ref() {
                if token.as_bytes() == b"--" {
                    terminated = true;
                    break;
                }
                tokens.push(self.canonical_segment_option(kind, token));
            }
            if !terminated {
                let message = if kind == SegmentKind::AddAttachment {
                    format!("--{option}: missing -- terminator")
                } else {
                    format!("--{option}: segment must be terminated by a `--` token")
                };
                return Err(message.into());
            }

            let segment = RawNamedSegment { option, tokens };
            let option = segment.option.as_str();
            let retain = kind.retain_in_residual(first_add_attachment);
            if kind == SegmentKind::AddAttachment {
                first_add_attachment = false;
            }
            if retain {
                residual_args.push(RawArg::from_bytes(format!("--{option}").into_bytes()));
                residual_args.extend(segment.tokens.iter().cloned());
                residual_args.push(RawArg::from_bytes(b"--".to_vec()));
            }
            named_segments.push(RawNamedSegment {
                option: segment.option,
                tokens: segment.tokens,
            });
        }

        let raw_residual_args = residual_args;
        let residual_args = raw_residual_args
            .iter()
            .cloned()
            .map(RawArg::into_os_string)
            .collect();
        let raw_named_segments = named_segments;
        let named_segments = raw_named_segments
            .iter()
            .map(|segment| NamedSegment {
                option: segment.option.clone(),
                tokens: segment
                    .tokens
                    .iter()
                    .cloned()
                    .map(RawArg::into_os_string)
                    .collect(),
            })
            .collect();
        Ok(ParsedArgs {
            residual_args,
            raw_residual_args,
            named_segments,
            raw_named_segments,
        })
    }

    fn canonical_top_level_option(&self, arg: RawArg) -> RawArg {
        let Some(arg_str) = std::str::from_utf8(arg.as_bytes()).ok() else {
            return canonical_top_level_non_utf8_option(
                &self.known_long_options,
                &self.bare_long_options,
                arg,
            );
        };
        if let Some(rest) = arg_str.strip_prefix("--") {
            let name = rest.split('=').next().unwrap_or(rest);
            if self.bare_long_options.contains(name) && should_discard_bare_value(name, arg_str) {
                return RawArg::from_bytes(format!("--{name}").into_bytes());
            }
            return arg;
        }

        let Some(rest) = arg_str.strip_prefix('-') else {
            return arg;
        };
        if rest.is_empty()
            || rest.starts_with('-')
            || rest.starts_with(|c: char| c.is_ascii_digit())
        {
            return arg;
        }

        let name = rest.split('=').next().unwrap_or(rest);
        let canonical = if self.known_long_options.contains(name) {
            RawArg::from_bytes(format!("--{rest}").into_bytes())
        } else {
            arg
        };
        let Some(name) = option_name(canonical.as_os_str()) else {
            return canonical;
        };
        if self.bare_long_options.contains(&name)
            && should_discard_bare_value(
                &name,
                std::str::from_utf8(canonical.as_bytes()).unwrap_or_default(),
            )
        {
            RawArg::from_bytes(format!("--{name}").into_bytes())
        } else {
            canonical
        }
    }

    fn canonical_segment_option(&self, kind: SegmentKind, token: RawArg) -> RawArg {
        let Some(token_str) = std::str::from_utf8(token.as_bytes()).ok() else {
            return canonical_segment_non_utf8_option(kind, token);
        };
        let Some(rest) = token_str.strip_prefix('-') else {
            return token;
        };
        if rest.is_empty()
            || rest.starts_with('-')
            || rest.starts_with(|c: char| c.is_ascii_digit())
        {
            return token;
        }

        let name = rest.split('=').next().unwrap_or(rest);
        if kind.accepts(name) {
            RawArg::from_bytes(format!("--{rest}").into_bytes())
        } else {
            token
        }
    }
}

fn required_parameter_name(name: &str) -> Option<&'static str> {
    QPDF_REQUIRED_PARAMETER_OPTIONS
        .iter()
        .find_map(|(option, parameter)| (*option == name).then_some(*parameter))
}

fn has_attached_parameter(arg: &[u8]) -> bool {
    arg.contains(&b'=')
}

/// Expand qpdf's one-level `@file` argument syntax before option parsing.
///
/// This is the responsibility of qpdf's `QPDFArgParser::handleArgFileArguments`
/// and `readArgsFromFile` (`QPDFArgParser.cc:232-260,347-360`). Each physical
/// line is one argument; the file is not shell-parsed and arguments read from a
/// file are deliberately not scanned for another `@file` reference.
fn expand_arg_files(args: Vec<RawArg>) -> CliResult<Vec<RawArg>> {
    let mut iter = args.into_iter();
    let Some(program) = iter.next() else {
        return Err("qpdf argument vector is empty".into());
    };

    let mut expanded = Vec::new();
    expanded.push(program);
    for arg in iter {
        let Some(path) = argument_file_path(&arg) else {
            expanded.push(arg);
            continue;
        };
        let Some(lines) = read_argument_file(&path)? else {
            // qpdf treats an @path that cannot be opened as an ordinary argv
            // token and lets normal option/positional parsing handle it.
            expanded.push(arg);
            continue;
        };
        expanded.extend(lines);
    }
    Ok(expanded)
}

fn argument_file_path(arg: &RawArg) -> Option<OsString> {
    let bytes = arg.as_bytes();
    if bytes.len() <= 1 || bytes[0] != b'@' {
        return None;
    }
    Some(os_string_from_bytes(&bytes[1..]))
}

/// Read one qpdf argument file, returning `None` when its path cannot be
/// opened. qpdf probes openability first and preserves such a token for the
/// regular parser; an error after a successful open is propagated instead.
fn read_argument_file(path: &OsStr) -> CliResult<Option<Vec<RawArg>>> {
    let bytes = if path == OsStr::new("-") {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        let mut file = match std::fs::File::open(Path::new(path)) {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        bytes
    };

    Ok(Some(
        read_argument_file_lines(&bytes)
            .into_iter()
            .map(RawArg::from_bytes)
            .collect(),
    ))
}

/// Match qpdf's `QUtil::read_lines_from_file(..., preserve_eol=false)`.
fn read_argument_file_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    for &byte in bytes {
        if byte == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(std::mem::take(&mut line));
        } else {
            line.push(byte);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Build only the OS-facing projection for a raw token. The original bytes
/// remain in [`RawArg`] and are never recovered from this projection.
pub(crate) fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        OsString::from_vec(bytes.to_vec())
    }

    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn collect_long_options(command: &Command, names: &mut HashSet<String>) {
    for arg in command.get_arguments() {
        if let Some(long) = arg.get_long() {
            names.insert(long.to_owned());
        }
        if let Some(aliases) = arg.get_all_aliases() {
            names.extend(aliases.into_iter().map(str::to_owned));
        }
    }
    for subcommand in command.get_subcommands() {
        collect_long_options(subcommand, names);
    }
}

fn option_name(arg: &OsStr) -> Option<String> {
    if let Some(arg) = arg.to_str() {
        let rest = arg.strip_prefix("--")?;
        return Some(rest.split('=').next().unwrap_or(rest).to_owned());
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let bytes = arg.as_bytes();
        let rest = bytes.strip_prefix(b"--")?;
        let end = rest
            .iter()
            .position(|byte| *byte == b'=')
            .unwrap_or(rest.len());
        String::from_utf8(rest[..end].to_vec()).ok()
    }

    #[cfg(not(unix))]
    {
        None
    }
}

/// Return argument bytes without a UTF-8 projection on Unix. Windows process
/// arguments are already Unicode and are encoded as UTF-8 for qpdf-shaped
/// byte options.
pub(crate) fn os_bytes(value: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    {
        value.to_string_lossy().into_owned().into_bytes()
    }
}

fn canonical_top_level_non_utf8_option(
    known_long_options: &HashSet<String>,
    bare_long_options: &HashSet<String>,
    arg: RawArg,
) -> RawArg {
    let bytes = arg.as_bytes();
    let (is_double_dash, rest) = if let Some(rest) = bytes.strip_prefix(b"--") {
        if rest.starts_with(b"-") {
            return arg;
        }
        (true, rest)
    } else if let Some(rest) = bytes.strip_prefix(b"-") {
        (false, rest)
    } else {
        return arg;
    };
    if rest.is_empty() || rest[0].is_ascii_digit() {
        return arg;
    }
    let name_end = rest
        .iter()
        .position(|byte| *byte == b'=')
        .unwrap_or(rest.len());
    let Ok(name) = std::str::from_utf8(&rest[..name_end]) else {
        return arg;
    };
    let name = name.to_owned();
    // A single-dash abbreviation only reaches the qpdf-shaped `--name`
    // grammar when the ASCII path (`canonical_top_level_option`) would
    // also promote it: either the argument was already `--name` form, or
    // `known_long_options` recognizes the abbreviated name. Gating the
    // bare-value discard below on that same condition keeps this raw-byte
    // path's decisions identical to the ASCII path for every name;
    // otherwise a name present in `QPDF_BARE_LONG_OPTIONS` but absent
    // from `known_long_options` would have its attached value silently
    // discarded here while the ASCII path leaves the same argument
    // untouched.
    let promoted = is_double_dash || known_long_options.contains(&name);
    let canonical = if is_double_dash {
        arg
    } else if known_long_options.contains(&name) {
        let mut bytes = Vec::with_capacity(rest.len() + 2);
        bytes.extend_from_slice(b"--");
        bytes.extend_from_slice(rest);
        RawArg::from_bytes(bytes)
    } else {
        arg
    };
    if promoted && bare_long_options.contains(&name) {
        let bytes = canonical.as_bytes();
        if let Some(equal_pos) = bytes.iter().position(|byte| *byte == b'=') {
            let value = &bytes[equal_pos + 1..];
            if name != "newline-before-endstream" || !matches!(value, b"y" | b"n" | b"never") {
                return RawArg::from_bytes(format!("--{name}").into_bytes());
            }
        }
    }
    canonical
}

fn canonical_segment_non_utf8_option(kind: SegmentKind, token: RawArg) -> RawArg {
    let bytes = token.as_bytes();
    let Some(rest) = bytes.strip_prefix(b"-") else {
        return token;
    };
    if rest.is_empty() || rest.starts_with(b"-") || rest[0].is_ascii_digit() {
        return token;
    }
    let name_end = rest
        .iter()
        .position(|byte| *byte == b'=')
        .unwrap_or(rest.len());
    let Ok(name) = std::str::from_utf8(&rest[..name_end]) else {
        return token;
    };
    if kind.accepts(name) {
        let mut bytes = Vec::with_capacity(rest.len() + 2);
        bytes.extend_from_slice(b"--");
        bytes.extend_from_slice(rest);
        RawArg::from_bytes(bytes)
    } else {
        token
    }
}

fn should_discard_bare_value(name: &str, arg: &str) -> bool {
    let Some((_, value)) = arg.split_once('=') else {
        return false;
    };
    if name == "newline-before-endstream" {
        !matches!(value, "y" | "n" | "never")
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_returns_canonical_residual_args_and_raw_segments() {
        let command = clap::Command::new("flpdf").arg(clap::Arg::new("qdf").long("qdf"));
        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), "-qdf".into(), "input.pdf".into()])
            .expect("qpdf argv should parse");

        assert_eq!(parsed.residual_args, ["flpdf", "--qdf", "input.pdf"]);
        assert!(parsed.named_segments.is_empty());
    }

    #[test]
    fn parser_expands_qpdf_argument_file_before_parsing() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("args");
        std::fs::write(&path, b"--qdf\ninput.pdf\n").expect("write argument file");
        let command = clap::Command::new("flpdf").arg(clap::Arg::new("qdf").long("qdf"));

        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), format!("@{}", path.display())])
            .expect("qpdf argument file should be expanded");

        assert_eq!(parsed.residual_args, ["flpdf", "--qdf", "input.pdf"]);
    }

    #[test]
    fn parser_preserves_qpdf_argument_file_line_boundaries_without_recursion() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("args");
        std::fs::write(&path, b"--qdf\r\n\n@nested\r\nplain argument\r\n")
            .expect("write argument file");
        let command = clap::Command::new("flpdf").arg(clap::Arg::new("qdf").long("qdf"));

        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), format!("@{}", path.display())])
            .expect("qpdf argument file should be expanded");

        assert_eq!(
            parsed.residual_args,
            ["flpdf", "--qdf", "", "@nested", "plain argument"]
        );
    }

    #[test]
    fn parser_keeps_an_unavailable_argument_file_as_an_original_token() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let missing = format!("@{}", directory.path().join("missing").display());
        let command = clap::Command::new("flpdf");

        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), missing.clone()])
            .expect("unavailable argument file should remain a positional token");

        assert_eq!(
            parsed.residual_args,
            vec![OsString::from("flpdf"), OsString::from(missing)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn parser_preserves_non_utf8_argument_file_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("args");
        std::fs::write(&path, b"input-\xff.pdf\n").expect("write argument file");
        let command = clap::Command::new("flpdf");
        let argfile = OsString::from_vec(format!("@{}", path.display()).into_bytes());

        let parsed = ArgParser::from_command(command)
            .parse_os(vec![OsString::from("flpdf"), argfile])
            .expect("qpdf argument file should be expanded");

        assert_eq!(
            parsed.residual_args,
            vec![
                OsString::from("flpdf"),
                OsString::from_vec(b"input-\xff.pdf".to_vec())
            ]
        );
    }

    #[test]
    fn parser_preserves_non_utf8_argument_file_bytes_on_every_platform() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("args");
        std::fs::write(&path, b"input-\xff.pdf\n").expect("write argument file");
        let command = clap::Command::new("flpdf");

        let parsed = ArgParser::from_command(command)
            .parse_os(vec![
                OsString::from("flpdf"),
                OsString::from(format!("@{}", path.display())),
            ])
            .expect("qpdf argument file should be expanded");

        assert_eq!(parsed.raw_residual_args[1].as_bytes(), b"input-\xff.pdf");
    }

    #[test]
    fn parser_preserves_non_utf8_password_bytes_inside_an_encrypt_segment() {
        let directory = tempfile::tempdir().expect("create argument-file directory");
        let path = directory.path().join("args");
        std::fs::write(&path, b"--encrypt\nuser-\xff\nowner\n128\n--\n")
            .expect("write argument file");
        let command = clap::Command::new("flpdf");

        let parsed = ArgParser::from_command(command)
            .parse_os(vec![
                OsString::from("flpdf"),
                OsString::from(format!("@{}", path.display())),
            ])
            .expect("qpdf argument file should be expanded");

        assert_eq!(
            parsed.raw_named_segments[0].tokens[0].as_bytes(),
            b"user-\xff"
        );
    }

    #[test]
    fn parser_captures_raw_overlay_segment_without_feature_validation() {
        let command = clap::Command::new("flpdf");
        let parsed = ArgParser::from_command(command)
            .parse(vec![
                "flpdf".into(),
                "--overlay".into(),
                "source.pdf".into(),
                "--to=not-a-page-range".into(),
                "--".into(),
            ])
            .expect("grammar parser should not validate overlay semantics");

        assert_eq!(parsed.residual_args, ["flpdf"]);
        assert_eq!(parsed.named_segments.len(), 1);
        assert_eq!(parsed.named_segments[0].option, "overlay");
        assert_eq!(
            parsed.named_segments[0].tokens,
            ["source.pdf", "--to=not-a-page-range"]
        );
    }

    #[test]
    fn parser_discards_attached_value_for_bare_option() {
        let command = clap::Command::new("flpdf").arg(
            clap::Arg::new("check")
                .long("check")
                .action(clap::ArgAction::SetTrue),
        );
        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), "--check=ignored".into()])
            .expect("bare option should accept qpdf's attached value form");

        assert_eq!(parsed.residual_args, ["flpdf", "--check"]);
    }

    #[test]
    fn parser_discards_attached_value_for_check_linearization() {
        let command = clap::Command::new("flpdf").arg(
            clap::Arg::new("check-linearization")
                .long("check-linearization")
                .action(clap::ArgAction::SetTrue),
        );
        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), "--check-linearization=ignored".into()])
            .expect("bare option should accept qpdf's attached value form");

        assert_eq!(parsed.residual_args, ["flpdf", "--check-linearization"]);
    }

    #[test]
    fn parser_captures_pages_segment_and_resumes_after_terminator() {
        let command = clap::Command::new("flpdf").arg(clap::Arg::new("qdf").long("qdf"));
        let parsed = ArgParser::from_command(command)
            .parse(vec![
                "flpdf".into(),
                "--pages".into(),
                "source.pdf".into(),
                "--".into(),
                "-qdf".into(),
            ])
            .expect("pages segment should parse");

        assert_eq!(
            parsed.residual_args,
            ["flpdf", "--pages", "source.pdf", "--", "--qdf"]
        );
        assert_eq!(parsed.named_segments[0].option, "pages");
        assert_eq!(parsed.named_segments[0].tokens, ["source.pdf"]);
    }

    #[test]
    fn parser_normalizes_segment_local_single_dash_suboption() {
        let command = clap::Command::new("flpdf");
        let parsed = ArgParser::from_command(command)
            .parse(vec![
                "flpdf".into(),
                "--overlay".into(),
                "source.pdf".into(),
                "-to=1".into(),
                "--".into(),
            ])
            .expect("overlay segment should parse");

        assert_eq!(parsed.named_segments[0].tokens, ["source.pdf", "--to=1"]);
    }

    #[cfg(unix)]
    #[test]
    fn parser_keeps_a_non_utf8_residual_argument_byte_for_byte() {
        use std::os::unix::ffi::OsStringExt;

        let command = clap::Command::new("flpdf");
        let input = OsString::from_vec(b"input-\xff.pdf".to_vec());
        let parsed = ArgParser::from_command(command)
            .parse_os(vec![OsString::from("flpdf"), input.clone()])
            .expect("raw argv should remain a residual positional");

        assert_eq!(parsed.residual_args[1], input);
    }

    #[cfg(unix)]
    #[test]
    fn parser_discards_a_non_utf8_attached_value_for_a_qpdf_bare_option() {
        use std::os::unix::ffi::OsStringExt;

        let command = clap::Command::new("flpdf").arg(
            clap::Arg::new("check")
                .long("check")
                .action(clap::ArgAction::SetTrue),
        );
        let parsed = ArgParser::from_command(command)
            .parse_os(vec![
                OsString::from("flpdf"),
                OsString::from_vec(b"--check=\xff".to_vec()),
            ])
            .expect("raw bare option should be normalized");

        assert_eq!(
            parsed.residual_args,
            [OsString::from("flpdf"), OsString::from("--check")]
        );
    }

    #[test]
    fn parser_leaves_a_single_dash_attached_value_unchanged_for_a_bare_option_not_registered_with_clap(
    ) {
        // "warning-exit-0" is in QPDF_BARE_LONG_OPTIONS but is not registered
        // as a clap long flag anywhere in the real CLI, so it is absent from
        // `known_long_options`: the single-dash abbreviation is never
        // promoted to `--warning-exit-0`, and `option_name` on the
        // unpromoted single-dash argument returns None, so
        // canonical_top_level_option returns the argument untouched.
        let command = clap::Command::new("flpdf");
        let parsed = ArgParser::from_command(command)
            .parse(vec!["flpdf".into(), "-warning-exit-0=1".into()])
            .expect("unrecognized single-dash option should pass through unchanged");

        assert_eq!(parsed.residual_args, ["flpdf", "-warning-exit-0=1"]);
    }

    #[cfg(unix)]
    #[test]
    fn parser_matches_the_ascii_path_for_a_non_utf8_attached_value_on_a_bare_option_not_registered_with_clap(
    ) {
        use std::os::unix::ffi::OsStringExt;

        // Same option as
        // parser_leaves_a_single_dash_attached_value_unchanged_for_a_bare_option_not_registered_with_clap,
        // but with a non-UTF-8 attached value so the raw-byte
        // canonical_top_level_non_utf8_option path runs instead of
        // canonical_top_level_option. Before this fix, that path applied the
        // bare-value discard unconditionally on bare_long_options
        // membership, without also requiring the promotion that
        // known_long_options gates in the ASCII path -- collapsing this
        // argument to bare `--warning-exit-0` even though the equivalent
        // ASCII-valued argument above passes through untouched.
        let command = clap::Command::new("flpdf");
        let input = OsString::from_vec(b"-warning-exit-0=\xff".to_vec());
        let parsed = ArgParser::from_command(command)
            .parse_os(vec![OsString::from("flpdf"), input.clone()])
            .expect("unrecognized single-dash option should pass through unchanged");

        assert_eq!(parsed.residual_args, [OsString::from("flpdf"), input]);
    }
}
