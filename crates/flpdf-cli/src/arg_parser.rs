use clap::Command;
use std::collections::HashSet;

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

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedArgs {
    pub(crate) residual_args: Vec<String>,
    pub(crate) named_segments: Vec<NamedSegment>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NamedSegment {
    pub(crate) option: String,
    pub(crate) tokens: Vec<String>,
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

    pub(crate) fn parse(&self, args: Vec<String>) -> CliResult<ParsedArgs> {
        let mut iter = args.into_iter();
        let Some(program) = iter.next() else {
            return Err("qpdf argument vector is empty".into());
        };
        let mut residual_args = vec![program];
        let mut named_segments = Vec::new();
        let mut first_add_attachment = true;

        while let Some(arg) = iter.next() {
            if arg == "--" {
                residual_args.push(arg);
                residual_args.extend(iter);
                break;
            }

            let canonical = self.canonical_top_level_option(arg);
            let option = option_name(&canonical);
            let Some(kind) = SegmentKind::from_option(option) else {
                residual_args.push(canonical);
                continue;
            };

            let mut tokens = Vec::new();
            let mut terminated = false;
            for token in iter.by_ref() {
                if token == "--" {
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

            let segment = NamedSegment {
                option: option.to_owned(),
                tokens,
            };
            let retain = kind.retain_in_residual(first_add_attachment);
            if kind == SegmentKind::AddAttachment {
                first_add_attachment = false;
            }
            if retain {
                residual_args.push(format!("--{option}"));
                residual_args.extend(segment.tokens.iter().cloned());
                residual_args.push("--".to_owned());
            }
            named_segments.push(segment);
        }

        Ok(ParsedArgs {
            residual_args,
            named_segments,
        })
    }

    fn canonical_top_level_option(&self, arg: String) -> String {
        if let Some(rest) = arg.strip_prefix("--") {
            let name = rest.split('=').next().unwrap_or(rest);
            if self.bare_long_options.contains(name) && should_discard_bare_value(name, &arg) {
                return format!("--{name}");
            }
            return arg;
        }

        let Some(rest) = arg.strip_prefix('-') else {
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
            format!("--{rest}")
        } else {
            arg
        };
        let name = option_name(&canonical);
        if self.bare_long_options.contains(name) && should_discard_bare_value(name, &canonical) {
            format!("--{name}")
        } else {
            canonical
        }
    }

    fn canonical_segment_option(&self, kind: SegmentKind, token: String) -> String {
        let Some(rest) = token.strip_prefix('-') else {
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
            format!("--{rest}")
        } else {
            token
        }
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

fn option_name(arg: &str) -> &str {
    let Some(rest) = arg.strip_prefix("--") else {
        return "";
    };
    rest.split('=').next().unwrap_or(rest)
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
}
