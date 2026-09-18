//! qpdf 11.9.0's raw `QPDFJob::initializeFromArgv` boundary.
//!
//! qpdf correspondence: `QPDFArgParser.cc:429-566`, `QPDFJob_argv.cc`, `QPDFJob_config.cc`, and `qpdf/auto_job_init.hh`.
//! The parser lives below `job::lifecycle` so it can
//! mutate the one `JobConfiguration` owned by [`super::QPDFJob`]; it does not
//! create a CLI-specific configuration copy.

use super::*;
use crate::encryption::{EncryptMethod, PasswordMode};
use crate::job::page_range::PageRange;
use crate::job::OverlayKind;
use crate::{EncryptParams, Error, PrintPermission, Result, UsageError};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Initialize one job from the already-byte-preserved argv vector.
pub(super) fn initialize(job: &mut QPDFJob, argv: Vec<Vec<u8>>) -> Result<()> {
    let expanded = expand_arg_files(argv)?;

    job.configuration = qpdf_default_job_configuration();
    // qpdf's Config defaults `require_outfile` to true for the argv boundary;
    // inspection selectors and JSON output options turn it off explicitly
    // (`QPDFJob_config.cc:75-83, QPDFJob.cc:591-595`).
    job.configuration.require_output = true;
    job.partial_json_initialized = false;
    // A fresh argv initialization restarts the job's lifecycle. Leaving
    // `has_run` set would send a later `--job-json-file` occurrence down
    // `initialize_from_json_with_partial`'s post-run fresh-configuration
    // branch, discarding the argv options parsed before it.
    job.reset_has_run_for_initialization();
    job.argv_early_exit = false;
    // qpdf sets the diagnostic prefix from `QPDFArgParser::getProgname()`
    // immediately before parsing (`QPDFJob_argv.cc:418-428`). That value is
    // the basename of argv[0], independent of the completion executable
    // override.
    let program = expanded
        .first()
        .map_or_else(Vec::new, |argv0| program_name_bytes(argv0).to_vec());
    job.set_message_prefix_bytes(program);

    if expanded.len() == 2 && handle_sole_help_option(job, &expanded[0], &expanded[1])? {
        return Ok(());
    }

    let mut parser = Parser::new(job);
    parser.parse(&expanded)?;
    parser.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Main,
    Pages,
    Encryption,
    UnderlayOverlay,
    Attachment,
    CopyAttachment,
    PageLabels,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Self::Main => "main", // cov:ignore: an active segment always selects a non-main table
            Self::Pages => "pages",
            Self::Encryption => "encryption", // cov:ignore: encryption errors use EncryptionState::table_name
            Self::UnderlayOverlay => "underlay/overlay",
            Self::Attachment => "attachment",
            Self::CopyAttachment => "copy attachment",
            Self::PageLabels => "set page labels",
        }
    }
}

struct Parser<'a> {
    job: &'a mut QPDFJob,
    table: Table,
    active: Option<ActiveSegment>,
    gave_input: bool,
    gave_output: bool,
}

enum ActiveSegment {
    Pages(PagesState),
    Encryption(EncryptionState),
    UnderlayOverlay(UnderlayOverlayState),
    Attachment(AttachmentState),
    CopyAttachment(CopyAttachmentState),
    PageLabels(Vec<Vec<u8>>),
}

impl<'a> Parser<'a> {
    fn new(job: &'a mut QPDFJob) -> Self {
        let gave_input = job.configuration.input_file.is_some() || job.configuration.empty_input;
        let gave_output =
            job.configuration.output_file.is_some() || job.configuration.replace_input;
        Self {
            job,
            table: Table::Main,
            active: None,
            gave_input,
            gave_output,
        }
    }

    fn parse(&mut self, argv: &[Vec<u8>]) -> Result<()> {
        let mut index = 1;
        while index < argv.len() {
            let argument = &argv[index];
            if self.active.is_some() {
                self.parse_segment_argument(argument)?;
            } else {
                self.parse_main_argument(argument)?;
            }
            index += 1;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.active.is_some() {
            return Err(UsageError::new(format!(
                "missing -- at end of {} options",
                self.active_table_name()
            ))
            .into());
        }
        if self.job.configuration.input_file.is_none() && !self.job.configuration.empty_input {
            return Err(UsageError::new("an input file name is required").into());
        }
        self.job.check_configuration()?;
        let input_name = self
            .job
            .configuration
            .input_file
            .as_ref()
            .map_or_else(Vec::new, |path| path_description_bytes(path));
        self.job.set_input_name_bytes(input_name);
        self.job.warnings = false;
        Ok(())
    }

    fn active_table_name(&self) -> &'static str {
        match self.active.as_ref() {
            Some(ActiveSegment::Encryption(state)) => state.table_name(),
            _ => self.table.name(),
        }
    }

