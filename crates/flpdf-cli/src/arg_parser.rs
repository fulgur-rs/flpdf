use clap::Command;
use std::collections::HashSet;

use super::CliResult;

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

pub(crate) struct ArgParser {
    known_long_options: HashSet<String>,
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
        Self {
            known_long_options,
        }
    }

    pub(crate) fn parse(&self, args: Vec<String>) -> CliResult<ParsedArgs> {
        let mut iter = args.into_iter();
        let Some(program) = iter.next() else {
            return Err("qpdf argument vector is empty".into());
        };
        let mut residual_args = vec![program];
        let mut named_segments = Vec::new();

        while let Some(arg) = iter.next() {
            if arg == "--" {
                residual_args.push(arg);
                residual_args.extend(iter);
                break;
            }

            if matches!(arg.as_str(), "--overlay" | "--underlay") {
                let option = arg.trim_start_matches('-').to_owned();
                let mut tokens = Vec::new();
                let mut terminated = false;
                for token in iter.by_ref() {
                    if token == "--" {
                        terminated = true;
                        break;
                    }
                    tokens.push(token);
                }
                if !terminated {
                    return Err(format!("--{option}: segment must be terminated by --").into());
                }
                named_segments.push(NamedSegment { option, tokens });
                continue;
            }

            residual_args.push(self.canonical_long_option(arg));
        }

        Ok(ParsedArgs {
            residual_args,
            named_segments,
        })
    }

    fn canonical_long_option(&self, arg: String) -> String {
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
        if self.known_long_options.contains(name) {
            format!("--{rest}")
        } else {
            arg
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_returns_canonical_residual_args_and_raw_segments() {
        let command = clap::Command::new("flpdf")
            .arg(clap::Arg::new("qdf").long("qdf"));
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
}