    fn parse_main_argument(&mut self, argument: &[u8]) -> Result<()> {
        if argument == b"--" {
            // qpdf's top-level marker is an option-table reset. We are already
            // in the main table, so consume it and continue.
            return Ok(());
        }
        let Some((name, value)) = option_parts(argument) else {
            if argument.first() == Some(&b'-') && argument != b"-" {
                return Err(unrecognized(argument).into());
            }
            return self.positional(argument);
        };

        match name {
            b"add-attachment" => self.begin_segment(
                Table::Attachment,
                ActiveSegment::Attachment(AttachmentState::default()),
            ),
            b"allow-weak-crypto" => {
                self.job.configuration.allow_weak_crypto = true;
                Ok(())
            }
            b"check" => {
                self.job.configuration.check = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"check-linearization" => {
                self.job.configuration.check_linearization = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"coalesce-contents" => {
                self.job.configuration.coalesce_contents = true;
                Ok(())
            }
            b"copy-attachments-from" => self.begin_segment(
                Table::CopyAttachment,
                ActiveSegment::CopyAttachment(CopyAttachmentState::default()),
            ),
            b"decrypt" => {
                self.job.configuration.writer.set_preserve_encryption(false);
                self.job.configuration.writer.clear_encryption_parameters();
                self.job.configuration.copy_encryption_applies_to_writer = false;
                Ok(())
            }
            b"deterministic-id" => {
                self.job.configuration.writer.set_deterministic_id(true);
                Ok(())
            }
            b"empty" => self.select_empty_input(),
            b"encrypt" => {
                let inherited = self.job.configuration.encryption_defaults.clone();
                self.begin_segment(
                    Table::Encryption,
                    ActiveSegment::Encryption(EncryptionState::new(inherited)),
                )
            }
            b"externalize-inline-images" => {
                self.job.configuration.externalize_inline_images = true;
                Ok(())
            }
            b"filtered-stream-data" => {
                self.job.configuration.show_filtered_stream_data = true;
                Ok(())
            }
            b"flatten-rotation" => {
                self.job.configuration.flatten_rotation = true;
                Ok(())
            }
            b"generate-appearances" => {
                self.job.configuration.generate_appearances = true;
                Ok(())
            }
            b"ignore-xref-streams" => {
                self.job.configuration.ignore_xref_streams = true;
                Ok(())
            }
            b"is-encrypted" => {
                self.job.configuration.is_encrypted = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"json-input" => {
                self.job.configuration.json_input = true;
                Ok(())
            }
            b"keep-inline-images" => {
                self.job.configuration.image_options.keep_inline_images = true;
                Ok(())
            }
            b"linearize" => {
                self.job.configuration.linearize = true;
                Ok(())
            }
            b"list-attachments" => {
                self.job.configuration.list_attachments = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"newline-before-endstream" => {
                self.job
                    .configuration
                    .writer
                    .set_newline_before_endstream(true);
                Ok(())
            }
            b"no-original-object-ids" => {
                self.job
                    .configuration
                    .writer
                    .set_suppress_original_object_ids(true);
                Ok(())
            }
            b"no-warn" => {
                self.job.suppress_warnings = true;
                Ok(())
            }
            b"optimize-images" => {
                self.job.configuration.optimize_images = true;
                Ok(())
            }
            b"overlay" => self.begin_segment(
                Table::UnderlayOverlay,
                ActiveSegment::UnderlayOverlay(UnderlayOverlayState::new(OverlayKind::Overlay)),
            ),
            b"pages" => {
                self.begin_segment(Table::Pages, ActiveSegment::Pages(PagesState::default()))
            }
            b"password-is-hex-key" => {
                self.job.configuration.password_is_hex_key = true;
                Ok(())
            }
            b"preserve-unreferenced" => {
                self.job
                    .configuration
                    .writer
                    .set_preserve_unreferenced_objects(true);
                Ok(())
            }
            b"preserve-unreferenced-resources" => {
                self.job.configuration.remove_unreferenced_resources =
                    RemoveUnreferencedResources::No;
                Ok(())
            }
            b"progress" => {
                self.job.configuration.progress = true;
                Ok(())
            }
            b"qdf" => {
                self.job.configuration.writer.set_qdf_mode(true);
                Ok(())
            }
            b"raw-stream-data" => {
                self.job.configuration.show_raw_stream_data = true;
                Ok(())
            }
            b"recompress-flate" => {
                self.job.configuration.writer.set_recompress_flate(true);
                Ok(())
            }
            b"remove-page-labels" => {
                self.job.configuration.remove_page_labels = true;
                Ok(())
            }
            b"replace-input" => self.select_replace_input(),
            b"report-memory-usage" => {
                self.job.configuration.report_memory_usage = true;
                Ok(())
            }
            b"requires-password" => {
                self.job.configuration.requires_password = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"remove-restrictions" => {
                self.job.configuration.remove_restrictions = true;
                Ok(())
            }
            b"set-page-labels" => {
                self.begin_segment(Table::PageLabels, ActiveSegment::PageLabels(Vec::new()))
            }
            b"show-encryption" => {
                self.job.configuration.show_encryption = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"show-encryption-key" => {
                self.job.configuration.show_encryption_key = true;
                Ok(())
            }
            b"show-linearization" => {
                self.job.configuration.show_linearization = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"show-npages" => {
                self.job.configuration.show_npages = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"show-pages" => {
                self.job.configuration.show_pages = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"show-xref" => {
                self.job.configuration.show_xref = true;
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"static-aes-iv" => {
                self.job.configuration.writer.set_static_aes_iv(true);
                Ok(())
            }
            b"static-id" => {
                self.job.configuration.writer.set_static_id(true);
                Ok(())
            }
            b"suppress-password-recovery" => {
                self.job.configuration.suppress_password_recovery = true;
                Ok(())
            }
            b"suppress-recovery" => {
                self.job.configuration.suppress_recovery = true;
                Ok(())
            }
            b"test-json-schema" => {
                self.job.configuration.test_json_schema = true;
                Ok(())
            }
            b"underlay" => self.begin_segment(
                Table::UnderlayOverlay,
                ActiveSegment::UnderlayOverlay(UnderlayOverlayState::new(OverlayKind::Underlay)),
            ),
            b"verbose" => {
                self.job.configuration.verbose = true;
                Ok(())
            }
            b"warning-exit-0" => {
                self.job.warnings_exit_zero = true;
                Ok(())
            }
            b"with-images" => {
                self.job.configuration.show_page_images = true;
                Ok(())
            }
            b"compression-level" => {
                let value = required_value(name, value, "level")?;
                self.job
                    .configuration
                    .writer
                    .set_compression_level(argv_callback_value(parse_job_compression_level(
                        value,
                    ))?);
                Ok(())
            }
            b"copy-encryption" => {
                let value = required_value(name, value, "file")?;
                self.job.configuration.copy_encryption = Some(path_from_bytes(value));
                self.job.configuration.copy_encryption_applies_to_writer = true;
                self.job.configuration.writer.clear_encryption_parameters();
                Ok(())
            }
            b"encryption-file-password" => {
                let value = required_value(name, value, "password")?;
                self.job.configuration.encryption_file_password = value.to_vec();
                Ok(())
            }
            b"force-version" => {
                let value = required_value(name, value, "version")?;
                let (version, extension) = parse_job_version(value, ".forceVersion")?;
                self.job
                    .configuration
                    .writer
                    .force_pdf_version(version, extension);
                Ok(())
            }
            b"ii-min-bytes" => {
                let value = required_value(name, value, "minimum")?;
                self.job.configuration.image_options.inline_min_bytes =
                    argv_callback_value(parse_qpdf_collate_uint(value))?;
                Ok(())
            }
            b"job-json-file" => {
                let value = required_value(name, value, "file")?;
                self.apply_job_json_file(value)
            }
            b"json-object" => {
                let value = required_value(name, value, "trailer")?;
                self.job
                    .configuration
                    .json_objects
                    .push(String::from_utf8_lossy(value).into_owned());
                Ok(())
            }
            b"keep-files-open-threshold" => {
                let value = required_value(name, value, "count")?;
                self.job.configuration.keep_files_open_threshold =
                    Some(argv_callback_value(parse_qpdf_collate_uint(value))?);
                Ok(())
            }
            b"linearize-pass1" => {
                let value = required_value(name, value, "filename")?;
                self.job.configuration.linearize_pass1 = Some(path_from_bytes(value));
                Ok(())
            }
            b"min-version" => {
                let value = required_value(name, value, "version")?;
                let (version, extension) = parse_job_version(value, ".minVersion")?;
                self.job
                    .configuration
                    .writer
                    .set_minimum_pdf_version(version, extension);
                Ok(())
            }
            b"oi-min-area" => {
                let value = required_value(name, value, "minimum")?;
                self.job.configuration.image_options.min_area =
                    argv_callback_value(parse_qpdf_collate_uint(value))? as u32;
                Ok(())
            }
            b"oi-min-height" => {
                let value = required_value(name, value, "minimum")?;
                self.job.configuration.image_options.min_height =
                    argv_callback_value(parse_qpdf_collate_uint(value))? as u32;
                Ok(())
            }
            b"oi-min-width" => {
                let value = required_value(name, value, "minimum")?;
                self.job.configuration.image_options.min_width =
                    argv_callback_value(parse_qpdf_collate_uint(value))? as u32;
                Ok(())
            }
            b"password" => {
                let value = required_value(name, value, "password")?;
                self.job.configuration.password = value.to_vec();
                Ok(())
            }
            b"password-file" => {
                let value = required_value(name, value, "password")?;
                // qpdf's `Config::passwordFile` lets `read_lines_from_file`'s
                // QPDFSystemError escape into `ArgParser::parseOptions`, which
                // converts every callback runtime_error through `usage()`
                // (`QPDFJob_config.cc:661-668`; `QPDFJob_argv.cc:407-415`).
                if let Some(password) = argv_callback_value(read_password_file(self.job, value))? {
                    self.job.configuration.password = password;
                }
                Ok(())
            }
            b"remove-attachment" => {
                let value = required_value(name, value, "attachment")?;
                self.job
                    .configuration
                    .attachments_to_remove
                    .push(value.to_vec());
                Ok(())
            }
            b"rotate" => {
                let value = required_value(name, value, "[+|-]angle")?;
                let rotation = argv_callback_value(parse_rotation_parameter(value))?;
                self.job
                    .configuration
                    .rotations
                    .insert(rotation.range, rotation.spec);
                Ok(())
            }
            b"show-attachment" => {
                let value = required_value(name, value, "attachment")?;
                self.job.configuration.show_attachment = Some(value.to_vec());
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"show-object" => {
                let value = required_value(name, value, "trailer")?;
                self.job.configuration.show_object =
                    Some(argv_callback_value(parse_job_object_selector(value))?);
                self.job.configuration.require_output = false;
                Ok(())
            }
            b"json-stream-prefix" => {
                let value = required_value(name, value, "stream-file-prefix")?;
                self.job.configuration.json_stream_prefix = Some(value.to_vec());
                Ok(())
            }
            b"update-from-json" => {
                let value = required_value(name, value, "qpdf-json file")?;
                self.job.configuration.update_from_json = Some(path_from_bytes(value));
                Ok(())
            }
            b"collate" => {
                let value = value.unwrap_or_default();
                self.job
                    .configuration
                    .collate
                    .get_or_insert_with(Vec::new)
                    .extend(argv_callback_value(parse_qpdf_collate_parameter(value))?);
                Ok(())
            }
            b"split-pages" => {
                let value = value.unwrap_or_default();
                self.job.configuration.split_pages =
                    Some(argv_callback_value(parse_job_split_pages(value))?);
                Ok(())
            }
            b"compress-streams" => {
                let value = required_choice(name, value, &[b"y", b"n"])?;
                self.job
                    .configuration
                    .writer
                    .set_compress_streams(value == b"y");
                Ok(())
            }
            b"decode-level" => {
                let value = required_choice(
                    name,
                    value,
                    &[b"none", b"generalized", b"specialized", b"all"],
                )?; // cov:ignore: LLVM maps the covered decode-level choice continuation to the call setup
                let level = match value {
                    b"none" => crate::writer::DecodeLevel::None,
                    b"generalized" => crate::writer::DecodeLevel::Generalized,
                    b"specialized" => crate::writer::DecodeLevel::Specialized,
                    b"all" => crate::writer::DecodeLevel::All,
                    _ => unreachable!(), // cov:ignore: required_choice validates every decode-level value
                };
                self.job.configuration.writer.set_decode_level(level);
                self.job.configuration.json_decode_level = level;
                self.job.configuration.json_decode_level_set = true;
                Ok(())
            }
            b"flatten-annotations" => {
                let value = required_choice(name, value, &[b"all", b"print", b"screen"])?;
                self.job.configuration.flatten_annotations = Some(match value {
                    b"all" => FlattenAnnotationsMode::All,
                    b"print" => FlattenAnnotationsMode::Print,
                    b"screen" => FlattenAnnotationsMode::Screen,
                    _ => unreachable!(), // cov:ignore: required_choice validates every flatten-annotations value
                });
                Ok(())
            }
            b"json-key" => {
                let value = required_choice(
                    name,
                    value,
                    &[
                        b"acroform",
                        b"attachments",
                        b"encrypt",
                        b"objectinfo",
                        b"objects",
                        b"outlines",
                        b"pagelabels",
                        b"pages",
                        b"qpdf",
                    ],
                )?; // cov:ignore: LLVM maps the covered json-key choice continuation to the call setup
                let value = std::str::from_utf8(value)
                    .map_err(|_| UsageError::new("--json-key must be given as --json-key={...}"))?;
                self.job.configuration.json_keys.push(
                    JsonKey::from_str(value)
                        .ok_or_else(|| UsageError::new("invalid json-key option"))?,
                );
                Ok(())
            }
            b"json-stream-data" => {
                let value = required_choice(name, value, &[b"none", b"inline", b"file"])?;
                self.job.configuration.json_stream_data = match value {
                    b"none" => JsonStreamData::None,
                    b"inline" => JsonStreamData::Inline,
                    b"file" => JsonStreamData::File,
                    _ => unreachable!(), // cov:ignore: required_choice validates every json-stream-data value
                };
                self.job.configuration.json_stream_data_set = true;
                Ok(())
            }
            b"keep-files-open" => {
                let value = required_choice(name, value, &[b"y", b"n"])?;
                self.job.configuration.keep_files_open = Some(value == b"y");
                Ok(())
            }
            b"normalize-content" => {
                let value = required_choice(name, value, &[b"y", b"n"])?;
                self.job.configuration.normalize_content = Some(value == b"y");
                Ok(())
            }
            b"object-streams" => {
                let value = required_choice(name, value, &[b"disable", b"preserve", b"generate"])?;
                self.job
                    .configuration
                    .writer
                    .set_object_stream_mode(parse_object_stream_mode(
                        std::str::from_utf8(value).unwrap(),
                    )?); // cov:ignore: required_choice validates UTF-8 object-streams values
                Ok(())
            }
            b"password-mode" => {
                let value =
                    required_choice(name, value, &[b"bytes", b"hex-bytes", b"unicode", b"auto"])?;
                self.job.configuration.password_mode = match value {
                    b"bytes" => PasswordMode::Bytes,
                    b"hex-bytes" => PasswordMode::HexBytes,
                    b"unicode" => PasswordMode::Unicode,
                    b"auto" => PasswordMode::Auto,
                    _ => unreachable!(), // cov:ignore: required_choice validates every password-mode value
                };
                Ok(())
            }
            b"remove-unreferenced-resources" => {
                let value = required_choice(name, value, &[b"auto", b"yes", b"no"])?;
                self.job.configuration.remove_unreferenced_resources = match value {
                    b"auto" => RemoveUnreferencedResources::Auto,
                    b"yes" => RemoveUnreferencedResources::Yes,
                    b"no" => RemoveUnreferencedResources::No,
                    _ => unreachable!(), // cov:ignore: required_choice validates every resource-removal value
                };
                Ok(())
            }
            b"stream-data" => {
                let value =
                    required_choice(name, value, &[b"compress", b"preserve", b"uncompress"])?;
                self.job
                    .configuration
                    .writer
                    .set_stream_data_mode(match value {
                        b"compress" => crate::StreamDataMode::Compress,
                        b"preserve" => crate::StreamDataMode::Preserve,
                        b"uncompress" => crate::StreamDataMode::Uncompress,
                        _ => unreachable!(), // cov:ignore: required_choice validates every stream-data value
                    });
                Ok(())
            }
            b"json" => self.apply_json_output(value, false),
            b"json-output" => self.apply_json_output(value, true),
            _ => Err(unrecognized(argument).into()),
        }
    }

    fn begin_segment(&mut self, table: Table, segment: ActiveSegment) -> Result<()> {
        if self.active.is_some() {
            return Err(Error::Internal("nested qpdf option segment".into())); // cov:ignore: parse_main_argument runs only while no segment is active
        }
        if table == Table::Pages
            && self.job.configuration.page_specs_origin != PageSpecsOrigin::None
        {
            return Err(UsageError::new("--pages may only be specified one time").into());
        }
        self.table = table;
        self.active = Some(segment);
        Ok(())
    }

    fn parse_segment_argument(&mut self, argument: &[u8]) -> Result<()> {
        if argument == b"--" {
            let segment = self.active.take().expect("active segment checked above");
            self.finish_segment(segment)?;
            self.table = Table::Main;
            return Ok(());
        }
        let segment = self.active.as_mut().expect("active segment checked above");
        match segment {
            ActiveSegment::Pages(state) => state.push(argument),
            ActiveSegment::Encryption(state) => state.push(argument),
            ActiveSegment::UnderlayOverlay(state) => state.push(argument),
            ActiveSegment::Attachment(state) => state.push(argument),
            ActiveSegment::CopyAttachment(state) => state.push(argument),
            ActiveSegment::PageLabels(specs) => {
                if argument.first() == Some(&b'-') {
                    let mut message = b"unrecognized argument ".to_vec();
                    message.extend_from_slice(argument);
                    message.extend_from_slice(
                        b" (set page labels options must be terminated with --)",
                    );
                    Err(UsageError::new(message).into())
                } else {
                    specs.push(argument.to_vec());
                    Ok(())
                }
            }
        }
    }

    fn finish_segment(&mut self, segment: ActiveSegment) -> Result<()> {
        match segment {
            ActiveSegment::Pages(state) => {
                let specs = state.finish()?;
                self.job.configuration.page_specs_origin = PageSpecsOrigin::Config;
                self.job.configuration.page_specs = specs
                    .into_iter()
                    .map(|spec| {
                        let range = if spec.range.is_empty() {
                            PageRange::all()
                        } else {
                            PageRange::parse_numrange(&spec.range)
                                .map_err(|error| UsageError::new(error.to_string()))?
                        };
                        Ok(JobPageConfig {
                            path: spec.path,
                            password: spec.password,
                            range,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(())
            }
            ActiveSegment::Encryption(state) => {
                let (params, defaults) = state.finish()?;
                self.job
                    .configuration
                    .writer
                    .set_encryption_parameters(params);
                self.job.configuration.encryption_defaults = defaults;
                self.job.configuration.copy_encryption_applies_to_writer = false;
                Ok(())
            }
            ActiveSegment::UnderlayOverlay(state) => {
                let state = state.finish()?;
                let path = state.path.expect("validated overlay path");
                let target = match state.kind {
                    OverlayKind::Overlay => &mut self.job.configuration.overlays,
                    OverlayKind::Underlay => &mut self.job.configuration.underlays,
                };
                target.push(JobOverlayConfig {
                    path,
                    password: state.password,
                    from: state.from,
                    to: state.to,
                    repeat: state.repeat,
                    kind: state.kind,
                });
                Ok(())
            }
            ActiveSegment::Attachment(state) => {
                self.job
                    .configuration
                    .attachments_to_add
                    .push(state.finish()?);
                Ok(())
            }
            ActiveSegment::CopyAttachment(state) => {
                let state = state.finish()?;
                self.job
                    .configuration
                    .attachments_to_copy
                    .push(JobCopyAttachmentsConfig {
                        path: state.path.expect("validated copy attachment path"),
                        password: state.password,
                        prefix: state.prefix,
                    });
                Ok(())
            }
            ActiveSegment::PageLabels(specs) => {
                self.job.configuration.set_page_labels = Some(
                    specs
                        .iter()
                        .map(|spec| parse_page_label_spec(spec))
                        .collect::<Result<Vec<_>>>()?,
                );
                Ok(())
            }
        }
    }

    fn positional(&mut self, argument: &[u8]) -> Result<()> {
        if !self.gave_input {
            if self.job.configuration.input_file.is_some() || self.job.configuration.empty_input {
                return Err(UsageError::new("input file has already been given").into());
                // cov:ignore: JSON input-slot regression test executes this error; LLVM maps the covered return continuation elsewhere
            }
            if argument.is_empty() {
                return self.select_empty_input();
            }
            self.job.configuration.input_file = Some(path_from_bytes(argument));
            self.gave_input = true;
            return Ok(());
        }
        if !self.gave_output {
            if self.job.configuration.output_file.is_some() || self.job.configuration.replace_input
            {
                return Err(UsageError::new("output file has already been given").into());
                // cov:ignore: JSON output-slot regression test executes this error; LLVM maps the covered return continuation elsewhere
            }
            self.job.configuration.output_file = Some(path_from_bytes(argument));
            self.gave_output = true;
            return Ok(());
        }
        Err(UsageError::new({
            let mut message = b"unknown argument ".to_vec();
            message.extend_from_slice(argument);
            message
        })
        .into())
    }

    fn select_empty_input(&mut self) -> Result<()> {
        if self.gave_input || self.job.configuration.input_file.is_some() {
            return Err(UsageError::new(
                "empty input can't be used since input file has already been given",
            )
            .into());
        }
        self.job.configuration.empty_input = true;
        self.gave_input = true;
        Ok(())
    }

    fn select_replace_input(&mut self) -> Result<()> {
        if self.gave_output
            || self.job.configuration.output_file.is_some()
            || self.job.configuration.replace_input
        {
            return Err(UsageError::new(
                "replace-input can't be used since output file has already been given",
            )
            .into());
        }
        self.job.configuration.replace_input = true;
        self.gave_output = true;
        Ok(())
    }

    fn apply_json_output(&mut self, value: Option<&[u8]>, output: bool) -> Result<()> {
        let version = match value {
            None | Some(b"latest") | Some(b"2") => 2,
            Some(b"1") if !output => 1,
            _ => {
                let name = if output { "json-output" } else { "json" };
                let choices = if output { "2,latest" } else { "1,2,latest" };
                return Err(UsageError::new(format!(
                    "--{name} must be given as --{name}={{{choices}}}"
                ))
                .into());
            }
        };
        self.job.configuration.json_version = Some(version);
        self.job.configuration.require_output = false;
        if output {
            self.job.configuration.json_output = true;
            if !self.job.configuration.json_stream_data_set {
                self.job.configuration.json_stream_data = JsonStreamData::Inline;
            }
            if !self.job.configuration.json_decode_level_set {
                self.job.configuration.json_decode_level = crate::writer::DecodeLevel::None;
            }
            self.job.configuration.json_keys.push(JsonKey::Qpdf);
        }
        Ok(())
    }

    fn apply_job_json_file(&mut self, value: &[u8]) -> Result<()> {
        let path = path_from_bytes(value);
        let prefix_bytes = self.job.message_prefix_bytes.clone();
        let result = (|| {
            // qpdf reads a `--job-json-file`/`jobJsonFile` path through
            // `QUtil::safe_fopen` (`QPDFJob_config.cc:776`,
            // `libqpdf/QUtil.cc:490-519`), which reports a missing or
            // unreadable file with portable `strerror` wording, not Rust's
            // `io::Error` text. Keep the raw path bytes for the actual read
            // (preserving non-UTF-8 paths) but normalize the error text.
            let bytes = std::fs::read(&path).map_err(|error| {
                Error::System(format!(
                    "open {}: {}",
                    path.display(),
                    crate::qutil::strerror_text(&error)
                ))
            })?;
            self.job.initialize_from_json_partial_bytes(&bytes)
        })();
        // The public JSON entry point uses a C-wrapper-compatible
        // `qpdfjob json` prefix. qpdf's Config::jobJsonFile callback is still
        // inside the argv job and keeps its original prefix, so restore it
        // after the nested dispatch on both success and failure.
        self.job.set_message_prefix_bytes(prefix_bytes.clone());
        result.map_err(|error| {
            let mut message = b"error with job-json file ".to_vec();
            message.extend_from_slice(value);
            message.extend_from_slice(b": ");
            message.extend_from_slice(error.to_string().as_bytes());
            message.extend_from_slice(b"\nRun ");
            message.extend_from_slice(&prefix_bytes);
            message.extend_from_slice(b" --job-json-help for information on the file format.");
            Error::Usage(UsageError::new(message))
        })?;
        Ok(())
    }
}

fn option_parts(argument: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
    if argument.len() <= 1 || argument[0] != b'-' || argument == b"-" {
        return None;
    }
    let rest = if argument.get(1) == Some(&b'-') {
        &argument[2..]
    } else {
        &argument[1..]
    };
    if rest.is_empty() || rest.first() == Some(&b'-') {
        return None;
    }
    let Some(equal) = rest.iter().position(|byte| *byte == b'=') else {
        return Some((rest, None));
    };
    Some((&rest[..equal], Some(&rest[equal + 1..])))
}

fn required_value<'a>(name: &[u8], value: Option<&'a [u8]>, parameter: &str) -> Result<&'a [u8]> {
    value.ok_or_else(|| {
        UsageError::new(format!(
            "--{} must be given as --{}={parameter}",
            String::from_utf8_lossy(name),
            String::from_utf8_lossy(name)
        ))
        .into()
    })
}

/// `ArgParser::parseOptions` catches callback `runtime_error` values and
/// routes them through qpdf's usage boundary (`QPDFJob_argv.cc:408-415`).
/// Preserve that classification for the Rust numeric/rotation helpers, which
/// also serve non-argv callers and therefore return their native `Error` kind.
fn argv_callback_value<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| match error {
        Error::Usage(_) => error,
        other => Error::Usage(UsageError::new(other.to_string())),
    })
}

fn required_choice<'a>(
    name: &[u8],
    value: Option<&'a [u8]>,
    choices: &[&[u8]],
) -> Result<&'a [u8]> {
    let choice_name = format!("{{{}}}", choices_text(choices));
    let value = required_value(name, value, &choice_name)?;
    if choices.contains(&value) {
        Ok(value)
    } else if name == b"keep-files-open" {
        Err(UsageError::new(format!(
            "invalid value for --keep-files-open: {}",
            String::from_utf8_lossy(value)
        ))
        .into())
    } else {
        Err(UsageError::new(format!(
            "--{} must be given as --{}={{{}}}",
            String::from_utf8_lossy(name),
            String::from_utf8_lossy(name),
            choices_text(choices)
        ))
        .into())
    }
}

fn choices_text(choices: &[&[u8]]) -> String {
    choices
        .iter()
        .map(|choice| String::from_utf8_lossy(choice).into_owned())
        .collect::<Vec<_>>()
        .join(",")
}

fn unrecognized(argument: &[u8]) -> UsageError {
    let mut message = b"unrecognized argument ".to_vec();
    message.extend_from_slice(argument);
    UsageError::new(message)
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    super::path_from_qpdf_json_bytes(bytes)
}

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

fn expand_arg_files(argv: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
    let mut iter = argv.into_iter();
    let Some(program) = iter.next() else {
        return Err(UsageError::new("qpdf argument vector is empty").into());
    };
    let mut expanded = vec![program];
    for argument in iter {
        if argument.len() <= 1 || argument[0] != b'@' {
            expanded.push(argument);
            continue;
        }
        let path = &argument[1..];
        if path == b"-" {
            let mut bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|error| Error::file_io("read argument file", "-", error))?;
            expanded.extend(split_argument_file_lines(&bytes));
            continue;
        }
        let path = path_from_bytes(path);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                expanded.push(argument);
                continue;
            }
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| Error::file_io("read argument file", path.clone(), error))?;
        expanded.extend(split_argument_file_lines(&bytes));
    }
    Ok(expanded)
}

fn split_argument_file_lines(bytes: &[u8]) -> Vec<Vec<u8>> {
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

#[derive(Default)]
struct PagesState {
    specs: Vec<PageArgSpec>,
    called_file: bool,
    called_range: bool,
}

struct PageArgSpec {
    path: PathBuf,
    password: Option<Vec<u8>>,
    range: String,
}

impl PagesState {
    fn push(&mut self, argument: &[u8]) -> Result<()> {
        if let Some((name, value)) = option_parts(argument) {
            match name {
                b"file" => {
                    let value = required_value(name, value, "file")?;
                    // qpdf's named callback does not mutate the positional
                    // disambiguation flags (`QPDFJob_argv.cc:39-40,243-271`).
                    self.add_file(path_from_bytes(value));
                    return Ok(());
                }
                b"range" => {
                    let value = required_value(name, value, "page-range")?;
                    let value = std::str::from_utf8(value)
                        .map_err(|_| UsageError::new("--range must be valid UTF-8"))?;
                    self.add_range(value, false)?;
                    return Ok(());
                }
                b"password" => {
                    let value = required_value(name, value, "password")?;
                    let current = self.specs.last_mut().ok_or_else(|| {
                        UsageError::new("in --pages, --password must follow a file name")
                    })?;
                    if current.password.is_some() {
                        return Err(
                            UsageError::new("--password already specified for this file").into(),
                        );
                    }
                    current.password = Some(value.to_vec());
                    return Ok(());
                }
                _ => {}
            }
            return Err(self.unknown_option(argument).into());
        }
        if argument.first() == Some(&b'-') && argument != b"-" {
            return Err(self.unknown_option(argument).into());
        }

        if !self.called_file {
            self.add_file(path_from_bytes(argument));
            self.called_file = true;
            return Ok(());
        }
        if self.called_range {
            self.add_file(path_from_bytes(argument));
            self.called_range = false;
            return Ok(());
        }

        if let Ok(range) = std::str::from_utf8(argument) {
            if PageRange::parse_numrange(range).is_ok() {
                self.add_range(range, true)?;
                return Ok(());
            }
        }
        let path = path_from_bytes(argument);
        if argument == b"." || File::open(&path).is_ok() {
            self.add_file(path);
            self.called_range = false;
            Ok(())
        } else {
            Err(UsageError::new(String::from_utf8_lossy(argument).into_owned()).into())
        }
    }

    fn add_file(&mut self, path: PathBuf) {
        self.specs.push(PageArgSpec {
            path,
            password: None,
            range: String::new(),
        });
    }

    fn add_range(&mut self, range: &str, positional: bool) -> Result<()> {
        let current = self
            .specs
            .last_mut()
            .ok_or_else(|| UsageError::new("in --range must follow a file name"))?;
        if !current.range.is_empty() {
            return Err(UsageError::new("--range already specified for this file").into());
        }
        PageRange::parse_numrange(range).map_err(|error| UsageError::new(error.to_string()))?;
        current.range = range.to_owned();
        if positional {
            self.called_range = true;
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<PageArgSpec>> {
        if self.specs.is_empty() {
            return Err(UsageError::new("--pages: no page specifications given").into());
        }
        Ok(self.specs)
    }

    fn unknown_option(&self, argument: &[u8]) -> UsageError {
        let mut message = unrecognized(argument).what_bytes().to_vec();
        message.extend_from_slice(b" (pages options must be terminated with --)");
        UsageError::new(message)
    }
}

struct UnderlayOverlayState {
    kind: OverlayKind,
    path: Option<PathBuf>,
    password: Vec<u8>,
    from: PageRange,
    to: PageRange,
    repeat: Option<PageRange>,
}

impl UnderlayOverlayState {
    fn new(kind: OverlayKind) -> Self {
        Self {
            kind,
            path: None,
            password: Vec::new(),
            from: PageRange::all(),
            to: PageRange::all(),
            repeat: None,
        }
    }

    fn label(&self) -> &'static str {
        match self.kind {
            OverlayKind::Overlay => "overlay",
            OverlayKind::Underlay => "underlay",
        }
    }

    fn push(&mut self, argument: &[u8]) -> Result<()> {
        if let Some((name, value)) = option_parts(argument) {
            match name {
                b"file" => {
                    let value = required_value(name, value, "file")?;
                    if self.path.is_some() {
                        return Err(UsageError::new(format!(
                            "{} file already specified",
                            self.label()
                        ))
                        .into());
                    }
                    self.path = Some(path_from_bytes(value));
                    return Ok(());
                }
                b"password" => {
                    self.password = required_value(name, value, "password")?.to_vec();
                    return Ok(());
                }
                b"to" => {
                    self.to = parse_overlay_range(name, value, self.label(), false)?;
                    return Ok(());
                }
                b"from" => {
                    self.from = parse_overlay_range(name, value, self.label(), true)?;
                    return Ok(());
                }
                b"repeat" => {
                    self.repeat = Some(parse_overlay_range(name, value, self.label(), true)?);
                    return Ok(());
                }
                _ => {}
            }
            return Err(self.unknown_option(argument).into());
        }
        if argument.first() == Some(&b'-') && argument != b"-" {
            return Err(self.unknown_option(argument).into());
        }
        if self.path.is_some() {
            return Err(UsageError::new(format!("{} file already specified", self.label())).into());
        }
        self.path = Some(path_from_bytes(argument));
        Ok(())
    }

    fn finish(self) -> Result<Self> {
        let Some(path) = self.path.as_ref() else {
            return Err(UsageError::new(format!("{} file not specified", self.label())).into());
        };
        if path.as_os_str().is_empty() {
            return Err(UsageError::new(format!("{} file not specified", self.label())).into());
        }
        Ok(self)
    }

    fn unknown_option(&self, argument: &[u8]) -> UsageError {
        let mut message = unrecognized(argument).what_bytes().to_vec();
        message.extend_from_slice(b" (underlay/overlay options must be terminated with --)");
        UsageError::new(message)
    }
}

fn parse_overlay_range(
    name: &[u8],
    value: Option<&[u8]>,
    label: &str,
    _from_range: bool,
) -> Result<PageRange> {
    let value = required_value(name, value, "page-range")?;
    let value = std::str::from_utf8(value).map_err(|_| {
        UsageError::new(format!(
            "{label} --{} must be valid UTF-8",
            String::from_utf8_lossy(name)
        ))
    })?;
    if value.is_empty() {
        return Ok(PageRange::empty());
    }
    Ok(PageRange::parse_numrange(value).map_err(|error| {
        UsageError::new(format!(
            "{label}: invalid --{}= page range {value:?}: {error}",
            String::from_utf8_lossy(name)
        ))
    })?)
}

#[derive(Default)]
struct AttachmentState {
    path: Option<PathBuf>,
    key: Option<Vec<u8>>,
    filename: Option<Vec<u8>>,
    mimetype: Option<Vec<u8>>,
    description: Option<Vec<u8>>,
    creation_date: Option<Vec<u8>>,
    modification_date: Option<Vec<u8>>,
    replace: bool,
}

impl AttachmentState {
    fn push(&mut self, argument: &[u8]) -> Result<()> {
        if let Some((name, value)) = option_parts(argument) {
            match name {
                b"key" => self.key = Some(required_value(name, value, "attachment-key")?.to_vec()),
                b"filename" => {
                    self.filename = Some(required_value(name, value, "filename")?.to_vec())
                }
                b"mimetype" => {
                    let value = required_value(name, value, "mime/type")?;
                    if !value.contains(&b'/') {
                        return Err(UsageError::new(
                            "mime type should be specified as type/subtype",
                        )
                        .into());
                    }
                    self.mimetype = Some(value.to_vec());
                }
                b"description" => {
                    self.description = Some(required_value(name, value, "description")?.to_vec())
                }
                b"creationdate" => {
                    self.creation_date = Some(parse_pdf_date(required_value(
                        name,
                        value,
                        "creation-date",
                    )?)?) // cov:ignore: LLVM maps the covered attachment date continuation to the call setup
                }
                b"moddate" => {
                    self.modification_date = Some(parse_pdf_date(required_value(
                        name,
                        value,
                        "modification-date",
                    )?)?) // cov:ignore: LLVM maps the covered attachment date continuation to the call setup
                }
                b"replace" => self.replace = true,
                _ => return Err(self.unknown(argument).into()),
            }
            return Ok(());
        }
        if argument.first() == Some(&b'-') && argument != b"-" {
            return Err(self.unknown(argument).into());
        }
        self.path = Some(path_from_bytes(argument));
        Ok(())
    }

    fn finish(self) -> Result<AttachmentAddOptions> {
        let path = self
            .path
            .ok_or_else(|| UsageError::new("add attachment: no file specified"))?;
        let basename = path
            .file_name()
            .map(path_component_to_qpdf_bytes)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| UsageError::new("file for --add-attachment may not be empty"))?;
        Ok(AttachmentAddOptions {
            path,
            key: self
                .key
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| basename.clone()),
            filename: self
                .filename
                .filter(|value| !value.is_empty())
                .unwrap_or(basename),
            mimetype: self.mimetype,
            description: self.description,
            creation_date: self.creation_date,
            modification_date: self.modification_date,
            replace: self.replace,
            verbose: false,
        })
    }

    fn unknown(&self, argument: &[u8]) -> UsageError {
        qpdf_subparser_unrecognized_argument(argument, b"attachment")
    }
}

/// Build qpdf's generic sub-parser rejection for a token the active option
/// table does not recognize (`QPDFArgParser::parseArgs`,
/// `libqpdf/QPDFArgParser.cc:498-500`): `unrecognized argument <token> (<table
/// name> options must be terminated with --)`, where `<token>` is the raw
/// argv token exactly as given (qpdf captures `o_arg` before stripping
/// leading dashes or splitting on `=`) and `<table name>` is the sub-parser's
/// name as registered by `QPDFArgParser::registerOptionTable`
/// (`libqpdf/qpdf/auto_job_init.hh:174,183`: `"attachment"` for
/// `--add-attachment`, `"copy attachment"` for `--copy-attachments-from`).
/// This is not an attachment-specific message; every qpdf sub-parser (pages,
/// underlay/overlay, encryption, ...) shares it, parameterized only by table
/// name.
fn qpdf_subparser_unrecognized_argument(argument: &[u8], table: &[u8]) -> UsageError {
    let mut message = b"unrecognized argument ".to_vec();
    message.extend_from_slice(argument);
    message.extend_from_slice(b" (");
    message.extend_from_slice(table);
    message.extend_from_slice(b" options must be terminated with --)");
    UsageError::new(message)
}

fn parse_pdf_date(value: &[u8]) -> Result<Vec<u8>> {
    let value = std::str::from_utf8(value)
        .map_err(|_| UsageError::new("--add-attachment date must be valid UTF-8"))?;
    let bytes = value.as_bytes();
    let valid_body =
        bytes.len() >= 16 && bytes[..2] == *b"D:" && bytes[2..16].iter().all(u8::is_ascii_digit);
    let valid_suffix = match bytes.len() {
        16 => true,
        17 => bytes[16] == b'Z',
        23 => {
            matches!(bytes[16], b'+' | b'-')
                && bytes[17].is_ascii_digit()
                && bytes[18].is_ascii_digit()
                && bytes[19] == b'\''
                && bytes[20].is_ascii_digit()
                && bytes[21].is_ascii_digit()
                && bytes[22] == b'\''
        }
        _ => false,
    };
    if !valid_body || !valid_suffix {
        return Err(UsageError::new(format!("{value} is not a valid PDF timestamp")).into());
    }
    Ok(bytes.to_vec())
}

#[derive(Default)]
struct CopyAttachmentState {
    path: Option<PathBuf>,
    password: Vec<u8>,
    prefix: Vec<u8>,
}

impl CopyAttachmentState {
    fn push(&mut self, argument: &[u8]) -> Result<()> {
        if let Some((name, value)) = option_parts(argument) {
            match name {
                b"password" => self.password = required_value(name, value, "password")?.to_vec(),
                b"prefix" => self.prefix = required_value(name, value, "prefix")?.to_vec(),
                _ => return Err(self.unknown(argument).into()),
            }
            return Ok(());
        }
        if argument.first() == Some(&b'-') && argument != b"-" {
            return Err(self.unknown(argument).into());
        }
        self.path = Some(path_from_bytes(argument));
        Ok(())
    }

    fn finish(self) -> Result<Self> {
        if self
            .path
            .as_ref()
            .is_none_or(|path| path.as_os_str().is_empty())
        {
            return Err(UsageError::new("copy attachments: no file specified").into());
        }
        Ok(self)
    }

    fn unknown(&self, argument: &[u8]) -> UsageError {
        qpdf_subparser_unrecognized_argument(argument, b"copy attachment")
    }
}

#[derive(Clone)]
struct EncryptSubFlag {
    original: Vec<u8>,
    name: Vec<u8>,
    value: Option<Vec<u8>>,
}

struct EncryptionState {
    inherited: EncryptionDefaults,
    positional: Vec<Vec<u8>>,
    dashed_mode: bool,
    positional_mode: bool,
    user_password: Option<Vec<u8>>,
    owner_password: Option<Vec<u8>>,
    key_len: Option<u32>,
    subflags: Vec<EncryptSubFlag>,
}

impl EncryptionState {
    fn new(inherited: EncryptionDefaults) -> Self {
        Self {
            inherited,
            positional: Vec::new(),
            dashed_mode: false,
            positional_mode: false,
            user_password: None,
            owner_password: None,
            key_len: None,
            subflags: Vec::new(),
        }
    }

    fn table_name(&self) -> &'static str {
        match self.key_len {
            Some(40) => "40-bit encryption",
            Some(128) => "128-bit encryption",
            Some(256) => "256-bit encryption",
            _ => "encryption",
        }
    }

    fn push(&mut self, argument: &[u8]) -> Result<()> {
        if let Some((name, value)) = option_parts(argument) {
            if matches!(name, b"user-password" | b"owner-password" | b"bits") {
                if self.key_len.is_some() || self.positional_mode {
                    return Err(self.unknown(argument).into());
                }
                let parameter = match name {
                    b"user-password" => "user_password",
                    b"owner-password" => "owner_password",
                    b"bits" => "{40,128,256}",
                    _ => unreachable!(), // cov:ignore: the selector is limited to user-password, owner-password, and bits
                };
                let value = required_value(name, value, parameter)?;
                self.dashed_mode = true;
                match name {
                    b"user-password" => self.user_password = Some(value.to_vec()),
                    b"owner-password" => self.owner_password = Some(value.to_vec()),
                    b"bits" => self.key_len = Some(parse_encryption_key_len(value)?),
                    _ => unreachable!(), // cov:ignore: the selector is limited to user-password, owner-password, and bits
                }
                return Ok(());
            }
            if !encryption_table_accepts(self.key_len, name) {
                return Err(self.unknown(argument).into());
            }
            if self.dashed_mode && self.key_len.is_none() {
                return Err(self.unknown(argument).into()); // cov:ignore: encryption_table_accepts makes this state unreachable
            }
            if !self.dashed_mode && self.positional.len() < 3 {
                return Err(self.unknown(argument).into()); // cov:ignore: encryption_table_accepts makes this state unreachable
            }
            validate_encryption_choice(name, value, self.key_len.unwrap_or(0))?;
            self.subflags.push(EncryptSubFlag {
                original: argument.to_vec(),
                name: name.to_vec(),
                value: value.map(ToOwned::to_owned),
            });
            return Ok(());
        }

        if argument.first() == Some(&b'-') && argument != b"-" {
            return Err(self.unknown(argument).into());
        }
        if self.dashed_mode {
            return Err(UsageError::new(
                "positional and dashed encryption arguments may not be mixed",
            )
            .into());
        }
        if self.positional.len() < 3 {
            self.positional_mode = true;
            self.positional.push(argument.to_vec());
            if self.positional.len() == 3 {
                self.key_len = Some(parse_encryption_key_len(argument)?);
            }
            return Ok(());
        }
        Err(self.unknown(argument).into())
    }

    fn finish(self) -> Result<(EncryptParams, EncryptionDefaults)> {
        let (user_password, owner_password, key_len) = if self.dashed_mode {
            (
                self.user_password
                    .clone()
                    .unwrap_or_else(|| self.inherited.user_password.clone()),
                self.owner_password
                    .clone()
                    .unwrap_or_else(|| self.inherited.owner_password.clone()),
                self.key_len
                    .ok_or_else(|| UsageError::new("encryption key length is required"))?,
            )
        } else {
            if self.positional.len() < 3 {
                return Err(UsageError::new("encryption key length is required").into());
            }
            (
                self.positional[0].clone(),
                self.positional[1].clone(),
                self.key_len
                    .ok_or_else(|| UsageError::new("encryption key length is required"))?,
            )
        };

        // QPDFJob::Config::encrypt(256, ...) unconditionally sets use_aes;
        // that setting remains in Config after a later --encrypt group changes
        // the key length back to 128 (`QPDFJob_config.cc:1088-1096`).
        let mut use_aes = self.inherited.use_aes || key_len == 256;
        let mut force_v4 = self.inherited.force_v4;
        let mut force_r5 = self.inherited.force_r5;
        let mut allow_insecure = self.inherited.allow_insecure;
        let mut cleartext_metadata = self.inherited.cleartext_metadata;
        let mut permissions = self.inherited.permissions;
        let mut r2_permissions = self.inherited.r2_permissions;
        let mut accessibility_disabled = self.inherited.accessibility_disabled;

        for subflag in &self.subflags {
            let value = subflag.value.as_deref().unwrap_or_default();
            match subflag.name.as_slice() {
                b"use-aes" => use_aes = parse_encryption_yn(value)?,
                b"force-V4" => force_v4 = true,
                b"force-R5" => force_r5 = true,
                b"allow-insecure" => allow_insecure = true,
                b"cleartext-metadata" => cleartext_metadata = true,
                b"print" if key_len == 40 => r2_permissions.print = parse_encryption_yn(value)?,
                b"print" => {
                    permissions.print = match value {
                        b"full" => PrintPermission::High,
                        b"low" => PrintPermission::Low,
                        b"none" => PrintPermission::None,
                        _ => unreachable!("validated encryption print choice"), // cov:ignore: required_choice validates every encryption print value
                    }
                }
                b"modify" if key_len == 40 => r2_permissions.modify = parse_encryption_yn(value)?,
                b"modify" => {
                    let (modify, annotate, forms, assemble) = match value {
                        b"all" => (true, true, true, true),
                        b"annotate" => (false, true, true, true),
                        b"form" => (false, false, true, true),
                        b"assembly" => (false, false, false, true),
                        b"none" => (false, false, false, false),
                        _ => unreachable!("validated encryption modify choice"), // cov:ignore: required_choice validates every encryption modify value
                    };
                    permissions.modify_contents = modify;
                    permissions.annotate = annotate;
                    permissions.fill_forms = forms;
                    permissions.assemble = assemble;
                }
                b"extract" => {
                    let value = parse_encryption_yn(value)?;
                    if key_len == 40 {
                        r2_permissions.extract = value;
                    } else {
                        permissions.extract = value;
                    }
                }
                b"annotate" => {
                    let value = parse_encryption_yn(value)?;
                    if key_len == 40 {
                        r2_permissions.annotate = value;
                    } else {
                        permissions.annotate = value;
                    }
                }
                b"form" => permissions.fill_forms = parse_encryption_yn(value)?,
                b"assemble" => permissions.assemble = parse_encryption_yn(value)?,
                b"accessibility" => {
                    let enabled = parse_encryption_yn(value)?;
                    permissions.accessibility = enabled;
                    accessibility_disabled = !enabled;
                }
                b"modify-other" => permissions.modify_contents = parse_encryption_yn(value)?,
                _ => return Err(self.unknown(&subflag.original).into()), // cov:ignore: encryption_table_accepts admits only the arms above
            }
        }

        let method = match key_len {
            40 => EncryptMethod::V1Rc440,
            128 if force_v4 || cleartext_metadata || use_aes => {
                if use_aes {
                    EncryptMethod::V4Aes128
                } else {
                    EncryptMethod::V4Rc4128
                }
            }
            128 => EncryptMethod::V2Rc4128,
            256 if force_r5 => EncryptMethod::V5R5Aes256,
            256 => EncryptMethod::V5R6Aes256,
            _ => unreachable!("encryption key length was validated"), // cov:ignore: parse_encryption_key_len admits only 40, 128, or 256
        };
        let defaults_user_password = user_password.clone();
        let defaults_owner_password = owner_password.clone();

        let mut params = match method {
            EncryptMethod::V1Rc440 => {
                let mut params = EncryptParams::rc4(method, user_password, owner_password);
                params.r2_permissions = r2_permissions;
                params
            }
            EncryptMethod::V2Rc4128 => {
                let mut params = EncryptParams::rc4(method, user_password, owner_password);
                params.permissions = permissions;
                params
            }
            EncryptMethod::V4Rc4128 | EncryptMethod::V4Aes128 => {
                let mut params = if method == EncryptMethod::V4Aes128 {
                    EncryptParams::v4_aes128(user_password, owner_password)
                } else {
                    EncryptParams::rc4(method, user_password, owner_password)
                };
                params.permissions = permissions;
                params.permissions.accessibility = true;
                params.encrypt_metadata = !cleartext_metadata;
                params
            }
            EncryptMethod::V5R5Aes256 | EncryptMethod::V5R6Aes256 => {
                let mut params = if method == EncryptMethod::V5R5Aes256 {
                    EncryptParams::v5_r5(user_password, owner_password)
                } else {
                    EncryptParams::v5_r6(user_password, owner_password)
                };
                params.permissions = permissions;
                params.permissions.accessibility = true;
                params.encrypt_metadata = !cleartext_metadata;
                params
            }
        };
        // qpdf's `--accessibility=n` warning is emitted at writer setup and
        // modern formats force the bit back on. The typed parameters retain
        // that same effective state here.
        if method == EncryptMethod::V4Aes128 || method == EncryptMethod::V4Rc4128 {
            params.permissions.accessibility = true;
        }
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

    fn unknown(&self, argument: &[u8]) -> UsageError {
        let table = self.table_name();
        let mut message = unrecognized(argument).what_bytes().to_vec();
        message.extend_from_slice(b" (");
        message.extend_from_slice(table.as_bytes());
        message.extend_from_slice(b" options must be terminated with --)");
        UsageError::new(message)
    }
}

fn encryption_table_accepts(key_len: Option<u32>, name: &[u8]) -> bool {
    match key_len {
        None => matches!(name, b"user-password" | b"owner-password" | b"bits"),
        Some(40) => matches!(name, b"extract" | b"annotate" | b"print" | b"modify"),
        Some(128) => matches!(
            name,
            b"cleartext-metadata"
                | b"force-V4"
                | b"accessibility"
                | b"extract"
                | b"print"
                | b"assemble"
                | b"annotate"
                | b"form"
                | b"modify-other"
                | b"modify"
                | b"use-aes"
        ),
        Some(256) => matches!(
            name,
            b"cleartext-metadata"
                | b"force-R5"
                | b"allow-insecure"
                | b"accessibility"
                | b"extract"
                | b"print"
                | b"assemble"
                | b"annotate"
                | b"form"
                | b"modify-other"
                | b"modify"
        ),
        Some(_) => false, // cov:ignore: parse_encryption_key_len admits only 40, 128, or 256
    }
}

fn validate_encryption_choice(name: &[u8], value: Option<&[u8]>, key_len: u32) -> Result<()> {
    let choices: &[&[u8]] = match name {
        b"accessibility" | b"extract" | b"annotate" | b"form" | b"assemble" | b"modify-other" => {
            &[b"y", b"n"]
        }
        b"print" if key_len == 40 => &[b"y", b"n"],
        b"print" => &[b"full", b"low", b"none"],
        b"modify" if key_len == 40 => &[b"y", b"n"],
        b"modify" => &[b"all", b"annotate", b"form", b"assembly", b"none"],
        b"use-aes" => &[b"y", b"n"],
        b"cleartext-metadata" | b"force-V4" | b"force-R5" | b"allow-insecure" => return Ok(()),
        _ => return Ok(()), // cov:ignore: callers pass only names accepted by encryption_table_accepts
    };
    required_choice(name, value, choices).map(|_| ())
}

fn parse_encryption_key_len(value: &[u8]) -> Result<u32> {
    match value {
        b"40" => Ok(40),
        b"128" => Ok(128),
        b"256" => Ok(256),
        _ => Err(UsageError::new("encryption key length must be 40, 128, or 256").into()),
    }
}

fn parse_encryption_yn(value: &[u8]) -> Result<bool> {
    match value {
        b"y" => Ok(true),
        b"n" => Ok(false),
        _ => Err(UsageError::new("encryption option must be y or n").into()), // cov:ignore: required_choice validates every encryption y/n value before this helper
    }
}

// Handle qpdf's dynamically registered help-table options when they are the
// sole post-program argument. The full help prose remains a CLI presentation
// concern, but the library must accept the same table entries, report the
// general help text, and terminate before it asks for an input file
// (`QPDFArgParser.cc:30-34,219-228,766-780`).

const QPDF_HELP_USAGE: &[u8] =
    br#"Read a PDF file, apply transformations or modifications, and write
a new PDF file.

Usage: qpdf [infile] [options] [outfile]
   OR qpdf --help[={topic|--option}]

- infile, options, and outfile may be in any order as long as infile
  precedes outfile.
- Use --empty in place of an input file for a zero-page, empty input
- Use --replace-input in place of an output file to overwrite the
  input file
- outfile may be - to write to stdout; reading from stdin is not supported
- @filename is an argument file; each line is treated as a separate
  command-line argument
- @- may be used to read arguments from stdin
- Later options may override earlier options if contradictory

Related options:
  --empty: use empty file as input
  --job-json-file: job JSON file
  --replace-input: overwrite input with output

For detailed help, visit the qpdf manual: https://qpdf.readthedocs.io
"#;

const QPDF_HELP_ENCRYPTION: &[u8] = br#"Create encrypted files. Usage:

--encrypt \
  [--user-password=user-password] \
  [--owner-password=owner-password] \
  --bits=key-length [options] --

OR

--encrypt user-password owner-password key-length [options] --

The first form, with flags for the passwords and bit length, was
introduced in qpdf 11.7.0. Only the --bits option is is mandatory.
This form allows you to use any text as the password. If passwords are
specified, they must be given before the --bits option.

The second form has been in qpdf since the beginning and wil
continue to be supported. Either or both of user-password and
owner-password may be empty strings.

The key-length parameter must be either 40, 128, or 256. The user
and/or owner password may be omitted. Omitting either password
enables the PDF file to be opened without a password. Specifying
the same value for the user and owner password and specifying an
empty owner password are both considered insecure.

Encryption options are terminated by "--" by itself.

40-bit encryption is insecure, as is 128-bit encryption without
AES. Use 256-bit encryption unless you have a specific reason to
use an insecure format, such as testing or compatibility with very
old viewers. You must use the --allow-weak-crypto to create
encrypted files that use insecure cryptographic algorithms. The
--allow-weak-crypto flag appears outside of --encrypt ... --
(before --encrypt or after --).

Options for 40-bit only:
  --annotate=[y|n]         restrict comments, filling forms, and signing
  --extract=[y|n]          restrict text/graphic extraction
  --modify=[y|n]           restrict document modification
  --print=[y|n]            restrict printing

Options for 128-bit or 256-bit:
  --accessibility=[y|n]    restrict accessibility (usually ignored)
  --annotate=[y|n]         restrict commenting/filling form fields
  --assemble=[y|n]         restrict document assembly
  --extract=[y|n]          restrict text/graphic extraction
  --form=[y|n]             restrict filling form fields
  --modify-other=[y|n]     restrict other modifications
  --modify=modify-opt      control modify access by level
  --print=print-opt        control printing access
  --cleartext-metadata     prevent encryption of metadata

For 128-bit only:
  --use-aes=[y|n]          indicates whether to use AES encryption
  --force-V4               forces use of V=4 encryption handler

For 256-bit only:
  --force-R5               forces use of deprecated R=5 encryption
  --allow-insecure         allow user password with empty owner password

For detailed help, visit the qpdf manual: https://qpdf.readthedocs.io
"#;

const QPDF_HELP_ROTATE: &[u8] = br#"--rotate=[+|-]angle[:page-range]

Rotate specified pages by multiples of 90 degrees specifying
either absolute or relative angles. "angle" may be 0, 90, 180,
or 270. You almost always want to use +angle or -angle rather
than just angle, as discussed in the manual. Run
qpdf --help=page-ranges for help with page ranges.

For detailed help, visit the qpdf manual: https://qpdf.readthedocs.io
"#;

const QPDF_HELP_LINEARIZE: &[u8] = br#"Create linearized (web-optimized) output files.

For detailed help, visit the qpdf manual: https://qpdf.readthedocs.io
"#;

const QPDF_HELP_PAGE_RANGES: &[u8] = br#"A full description of the page range syntax, with examples, can be
found in the manual. In summary, a range is a comma-separated list of groups. A group is a number or a range of numbers separated by a
dash. A group may be prepended by x to exclude its members from the
previous group. A number may be one of

- <n>        where <n> represents a number is the <n>th page
- r<n>       is the <n>th page from the end
- z          the last page, same as r1

- a,b,c      pages a, b, and c
- a-b        pages a through b inclusive; if a > b, this counts down
- a-b,xc     pages a through b except page c
- a-b,xc-d   pages a through b except pages c through d

You can append :even or :odd to select every other page from the
resulting set of pages, where :odd starts with the first page and
:even starts with the second page. These are odd and even pages
from the resulting set, not based on the original page numbers.

For detailed help, visit the qpdf manual: https://qpdf.readthedocs.io
"#;

const QPDF_HELP_DECRYPT: &[u8] = br#"Create an unencrypted output file even if the input file was
encrypted. Normally qpdf preserves whatever encryption was
present on the input file. This option overrides that behavior.

For detailed help, visit the qpdf manual: https://qpdf.readthedocs.io
"#;

const QPDF_HELP_TOPICS: &[&[u8]] = &[
    b"add-attachment",
    b"advanced-control",
    b"attachments",
    b"completion",
    b"copy-attachments",
    b"encryption",
    b"exit-status",
    b"general",
    b"help",
    b"inspection",
    b"json",
    b"modification",
    b"overlay-underlay",
    b"page-ranges",
    b"page-selection",
    b"pdf-dates",
    b"testing",
    b"transformation",
    b"usage",
];

const QPDF_HELP_ALL: &str = include_str!("../../qpdf-help-all.txt");
const QPDF_COPYRIGHT: &str = include_str!("../../qpdf-copyright.txt");
const QPDF_SHOW_CRYPTO: &[u8] = include_bytes!("../../qpdf-show-crypto.txt");
const QPDF_JSON_HELP_1: &[u8] = include_bytes!("../../qpdf-json-help-1.txt");
const QPDF_JSON_HELP_2: &[u8] = include_bytes!("../../qpdf-json-help-2.txt");
const QPDF_JSON_HELP_LATEST: &[u8] = include_bytes!("../../qpdf-json-help-latest.txt");
const QPDF_JOB_JSON_HELP: &[u8] = include_bytes!("../../qpdf-job-json-help.txt");

const QPDF_HELP_OPTIONS: &[&[u8]] = &[
    b"--accessibility",
    b"--add-attachment",
    b"--allow-insecure",
    b"--allow-weak-crypto",
    b"--annotate",
    b"--assemble",
    b"--bits",
    b"--check",
    b"--check-linearization",
    b"--cleartext-metadata",
    b"--coalesce-contents",
    b"--collate",
    b"--completion-bash",
    b"--completion-zsh",
    b"--compress-streams",
    b"--compression-level",
    b"--copy-attachments-from",
    b"--copy-encryption",
    b"--copyright",
    b"--creationdate",
    b"--decode-level",
    b"--decrypt",
    b"--description",
    b"--deterministic-id",
    b"--empty",
    b"--encrypt",
    b"--encryption-file-password",
    b"--externalize-inline-images",
    b"--extract",
    b"--file",
    b"--filename",
    b"--filtered-stream-data",
    b"--flatten-annotations",
    b"--flatten-rotation",
    b"--force-R5",
    b"--force-V4",
    b"--force-version",
    b"--form",
    b"--from",
    b"--generate-appearances",
    b"--help",
    b"--ignore-xref-streams",
    b"--ii-min-bytes",
    b"--is-encrypted",
    b"--job-json-file",
    b"--job-json-help",
    b"--json",
    b"--json-help",
    b"--json-input",
    b"--json-key",
    b"--json-object",
    b"--json-output",
    b"--json-stream-data",
    b"--json-stream-prefix",
    b"--keep-files-open",
    b"--keep-files-open-threshold",
    b"--keep-inline-images",
    b"--key",
    b"--linearize",
    b"--linearize-pass1",
    b"--list-attachments",
    b"--mimetype",
    b"--min-version",
    b"--moddate",
    b"--modify",
    b"--modify-other",
    b"--newline-before-endstream",
    b"--no-original-object-ids",
    b"--no-warn",
    b"--normalize-content",
    b"--object-streams",
    b"--oi-min-area",
    b"--oi-min-height",
    b"--oi-min-width",
    b"--optimize-images",
    b"--overlay",
    b"--owner-password",
    b"--pages",
    b"--password",
    b"--password-file",
    b"--password-is-hex-key",
    b"--password-mode",
    b"--prefix",
    b"--preserve-unreferenced",
    b"--preserve-unreferenced-resources",
    b"--print",
    b"--progress",
    b"--qdf",
    b"--range",
    b"--raw-stream-data",
    b"--recompress-flate",
    b"--remove-attachment",
    b"--remove-page-labels",
    b"--remove-restrictions",
    b"--remove-unreferenced-resources",
    b"--repeat",
    b"--replace",
    b"--replace-input",
    b"--report-memory-usage",
    b"--requires-password",
    b"--rotate",
    b"--set-page-labels",
    b"--show-attachment",
    b"--show-crypto",
    b"--show-encryption",
    b"--show-encryption-key",
    b"--show-linearization",
    b"--show-npages",
    b"--show-object",
    b"--show-pages",
    b"--show-xref",
    b"--split-pages",
    b"--static-aes-iv",
    b"--static-id",
    b"--stream-data",
    b"--suppress-password-recovery",
    b"--suppress-recovery",
    b"--test-json-schema",
    b"--to",
    b"--underlay",
    b"--update-from-json",
    b"--use-aes",
    b"--user-password",
    b"--verbose",
    b"--version",
    b"--warning-exit-0",
    b"--with-images",
];

fn known_help_target(value: &[u8]) -> bool {
    value == b"all" || QPDF_HELP_TOPICS.contains(&value) || QPDF_HELP_OPTIONS.contains(&value)
}

fn program_name_bytes(argv0: &[u8]) -> &[u8] {
    // qpdf's getWhoami checks the last '/' first and only falls back to the
    // last '\\' when no '/' exists (QUtil.cc:788-803).
    let separator = argv0
        .iter()
        .rposition(|byte| *byte == b'/')
        .or_else(|| argv0.iter().rposition(|byte| *byte == b'\\'));
    let name = separator.map_or(argv0, |index| &argv0[index + 1..]);
    if name.len() > 4 && name.ends_with(b".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

fn help_top(program: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    for suffix in [
        b" --help=topic\" for help on a topic.\n".as_slice(),
        b" --help=--option\" for help on an option.\n".as_slice(),
        b" --help=all\" to see all available help.\n".as_slice(),
    ] {
        output.extend_from_slice(b"Run \"");
        output.extend_from_slice(program);
        output.extend_from_slice(suffix);
    }
    output.extend_from_slice(
        b"\nTopics:\n  add-attachment: attach (embed) files\n  advanced-control: tweak qpdf's behavior\n  attachments: work with embedded files\n  completion: shell completion\n  copy-attachments: copy attachments from another file\n  encryption: create encrypted files\n  exit-status: meanings of qpdf's exit codes\n  general: general options\n  help: information about qpdf\n  inspection: inspect PDF files\n  json: JSON output for PDF information\n  modification: change parts of the PDF\n  overlay-underlay: overlay/underlay pages from other files\n  page-ranges: page range syntax\n  page-selection: select pages from one or more files\n  pdf-dates: PDF date format\n  testing: options for testing or debugging\n  transformation: make structural PDF changes\n  usage: basic invocation\n\nFor detailed help, visit the qpdf manual: https://qpdf.readthedocs.io\n",
    );
    output
}

fn help_all_for_program(program: &[u8]) -> Vec<u8> {
    if program == b"qpdf" {
        return QPDF_HELP_ALL.as_bytes().to_vec();
    }
    let marker = b"qpdf --help=";
    let source = QPDF_HELP_ALL.as_bytes();
    let mut output = Vec::with_capacity(source.len());
    let mut cursor = 0;
    let mut replacements = 0;
    while let Some(relative) = source[cursor..]
        .windows(marker.len())
        .position(|window| window == marker && replacements < 3)
    {
        let offset = cursor + relative;
        output.extend_from_slice(&source[cursor..offset]);
        output.extend_from_slice(program);
        output.extend_from_slice(b" --help=");
        cursor = offset + marker.len();
        replacements += 1;
    }
    output.extend_from_slice(&source[cursor..]);
    output
}

fn version_output(program: &[u8]) -> Vec<u8> {
    let mut output = program.to_vec();
    output.extend_from_slice(b" version ");
    output.extend_from_slice(crate::qpdf_version().as_bytes());
    output.extend_from_slice(b"\nRun ");
    output.extend_from_slice(program);
    output.extend_from_slice(b" --copyright to see copyright and license information.\n");
    output
}

fn copyright_output(program: &[u8]) -> Vec<u8> {
    let source = QPDF_COPYRIGHT.as_bytes();
    let marker = b"qpdf version";
    let Some(offset) = source
        .windows(marker.len())
        .position(|window| window == marker)
    else {
        return source.to_vec(); // cov:ignore: QPDF_COPYRIGHT is a pinned compile-time asset and always contains this marker
    };
    let mut output = Vec::with_capacity(source.len() + program.len());
    output.extend_from_slice(&source[..offset]);
    output.extend_from_slice(program);
    output.extend_from_slice(b" version");
    output.extend_from_slice(&source[offset + marker.len()..]);
    output
}

fn completion_command(
    argv0: &[u8],
    zsh: bool,
    qpdf_executable: Option<&[u8]>,
    appdir: Option<&[u8]>,
    appimage: Option<&[u8]>,
) -> (Vec<u8>, bool) {
    let executable = qpdf_executable
        .map(ToOwned::to_owned)
        .or_else(|| {
            appdir
                .filter(|appdir| appdir.len() < argv0.len() && argv0.starts_with(appdir))
                .and(appimage.map(ToOwned::to_owned))
        })
        .unwrap_or_else(|| argv0.to_vec());
    let program = program_name_bytes(argv0);
    let mut command = Vec::new();
    if zsh {
        command.extend_from_slice(b"autoload -U +X bashcompinit && bashcompinit && ");
    }
    command.extend_from_slice(b"complete -o bashdefault -o default");
    if !zsh {
        command.extend_from_slice(b" -o nospace");
    }
    command.extend_from_slice(b" -C \"");
    command.extend_from_slice(&executable);
    command.extend_from_slice(b"\" ");
    command.extend_from_slice(program);
    command.push(b'\n');
    let slash = executable.iter().position(|byte| *byte == b'/');
    (command, slash.is_some_and(|offset| offset != 0))
}

fn os_string_bytes(value: &std::ffi::OsString) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    {
        value.to_string_lossy().as_bytes().to_vec()
    }
}

fn completion_with_values(
    job: &QPDFJob,
    argv0: &[u8],
    zsh: bool,
    qpdf_executable: Option<&[u8]>,
    appdir: Option<&[u8]>,
    appimage: Option<&[u8]>,
) -> Result<()> {
    let (command, relative) = completion_command(argv0, zsh, qpdf_executable, appdir, appimage);
    job.logger.info(command)?;
    if relative {
        let mut warning = b"WARNING: ".to_vec();
        warning.extend_from_slice(program_name_bytes(argv0));
        warning.extend_from_slice(b" completion enabled using relative path to executable\n");
        job.logger.error(warning)?;
    }
    Ok(())
}

fn completion(job: &QPDFJob, argv0: &[u8], zsh: bool) -> Result<()> {
    let qpdf_executable = std::env::var_os("QPDF_EXECUTABLE");
    let appdir = std::env::var_os("APPDIR");
    let appimage = std::env::var_os("APPIMAGE");
    let qpdf_executable = qpdf_executable.as_ref().map(os_string_bytes);
    let appdir = appdir.as_ref().map(os_string_bytes);
    let appimage = appimage.as_ref().map(os_string_bytes);
    completion_with_values(
        job,
        argv0,
        zsh,
        qpdf_executable.as_deref(),
        appdir.as_deref(),
        appimage.as_deref(),
    )
}

fn show_crypto_provider(job: &QPDFJob, provider: &str) -> Result<()> {
    let registered = registered_crypto_providers();
    if !registered.iter().any(|name| name == provider) {
        // qpdf's provider registry raises std::logic_error here
        // (`QPDFCryptoProvider.cc:91-99`), not QPDFUsage.
        return Err(Error::Internal(format!(
            "QPDFCryptoProvider: request to set default provider to unknown implementation \"{provider}\""
        )));
    }
    job.logger
        .info(crypto_provider_output(provider, &registered))
}

fn registered_crypto_providers() -> Vec<String> {
    // qpdf registers providers selected by the build (`QPDFCryptoProvider.cc:49-62`;
    // `libqpdf/CMakeLists.txt:207-270`). The pinned Linux oracle asset is GnuTLS;
    // the CI Windows and macOS qpdf builds use their required OpenSSL provider.
    QPDF_CRYPTO_REGISTRY
        .split(|byte| *byte == b'\n')
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect()
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
const QPDF_CRYPTO_REGISTRY: &[u8] = b"openssl\n";

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const QPDF_CRYPTO_REGISTRY: &[u8] = QPDF_SHOW_CRYPTO;

fn crypto_provider_output(provider: &str, registered: &[String]) -> String {
    let mut output = String::new();
    output.push_str(provider);
    output.push('\n');
    for name in registered {
        if name != provider {
            output.push_str(name);
            output.push('\n');
        }
    }
    output
}

fn show_crypto(job: &QPDFJob) -> Result<()> {
    let registered = registered_crypto_providers();
    let provider = std::env::var_os("QPDF_CRYPTO_PROVIDER")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| registered.first().cloned().unwrap_or_default());
    show_crypto_provider(job, &provider)
}

fn help_from_generated_table(value: Option<&[u8]>, program: &[u8]) -> Option<Vec<u8>> {
    let Some(value) = value else {
        return Some(help_top(program));
    };
    if value == b"all" {
        return Some(help_all_for_program(program));
    }
    let target = String::from_utf8_lossy(value);
    let marker = format!("== {target} (");
    let start = QPDF_HELP_ALL.find(&marker)?;
    let section_start = QPDF_HELP_ALL[start..].find('\n')? + start + 1;
    let section_end = QPDF_HELP_ALL[section_start..]
        .find("\n== ")
        .or_else(|| QPDF_HELP_ALL[section_start..].find("\n===="))
        .map_or(QPDF_HELP_ALL.len(), |offset| section_start + offset);
    let section = QPDF_HELP_ALL[section_start..section_end].trim_matches('\n');
    Some(
        format!(
            "{section}\n\nFor detailed help, visit the qpdf manual: https://qpdf.readthedocs.io\n"
        )
        .into_bytes(),
    )
}

fn help_text(value: Option<&[u8]>, program: &[u8]) -> Vec<u8> {
    if let Some(help) = help_from_generated_table(value, program) {
        return help;
    } // cov:ignore: the generated table covers every validated help target; LLVM maps this covered exit to the closing brace
      // cov:ignore-start: fallback help text is defensive for a generated-table drift; all registered qpdf targets are checked above
    match value {
        Some(b"usage") => QPDF_HELP_USAGE.to_vec(),
        Some(b"encryption") => QPDF_HELP_ENCRYPTION.to_vec(),
        Some(b"page-ranges") => QPDF_HELP_PAGE_RANGES.to_vec(),
        Some(b"--rotate") => QPDF_HELP_ROTATE.to_vec(),
        Some(b"--linearize") => QPDF_HELP_LINEARIZE.to_vec(),
        Some(b"--decrypt") => QPDF_HELP_DECRYPT.to_vec(),
        Some(target) if target.starts_with(b"--") => format!(
            "{}\n\nFor detailed help, visit the qpdf manual: https://qpdf.readthedocs.io\n",
            String::from_utf8_lossy(target)
        )
        .into_bytes(),
        _ => help_top(program),
    }
    // cov:ignore-end
}

fn handle_sole_help_option(job: &mut QPDFJob, argv0: &[u8], argument: &[u8]) -> Result<bool> {
    let Some((name, value)) = option_parts(argument) else {
        return Ok(false);
    };
    let program_bytes = program_name_bytes(argv0);
    match name {
        b"version" | b"copyright" | b"show-crypto" | b"job-json-help" | b"completion-bash"
        | b"completion-zsh" | b"help" => {
            job.argv_early_exit = true;
            match name {
                b"version" => job.logger.info(version_output(program_bytes))?, // cov:ignore: LLVM maps the covered version logger continuation to the call setup
                b"copyright" => job.logger.info(copyright_output(program_bytes))?, // cov:ignore: LLVM maps the covered copyright logger continuation to the call setup
                b"show-crypto" => show_crypto(job)?,
                b"completion-bash" => completion(job, argv0, false)?,
                b"completion-zsh" => completion(job, argv0, true)?,
                b"help" => {
                    if let Some(value) = value {
                        if !known_help_target(value) {
                            let mut message = b"unknown help option".to_vec();
                            if !value.is_empty() {
                                message.push(b' ');
                                message.extend_from_slice(value);
                            }
                            return Err(UsageError::new(message).into());
                        }
                    }
                    job.logger.info(help_text(value, program_bytes))?;
                }
                b"job-json-help" => job.logger.info(QPDF_JOB_JSON_HELP)?,
                _ => unreachable!(), // cov:ignore: the outer match restricts this arm to the listed help names
            } // cov:ignore: the outer help-name match restricts this inner match to the listed arms; LLVM maps its covered exit to the closing brace
            Ok(true)
        }
        b"json-help" => {
            if matches!(value, Some(b""))
                || !matches!(value, None | Some(b"1") | Some(b"2") | Some(b"latest"))
            {
                return Err(UsageError::new(
                    "--json-help must be given as --json-help={1,2,latest}",
                )
                .into());
            }
            job.argv_early_exit = true; // cov:ignore: json-help success is exercised; llvm-cov leaves this shared match assignment at zero in its duplicate record
            let help = match value {
                Some(b"1") => QPDF_JSON_HELP_1,
                Some(b"2") => QPDF_JSON_HELP_2,
                _ => QPDF_JSON_HELP_LATEST,
            };
            job.logger.info(help)?;
            Ok(true)
        }
        _ => Ok(false),
    }
} // cov:ignore: LLVM attributes the read-password helper boundary to its neighboring function records

fn read_password_file(job: &QPDFJob, value: &[u8]) -> Result<Option<Vec<u8>>> {
    let path = path_from_bytes(value);
    let bytes = if value == b"-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| Error::file_io("read password file", "-", error))?;
        bytes
    } else {
        // cov:ignore: the file-backed branch is exercised; llvm-cov leaves this shared branch line at zero in its duplicate record
        std::fs::read(&path)
            .map_err(|error| Error::file_io("read password file", path.clone(), error))?
    }; // cov:ignore: LLVM maps the covered password-file read continuation to its branch arms
    if bytes.is_empty() {
        // cov:ignore-start: these explanatory comments have no executable path
        // QPDFJob::Config::passwordFile leaves the existing password intact
        // when the input has no lines (`QPDFJob_config.cc:649-658`).
        // cov:ignore-end
        return Ok(None); // cov:ignore: empty password-file test executes this branch; LLVM maps the hit to the guard
    }
    let first_newline = bytes.iter().position(|byte| *byte == b'\n');
    if first_newline.is_some_and(|index| index + 1 < bytes.len()) {
        job.logger.error(job.prefixed_message(
            b"WARNING: all but the first line of the password file are ignored\n",
        ))?; // cov:ignore: password-file warning test executes the logger write; LLVM maps the covered continuation to the format call
    }
    let first_line_len = first_newline.unwrap_or(bytes.len());
    let mut password = bytes[..first_line_len].to_vec();
    if first_newline.is_some() && password.last() == Some(&b'\r') {
        password.pop();
    }
    Ok(Some(password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QPDFLogger;
    use std::ffi::OsString;

    #[test]
    fn show_crypto_rejects_a_provider_outside_the_pinned_registry() {
        let job = QPDFJob::new();
        let error = show_crypto_provider(&job, "unknown-provider").unwrap_err();
        assert!(matches!(error, Error::Internal(message) if message
            .contains("unknown implementation \"unknown-provider\"")));
    }

    #[test]
    fn crypto_provider_output_puts_the_default_first() {
        let registered = ["gnutls".to_owned(), "openssl".to_owned()];
        assert_eq!(
            crypto_provider_output("openssl", &registered),
            "openssl\ngnutls\n"
        );
    }

    #[test]
    fn completion_reports_a_relative_executable_warning() {
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        let mut job = QPDFJob::new();
        job.set_logger(logger);

        completion_with_values(&job, b"./qpdf", false, None, None, None).unwrap();
    }

    #[test]
    fn completion_matches_qpdf_appimage_and_relative_path_selection() {
        let (command, relative) = completion_command(
            b"/opt/bundle/usr/bin/qpdf",
            false,
            None,
            Some(b"/opt/bundle"),
            Some(b"/opt/qpdf.AppImage"),
        );
        assert_eq!(
            command,
            b"complete -o bashdefault -o default -o nospace -C \"/opt/qpdf.AppImage\" qpdf\n"
        );
        assert!(!relative);

        let (command, relative) = completion_command(b"./qpdf", false, None, None, None);
        assert_eq!(
            command,
            b"complete -o bashdefault -o default -o nospace -C \"./qpdf\" qpdf\n"
        );
        assert!(relative);

        let (command, relative) = completion_command(
            b"qpdf",
            true,
            Some(b"./custom-qpdf"),
            Some(b"/opt/bundle"),
            Some(b"/opt/qpdf.AppImage"),
        );
        assert_eq!(
            command,
            b"autoload -U +X bashcompinit && bashcompinit && complete -o bashdefault -o default -C \"./custom-qpdf\" qpdf\n"
        );
        assert!(relative);
    }

    #[test]
    fn program_name_matches_qpdf_basename_and_exe_stripping() {
        assert_eq!(program_name_bytes(b"/opt/custom-qpdf"), b"custom-qpdf");
        assert_eq!(program_name_bytes(b"C:\\tools\\qpdf.exe"), b"qpdf");
        assert_eq!(
            program_name_bytes(b"/tmp/custom\\qpdf.exe"),
            b"custom\\qpdf"
        );
        assert_eq!(program_name_bytes(b"/tmp/"), b"");
    }

    #[test]
    fn completion_preserves_raw_executable_and_program_name_bytes() {
        let (command, relative) =
            completion_command(b"/opt/qpdf", false, Some(b"/tmp/qpdf-\xff"), None, None);
        assert_eq!(
            command,
            b"complete -o bashdefault -o default -o nospace -C \"/tmp/qpdf-\xff\" qpdf\n"
        );
        assert!(!relative);

        let (command, relative) = completion_command(b"/tmp/custom-\xff", false, None, None, None);
        assert_eq!(
            command,
            b"complete -o bashdefault -o default -o nospace -C \"/tmp/custom-\xff\" custom-\xff\n"
        );
        assert!(!relative);
    }

    #[test]
    fn os_string_bytes_preserves_the_completion_executable() {
        let executable = OsString::from("/tmp/qpdf");
        assert_eq!(os_string_bytes(&executable), b"/tmp/qpdf");
    }

    #[test]
    fn raw_argv_preserves_program_bytes_for_prefix_and_version() {
        let mut job = QPDFJob::new();
        job.initialize_from_raw_argv(&[b"/tmp/custom-\xff.exe".to_vec(), b"--version".to_vec()])
            .unwrap();

        assert_eq!(job.message_prefix_bytes(), b"custom-\xff");
        assert_eq!(
            version_output(b"custom-\xff"),
            b"custom-\xff version 11.9.0\nRun custom-\xff --copyright to see copyright and license information.\n"
        );
    }

    #[test]
    fn add_attachment_rejects_an_unrecognized_dashed_token_with_qpdf_wording() {
        // Confirmed live: `qpdf in.pdf --add-attachment=x --overlay -- out.pdf`
        // prints exactly this message (flpdf-3yn9.48.193). `--overlay` is not
        // a registered `--add-attachment` sub-flag, and unlike a bare
        // positional token it is not accepted as an opaque attachment
        // filename either -- qpdf's generic sub-parser rejects any dashed
        // token that isn't a known sub-flag name.
        let mut job = QPDFJob::new();
        let error = job
            .initialize_from_raw_argv(&[
                b"qpdf".to_vec(),
                b"in.pdf".to_vec(),
                b"--add-attachment=x".to_vec(),
                b"--overlay".to_vec(),
                b"--".to_vec(),
                b"out.pdf".to_vec(),
            ])
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "unrecognized argument --overlay (attachment options must be terminated with --)"
        );
    }

    #[test]
    fn add_attachment_rejects_an_unrecognized_named_sub_flag_with_the_full_token() {
        // Confirmed live: `qpdf in.pdf --add-attachment=x --bogus=1 -- out.pdf`
        // prints the *complete* original token, `=1` included -- qpdf
        // captures `o_arg` before splitting on `=` (flpdf-3yn9.48.193).
        let mut job = QPDFJob::new();
        let error = job
            .initialize_from_raw_argv(&[
                b"qpdf".to_vec(),
                b"in.pdf".to_vec(),
                b"--add-attachment=x".to_vec(),
                b"--bogus=1".to_vec(),
                b"--".to_vec(),
                b"out.pdf".to_vec(),
            ])
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "unrecognized argument --bogus=1 (attachment options must be terminated with --)"
        );
    }

    #[test]
    fn copy_attachments_from_rejects_an_unrecognized_dashed_token_with_qpdf_wording() {
        // Confirmed live: `qpdf in.pdf --copy-attachments-from=x --overlay --
        // out.pdf` prints this message, with "copy attachment" (not
        // "attachment") naming the sub-parser table
        // (`libqpdf/qpdf/auto_job_init.hh:183`, flpdf-3yn9.48.193).
        let mut job = QPDFJob::new();
        let error = job
            .initialize_from_raw_argv(&[
                b"qpdf".to_vec(),
                b"in.pdf".to_vec(),
                b"--copy-attachments-from=x".to_vec(),
                b"--overlay".to_vec(),
                b"--".to_vec(),
                b"out.pdf".to_vec(),
            ])
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "unrecognized argument --overlay (copy attachment options must be terminated with --)"
        );
    }
}
