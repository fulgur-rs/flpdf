use flpdf::job::{
    AttachmentAddOptions, JobDocument, JobExitCode, JsonJobOptions, JsonJobOutput, JsonStreamData,
    PageSpecInput, QPDFJob,
};
use flpdf::json_inspect::DecodeLevel;
use flpdf::pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
use flpdf::{
    EncryptParams, EncryptedError, Error, ObjectHandle, PageRange, Pdf, PdfOpenOptions, PdfWriter,
    QPDFLogger, WriterConfiguration,
};
use std::fs::File;
use std::io::{BufReader, Cursor, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[test]
fn raw_argv_initializer_matches_qpdf_final_configuration_state() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    // QPDFJob::EncConfig keeps permissions across repeated --encrypt groups;
    // only the passwords and key length are replaced by the later group.
    let repeated_output = tempdir.path().join("repeated-encryption.pdf");
    let mut repeated = QPDFJob::new();
    repeated
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--allow-weak-crypto".to_vec(),
            b"--encrypt".to_vec(),
            b"user".to_vec(),
            b"owner".to_vec(),
            b"128".to_vec(),
            b"--print=none".to_vec(),
            b"--".to_vec(),
            b"--encrypt".to_vec(),
            b"user-2".to_vec(),
            b"owner-2".to_vec(),
            b"128".to_vec(),
            b"--".to_vec(),
            repeated_output.to_string_lossy().into_owned().into_bytes(),
        ])
        .expect("repeated encryption groups should initialize");
    assert_eq!(repeated.run().unwrap(), JobExitCode::Success);
    let repeated_bytes = std::fs::read(&repeated_output).unwrap();
    assert!(
        repeated_bytes
            .windows(b"/P -2056".len())
            .any(|window| window == b"/P -2056"),
        "repeated output did not retain qpdf permissions: {}",
        String::from_utf8_lossy(&repeated_bytes)
    );

    // qpdf delays the insecure-empty-owner check until the final configuration
    // pass, so a later --decrypt is allowed to clear the pending encryption.
    let decrypt_output = tempdir.path().join("decrypt-after-encrypt.pdf");
    let mut decrypt_after_encrypt = QPDFJob::new();
    decrypt_after_encrypt
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--encrypt".to_vec(),
            b"user".to_vec(),
            Vec::new(),
            b"256".to_vec(),
            b"--".to_vec(),
            b"--decrypt".to_vec(),
            decrypt_output.to_string_lossy().into_owned().into_bytes(),
        ])
        .expect("later --decrypt should clear the pending encryption");
    assert_eq!(decrypt_after_encrypt.run().unwrap(), JobExitCode::Success);

    // A job-json inputFile changes Config, but it does not change the argv
    // parser's local gave_input flag. The following positional argument must
    // therefore be rejected by Config::inputFile, just as qpdf does.
    let job_json = tempdir.path().join("input-only.json");
    std::fs::write(
        &job_json,
        serde_json::json!({"inputFile": fixture}).to_string(),
    )
    .unwrap();
    let mut json_then_positional = QPDFJob::new();
    let error = json_then_positional
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            format!("--job-json-file={}", job_json.display()).into_bytes(),
            tempdir
                .path()
                .join("unexpected-output.pdf")
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
        ])
        .expect_err("a positional input after job-json inputFile must be rejected");
    assert!(matches!(error, Error::Usage(_)));

    // Named --file does not set QPDFJob_argv's called_pages_file flag. Thus a
    // following positional numeric token is a second filename, not a range.
    let pages_output = tempdir.path().join("named-file-pages.pdf");
    let mut named_file_pages = QPDFJob::new();
    named_file_pages
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            b"--empty".to_vec(),
            b"--pages".to_vec(),
            format!("--file={}", fixture.display()).into_bytes(),
            b"1".to_vec(),
            b"--".to_vec(),
            pages_output.to_string_lossy().into_owned().into_bytes(),
        ])
        .expect("named pages arguments should initialize");
    assert_eq!(named_file_pages.run().unwrap(), JobExitCode::Error);

    for option in [b"--json=".as_slice(), b"--json-output=".as_slice()] {
        let mut job = QPDFJob::new();
        assert!(
            job.initialize_from_raw_argv(&[
                b"qpdfjob".to_vec(),
                fixture.to_string_lossy().into_owned().into_bytes(),
                option.to_vec(),
            ])
            .is_err(),
            "an explicit empty value must not select a JSON default"
        );
    }
}

#[test]
fn raw_argv_help_and_attachment_defaults_match_qpdf() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let (logger, info) = logger_with_info_sink();
    let mut help = QPDFJob::new();
    help.set_logger(logger);
    help.initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes).contains("Topics:"));

    let mut unknown_help = QPDFJob::new();
    let error = unknown_help
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=foobar".to_vec()])
        .expect_err("unknown help targets must be usage errors");
    assert!(matches!(error, Error::Usage(_)));

    let output = tempdir.path().join("attachment-defaults.pdf");
    let mut attachment_job = QPDFJob::new();
    attachment_job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--add-attachment".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--key=".to_vec(),
            b"--filename=".to_vec(),
            b"--".to_vec(),
            output.to_string_lossy().into_owned().into_bytes(),
        ])
        .unwrap();
    assert_eq!(attachment_job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"one-page.pdf")
        .unwrap()
        .is_some());
}

#[test]
fn raw_argv_password_file_preserves_terminal_cr_and_warns_on_extra_lines() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let encrypted = tempdir.path().join("encrypted.pdf");
    let decrypted = tempdir.path().join("decrypted.pdf");
    let mut source = Pdf::open(BufReader::new(File::open(&fixture).unwrap())).unwrap();
    let mut writer = PdfWriter::new(&mut source);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(b"pw-\r", b"owner"));
    writer.set_output_file(&encrypted).unwrap();
    writer.write().unwrap();

    let password_file = tempdir.path().join("password.txt");
    std::fs::write(&password_file, b"pw-\r").unwrap();
    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        encrypted.to_string_lossy().into_owned().into_bytes(),
        decrypted.to_string_lossy().into_owned().into_bytes(),
        format!("--password-file={}", password_file.display()).into_bytes(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let extra_lines = tempdir.path().join("password-extra.txt");
    std::fs::write(&extra_lines, b"pw-\r\nignored\n").unwrap();
    let (logger, errors) = logger_with_error_sink();
    let mut warning_job = QPDFJob::new();
    warning_job.set_logger(logger);
    warning_job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            tempdir
                .path()
                .join("warning-output.pdf")
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
            format!("--password-file={}", extra_lines.display()).into_bytes(),
        ])
        .unwrap();
    assert!(String::from_utf8_lossy(&errors.lock().unwrap().bytes)
        .contains("WARNING: all but the first line of the password file are ignored"));
}

#[test]
fn raw_argv_modern_accessibility_n_emits_qpdf_diagnostic() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("accessibility.pdf");
    let (logger, errors) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"--force-V4".to_vec(),
        b"--use-aes=y".to_vec(),
        b"--accessibility=n".to_vec(),
        b"--".to_vec(),
        output.to_string_lossy().into_owned().into_bytes(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(String::from_utf8_lossy(&errors.lock().unwrap().bytes)
        .contains("-accessibility=n is ignored for modern encryption formats"));
}

#[test]
fn raw_argv_encryption_config_survives_repeated_groups_and_decrypt() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let aes_toggle_output = tempdir.path().join("aes-toggle.pdf");
    let mut aes_toggle = QPDFJob::new();
    aes_toggle
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--allow-weak-crypto".to_vec(),
            b"--encrypt".to_vec(),
            b"user".to_vec(),
            b"owner".to_vec(),
            b"128".to_vec(),
            b"--use-aes=y".to_vec(),
            b"--".to_vec(),
            b"--encrypt".to_vec(),
            b"user-2".to_vec(),
            b"owner-2".to_vec(),
            b"128".to_vec(),
            b"--use-aes=n".to_vec(),
            b"--".to_vec(),
            aes_toggle_output
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
        ])
        .unwrap();
    assert_eq!(aes_toggle.run().unwrap(), JobExitCode::Success);
    let aes_toggle_bytes = std::fs::read(aes_toggle_output).unwrap();
    assert!(aes_toggle_bytes
        .windows(b"/V 2".len())
        .any(|window| window == b"/V 2"));
    assert!(aes_toggle_bytes
        .windows(b"/R 3".len())
        .any(|window| window == b"/R 3"));

    let decrypt_output = tempdir.path().join("decrypt-encrypt.pdf");
    let mut decrypt_encrypt = QPDFJob::new();
    decrypt_encrypt
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--allow-weak-crypto".to_vec(),
            b"--encrypt".to_vec(),
            b"user".to_vec(),
            b"owner".to_vec(),
            b"128".to_vec(),
            b"--print=none".to_vec(),
            b"--".to_vec(),
            b"--decrypt".to_vec(),
            b"--encrypt".to_vec(),
            b"user-2".to_vec(),
            b"owner-2".to_vec(),
            b"128".to_vec(),
            b"--".to_vec(),
            decrypt_output.to_string_lossy().into_owned().into_bytes(),
        ])
        .unwrap();
    assert_eq!(decrypt_encrypt.run().unwrap(), JobExitCode::Success);
    let decrypt_bytes = std::fs::read(decrypt_output).unwrap();
    assert!(decrypt_bytes
        .windows(b"/P -2056".len())
        .any(|window| window == b"/P -2056"));

    let json_config = tempdir.path().join("aes-config.json");
    let json_output = tempdir.path().join("aes-config-output.pdf");
    std::fs::write(
        &json_config,
        serde_json::json!({
            "inputFile": fixture,
            "outputFile": json_output,
            "encrypt": {
                "userPassword": "json-user",
                "ownerPassword": "json-owner",
                "128bit": {"useAes": "y"}
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut json_then_argv = QPDFJob::new();
    json_then_argv
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            format!("--job-json-file={}", json_config.display()).into_bytes(),
            b"--allow-weak-crypto".to_vec(),
            b"--encrypt".to_vec(),
            b"argv-user".to_vec(),
            b"argv-owner".to_vec(),
            b"128".to_vec(),
            b"--use-aes=n".to_vec(),
            b"--".to_vec(),
        ])
        .unwrap();
    assert_eq!(json_then_argv.run().unwrap(), JobExitCode::Success);
    let json_bytes = std::fs::read(json_output).unwrap();
    assert!(json_bytes
        .windows(b"/V 2".len())
        .any(|window| window == b"/V 2"));

    let mut insecure = QPDFJob::new();
    let error = insecure
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--encrypt".to_vec(),
            b"user".to_vec(),
            Vec::new(),
            b"256".to_vec(),
            b"--".to_vec(),
            tempdir
                .path()
                .join("rejected-insecure.pdf")
                .to_string_lossy()
                .into_owned()
                .into_bytes(),
        ])
        .expect_err("the final qpdf configuration check must reject insecure encryption");
    assert!(matches!(error, Error::Usage(_)));
}

#[test]
fn raw_argv_256_bit_group_keeps_aes_for_a_later_128_bit_group() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("256-then-128.pdf");
    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"256".to_vec(),
        b"--".to_vec(),
        b"--encrypt".to_vec(),
        b"user-2".to_vec(),
        b"owner-2".to_vec(),
        b"128".to_vec(),
        b"--".to_vec(),
        output.to_string_lossy().into_owned().into_bytes(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let bytes = std::fs::read(output).unwrap();
    assert!(bytes.windows(b"/V 4".len()).any(|window| window == b"/V 4"));
}

#[test]
fn raw_argv_named_pages_range_does_not_change_positional_state() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let (logger, info) = logger_with_info_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        b"--empty".to_vec(),
        b"--pages".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--range=1".to_vec(),
        format!("--file={}", fixture.display()).into_bytes(),
        b"1".to_vec(),
        b"--".to_vec(),
        b"--show-npages".to_vec(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert_eq!(info.lock().unwrap().bytes, b"2\n");

    let third_source = tempfile::tempdir().unwrap();
    let third_file = third_source.path().join("1");
    std::fs::copy(&fixture, &third_file).unwrap();
    let (logger, info) = logger_with_info_sink();
    let mut repeated_named_range = QPDFJob::new();
    repeated_named_range.set_logger(logger);
    repeated_named_range
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            b"--empty".to_vec(),
            b"--pages".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"1".to_vec(),
            format!("--file={}", fixture.display()).into_bytes(),
            b"--range=1".to_vec(),
            third_file.to_string_lossy().into_owned().into_bytes(),
            b"--".to_vec(),
            b"--show-npages".to_vec(),
        ])
        .unwrap();
    assert_eq!(repeated_named_range.run().unwrap(), JobExitCode::Success);
    assert_eq!(info.lock().unwrap().bytes, b"3\n");
}

#[test]
fn raw_argv_help_table_and_completion_emit_qpdf_output() {
    let (logger, info) = logger_with_info_sink();
    let mut usage = QPDFJob::new();
    usage.set_logger(logger);
    usage
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=usage".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes).contains("Usage:"));

    let (logger, info) = logger_with_info_sink();
    let mut encryption = QPDFJob::new();
    encryption.set_logger(logger);
    encryption
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=encryption".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes)
        .contains("Create encrypted files. Usage:"));

    let (logger, info) = logger_with_info_sink();
    let mut rotate = QPDFJob::new();
    rotate.set_logger(logger);
    rotate
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=--rotate".to_vec()])
        .unwrap();
    assert!(!info.lock().unwrap().bytes.is_empty());

    let (logger, info) = logger_with_info_sink();
    let mut linearize = QPDFJob::new();
    linearize.set_logger(logger);
    linearize
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=--linearize".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes).contains("Create linearized"));

    let (logger, info) = logger_with_info_sink();
    let mut option_help = QPDFJob::new();
    option_help.set_logger(logger);
    option_help
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=--allow-insecure".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes).contains("Allow creation"));

    let (logger, info) = logger_with_info_sink();
    let mut page_ranges = QPDFJob::new();
    page_ranges.set_logger(logger);
    page_ranges
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=page-ranges".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes)
        .contains("A full description of the page range syntax"));

    let (logger, info) = logger_with_info_sink();
    let mut decrypt_help = QPDFJob::new();
    decrypt_help.set_logger(logger);
    decrypt_help
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--help=--decrypt".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes)
        .contains("Create an unencrypted output file"));

    let qpdf_help = Command::new("qpdf").args(["--help=all"]).output().unwrap();
    assert!(qpdf_help.status.success());
    let (logger, info) = logger_with_info_sink();
    let mut all_help = QPDFJob::new();
    all_help.set_logger(logger);
    all_help
        .initialize_from_raw_argv(&[b"qpdf".to_vec(), b"--help=all".to_vec()])
        .unwrap();
    assert_eq!(info.lock().unwrap().bytes, qpdf_help.stdout);

    let (logger, info) = logger_with_info_sink();
    let mut custom_all_help = QPDFJob::new();
    custom_all_help.set_logger(logger);
    custom_all_help
        .initialize_from_raw_argv(&[b"/opt/custom-qpdf".to_vec(), b"--help=all".to_vec()])
        .unwrap();
    assert!(String::from_utf8_lossy(&info.lock().unwrap().bytes)
        .starts_with("Run \"custom-qpdf --help=topic\""));

    let (logger, info) = logger_with_info_sink();
    let mut completion = QPDFJob::new();
    completion.set_logger(logger);
    completion
        .initialize_from_raw_argv(&[b"qpdfjob".to_vec(), b"--completion-bash".to_vec()])
        .unwrap();
    assert_eq!(
        info.lock().unwrap().bytes,
        b"complete -o bashdefault -o default -o nospace -C \"qpdfjob\" qpdfjob\n"
    );

    let (logger, info) = logger_with_info_sink();
    let mut custom_completion = QPDFJob::new();
    custom_completion.set_logger(logger);
    custom_completion
        .initialize_from_raw_argv(&[b"/opt/custom-qpdf".to_vec(), b"--completion-bash".to_vec()])
        .unwrap();
    assert_eq!(
        info.lock().unwrap().bytes,
        b"complete -o bashdefault -o default -o nospace -C \"/opt/custom-qpdf\" custom-qpdf\n"
    );

    let (logger, info) = logger_with_info_sink();
    let mut custom_zsh_completion = QPDFJob::new();
    custom_zsh_completion.set_logger(logger);
    custom_zsh_completion
        .initialize_from_raw_argv(&[b"/opt/custom-qpdf".to_vec(), b"--completion-zsh".to_vec()])
        .unwrap();
    assert_eq!(
        info.lock().unwrap().bytes,
        b"autoload -U +X bashcompinit && bashcompinit && complete -o bashdefault -o default -C \"/opt/custom-qpdf\" custom-qpdf\n"
    );

    for option in [
        "--copyright",
        "--show-crypto",
        "--json-help=1",
        "--json-help=2",
        "--json-help=latest",
        "--job-json-help",
    ] {
        let oracle = Command::new("qpdf").arg(option).output().unwrap();
        if !oracle.status.success() {
            continue;
        }
        let (logger, info) = logger_with_info_sink();
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.initialize_from_raw_argv(&[b"qpdf".to_vec(), option.as_bytes().to_vec()])
            .unwrap();
        let output = info.lock().unwrap().bytes.clone();
        if option == "--show-crypto" {
            // qpdf enumerates the providers compiled into the process. The
            // pinned qpdf binary has gnutls on Linux and gnutls+openssl on
            // macOS; flpdf's library boundary intentionally does not own a
            // process-global provider registry.
            assert!(!output.is_empty(), "{option} must report a provider");
        } else {
            assert_eq!(output, oracle.stdout, "{option}");
        }
    }
}

#[test]
fn json_encrypt_allow_insecure_reaches_final_configuration_check() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("allowed.pdf");
    let json = serde_json::json!({
        "inputFile": fixture,
        "outputFile": output,
        "encrypt": {
            "userPassword": "u",
            "ownerPassword": "",
            "256bit": {"allowInsecure": ""}
        }
    });
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json.to_string()).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
}

#[test]
fn json_encrypt_insecure_can_be_cleared_by_later_argv_decrypt() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let json_path = tempdir.path().join("insecure.json");
    let output = tempdir.path().join("decrypted.pdf");
    std::fs::write(
        &json_path,
        serde_json::json!({
            "encrypt": {
                "userPassword": "u",
                "ownerPassword": "",
                "256bit": {}
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        format!("--job-json-file={}", json_path.display()).into_bytes(),
        b"--decrypt".to_vec(),
        output.to_string_lossy().into_owned().into_bytes(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
}

#[test]
fn json_encrypt_keeps_qpdf_r2_permissions_from_an_earlier_argv_group() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let json_path = tempdir.path().join("encrypt-40.json");
    let output = tempdir.path().join("encrypt-40.pdf");
    std::fs::write(
        &json_path,
        serde_json::json!({
            "encrypt": {
                "userPassword": "json-user",
                "ownerPassword": "json-owner",
                "40bit": {}
            }
        })
        .to_string(),
    )
    .unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--allow-weak-crypto".to_vec(),
        b"--encrypt".to_vec(),
        b"argv-user".to_vec(),
        b"argv-owner".to_vec(),
        b"40".to_vec(),
        b"--print=n".to_vec(),
        b"--".to_vec(),
        format!("--job-json-file={}", json_path.display()).into_bytes(),
        output.to_string_lossy().into_owned().into_bytes(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let bytes = std::fs::read(output).unwrap();
    assert!(bytes
        .windows(b"/P -8".len())
        .any(|window| window == b"/P -8"));

    let extract_json = tempdir.path().join("extract-40.json");
    let extract_output = tempdir.path().join("extract-128.pdf");
    std::fs::write(
        &extract_json,
        serde_json::json!({
            "encrypt": {
                "userPassword": "json-user",
                "ownerPassword": "json-owner",
                "40bit": {"extract": "n"}
            }
        })
        .to_string(),
    )
    .unwrap();
    let mut r2_then_r3 = QPDFJob::new();
    r2_then_r3
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            fixture.to_string_lossy().into_owned().into_bytes(),
            b"--allow-weak-crypto".to_vec(),
            b"--encrypt".to_vec(),
            b"argv-user".to_vec(),
            b"argv-owner".to_vec(),
            b"40".to_vec(),
            b"--extract=n".to_vec(),
            b"--".to_vec(),
            format!("--job-json-file={}", extract_json.display()).into_bytes(),
            b"--encrypt".to_vec(),
            b"argv-user-2".to_vec(),
            b"argv-owner-2".to_vec(),
            b"128".to_vec(),
            b"--".to_vec(),
            extract_output.to_string_lossy().into_owned().into_bytes(),
        ])
        .unwrap();
    assert_eq!(r2_then_r3.run().unwrap(), JobExitCode::Success);
    let extract_bytes = std::fs::read(extract_output).unwrap();
    assert!(extract_bytes
        .windows(b"/P -4".len())
        .any(|window| window == b"/P -4"));
}

#[test]
fn raw_argv_job_json_positionals_reject_existing_config_slots() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let input_json = tempdir.path().join("input.json");
    std::fs::write(
        &input_json,
        serde_json::json!({"inputFile": fixture}).to_string(),
    )
    .unwrap();
    let mut input_job = QPDFJob::new();
    assert!(matches!(
        input_job
            .initialize_from_raw_argv(&[
                b"qpdfjob".to_vec(),
                format!("--job-json-file={}", input_json.display()).into_bytes(),
                b"unexpected-output.pdf".to_vec(),
            ])
            .unwrap_err(),
        Error::Usage(_)
    ));

    let output_json = tempdir.path().join("output.json");
    std::fs::write(
        &output_json,
        serde_json::json!({"outputFile": tempdir.path().join("configured.pdf")}).to_string(),
    )
    .unwrap();
    let mut output_job = QPDFJob::new();
    assert!(matches!(
        output_job
            .initialize_from_raw_argv(&[
                b"qpdfjob".to_vec(),
                format!("--job-json-file={}", output_json.display()).into_bytes(),
                fixture.to_string_lossy().into_owned().into_bytes(),
                b"second-output.pdf".to_vec(),
            ])
            .unwrap_err(),
        Error::Usage(_)
    ));
}

#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(target_os = "linux")]
use std::path::PathBuf;

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
struct SinkState {
    bytes: Vec<u8>,
}

struct RecordingSink {
    state: Arc<Mutex<SinkState>>,
}

struct FailingSink;

impl Pipeline for RecordingSink {
    fn identifier(&self) -> &str {
        "job lifecycle test sink"
    }

    fn write(&mut self, data: &[u8]) -> PipelineResult<()> {
        self.state.lock().unwrap().bytes.extend_from_slice(data);
        Ok(())
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

impl Pipeline for FailingSink {
    fn identifier(&self) -> &str {
        "job lifecycle failing sink"
    }

    fn write(&mut self, _data: &[u8]) -> PipelineResult<()> {
        Err(PipelineError::runtime("warning sink failed"))
    }

    fn finish(&mut self) -> PipelineResult<()> {
        Ok(())
    }
}

fn logger_with_warning_sink() -> (QPDFLogger, Arc<Mutex<SinkState>>) {
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger.set_warn(Some(PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    })));
    (logger, state)
}

fn logger_with_info_sink() -> (QPDFLogger, Arc<Mutex<SinkState>>) {
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger.set_info(Some(PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    })));
    (logger, state)
}

fn logger_with_error_sink() -> (QPDFLogger, Arc<Mutex<SinkState>>) {
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger.set_error(Some(PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    })));
    (logger, state)
}

fn add_raw_argv_output_if_required(args: &mut Vec<Vec<u8>>) {
    let inspection = args.iter().any(|argument| {
        let name = argument
            .strip_prefix(b"--")
            .unwrap_or(argument)
            .split(|byte| *byte == b'=')
            .next()
            .unwrap_or_default();
        matches!(
            name,
            b"check"
                | b"check-linearization"
                | b"is-encrypted"
                | b"job-json-help"
                | b"json"
                | b"json-help"
                | b"json-output"
                | b"list-attachments"
                | b"requires-password"
                | b"show-attachment"
                | b"show-encryption"
                | b"show-linearization"
                | b"show-npages"
                | b"show-pages"
                | b"show-object"
                | b"show-xref"
                | b"version"
                | b"copyright"
                | b"show-crypto"
                | b"help"
                | b"completion-bash"
                | b"completion-zsh"
        )
    });
    if !inspection {
        args.push(b"raw-argv-output.pdf".to_vec());
    }
}

/// qpdf's Windows C-runtime text stdout is CRLF-terminated while a recording
/// pipeline receives the logger's logical LF bytes. Compare the text payload
/// independently of that host transport convention.
fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }
    normalized
}

#[cfg(target_os = "linux")]
fn non_utf8_path(directory: &Path, filename: &[u8]) -> PathBuf {
    let mut bytes = directory.as_os_str().as_bytes().to_vec();
    bytes.push(b'/');
    bytes.extend_from_slice(filename);
    PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(target_os = "linux")]
fn job_json_with_paths(input: &Path, output: &Path) -> Vec<u8> {
    let mut json = b"{\"inputFile\":\"".to_vec();
    json.extend_from_slice(input.as_os_str().as_bytes());
    json.extend_from_slice(b"\",\"outputFile\":\"");
    json.extend_from_slice(output.as_os_str().as_bytes());
    json.extend_from_slice(b"\"}");
    json
}

fn xref_stream_with_extra_data() -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let xref_offset = bytes.len();
    let xref_header =
        b"1 0 obj\n<< /Type /XRef /Size 4 /Root 2 0 R /W [1 3 1] /Index [0 4] /Length 21 >>\nstream\n";
    let xref_tail = b"\nendstream\nendobj\n";
    let catalog_offset = xref_offset + xref_header.len() + 21 + xref_tail.len();
    let catalog = b"2 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n";
    let pages_offset = catalog_offset + catalog.len();
    let pages = b"3 0 obj\n<< /Type /Pages /Count 0 /Kids [] >>\nendobj\n";

    let mut entries = Vec::with_capacity(21);
    entries.extend_from_slice(&[0, 0, 0, 0xff, 0xff]);
    for offset in [xref_offset, catalog_offset, pages_offset] {
        entries.push(1);
        entries.extend_from_slice(&(offset as u32).to_be_bytes()[1..]);
        entries.push(0);
    }
    entries.push(0);

    bytes.extend_from_slice(xref_header);
    bytes.extend_from_slice(&entries);
    bytes.extend_from_slice(xref_tail);
    bytes.extend_from_slice(catalog);
    bytes.extend_from_slice(pages);
    bytes.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    bytes
}

#[test]
fn new_job_matches_qpdf_defaults() {
    let job = QPDFJob::default();

    assert_eq!(job.message_prefix(), "qpdf");
    assert_eq!(job.logger(), QPDFLogger::default_logger());
    assert!(!job.has_warnings());
    job.complete(false).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);
    assert_eq!(JobExitCode::Error.as_i32(), 2);
    assert_eq!(JobExitCode::Success.as_i32(), 0);
    assert_eq!(JobExitCode::Warning.as_i32(), 3);
}

#[test]
fn job_json_byte_entry_point_accepts_literal_high_bit_password_bytes() {
    let mut json = br#"{"inputFile":"input.pdf","outputFile":"output.pdf","password":""}"#.to_vec();
    let password_end = json.len() - 2;
    json.insert(password_end, 0x80);

    let mut job = QPDFJob::new();
    job.initialize_from_json_bytes(&json)
        .expect("qpdf accepts raw high-bit bytes in a JSON string");

    let mut partial_json = br#"{"password":""}"#.to_vec();
    partial_json.insert(partial_json.len() - 2, 0x80);
    let mut partial_job = QPDFJob::new();
    partial_job
        .initialize_from_json_partial_bytes(&partial_json)
        .expect("partial byte entry point must preserve raw JSON bytes");
}

#[test]
fn qpdfjob_error_report_matches_the_qpdf_c_wrapper_boundary() {
    let (logger, state) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_message_prefix("qpdfjob json");

    job.report_job_error(&Error::Usage(flpdf::UsageError::new(
        "an output file name is required; use - for standard output",
    )))
    .unwrap();

    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdfjob json: an output file name is required; use - for standard output\n"
    );

    state.lock().unwrap().bytes.clear();
    job.report_job_error(&Error::SystemBytes(
        b"json-input-\xff: errors found in JSON".to_vec(),
    ))
    .unwrap();
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdfjob json: json-input-\xff: errors found in JSON\n"
    );
}

#[test]
fn qpdfjob_error_report_includes_the_input_name_for_terminal_parse_failure() {
    let (logger, state) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_input_name_bytes(b"bad.pdf");

    job.report_job_error(&Error::parse(
        0,
        "unable to find trailer dictionary while recovering damaged file",
    ))
    .unwrap();

    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: bad.pdf: unable to find trailer dictionary while recovering damaged file\n"
    );
}

#[test]
fn qpdfjob_error_report_uses_qpdf_invalid_password_wording() {
    let (logger, state) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_input_name("encrypted.pdf");

    job.report_job_error(&Error::Encrypted(flpdf::EncryptedError::BadPassword))
        .unwrap();

    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: encrypted.pdf: invalid password\n"
    );

    state.lock().unwrap().bytes.clear();
    job.set_input_name("input.pdf");
    job.report_job_error(&Error::Io(std::io::Error::from(
        std::io::ErrorKind::PermissionDenied,
    )))
    .unwrap();
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: input.pdf: Permission denied\n"
    );

    for (kind, expected) in [
        (std::io::ErrorKind::AlreadyExists, "File exists"),
        (std::io::ErrorKind::InvalidInput, "Invalid argument"),
        (std::io::ErrorKind::IsADirectory, "Is a directory"),
        (std::io::ErrorKind::NotADirectory, "Not a directory"),
    ] {
        state.lock().unwrap().bytes.clear();
        job.set_input_name("input.pdf");
        job.report_job_error(&Error::Io(std::io::Error::from(kind)))
            .unwrap();
        assert_eq!(
            state.lock().unwrap().bytes,
            format!("qpdf: input.pdf: {expected}\n").as_bytes()
        );
    }

    state.lock().unwrap().bytes.clear();
    job.set_input_name_bytes(b"");
    job.report_job_error(&Error::Io(std::io::Error::from(
        std::io::ErrorKind::PermissionDenied,
    )))
    .unwrap();
    assert_eq!(state.lock().unwrap().bytes, b"qpdf: Permission denied\n");

    state.lock().unwrap().bytes.clear();
    job.report_job_error(&Error::Encrypted(flpdf::EncryptedError::BadPassword))
        .unwrap();
    assert_eq!(state.lock().unwrap().bytes, b"qpdf: invalid password\n");
}

#[test]
fn keep_files_open_policy_counts_distinct_page_sources_and_honors_overrides() {
    let range = PageRange::parse_numrange("1").unwrap();
    let one_source = [
        PageSpecInput::new(1, range.clone()),
        PageSpecInput::new(1, range.clone()),
    ];
    let two_sources = [
        PageSpecInput::new(1, range.clone()),
        PageSpecInput::new(2, range),
    ];

    let mut job = QPDFJob::new();
    job.set_keep_files_open_threshold(1);
    assert!(job.keep_files_open_for_page_specs(&one_source));
    assert!(!job.keep_files_open_for_page_specs(&two_sources));

    job.set_keep_files_open(true);
    assert!(job.keep_files_open_for_page_specs(&two_sources));
    job.set_keep_files_open(false);
    assert!(!job.keep_files_open_for_page_specs(&one_source));

    assert_eq!(
        QPDFJob::parse_keep_files_open_threshold("+50junk").unwrap(),
        50
    );
}

#[test]
fn keep_files_open_policy_is_parsed_at_argv_and_json_job_boundaries() {
    let range = PageRange::parse_numrange("1").unwrap();
    let specs = [PageSpecInput::new(1, range.clone())];

    let mut argv_job = QPDFJob::new();
    argv_job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            "--keep-files-open=n".to_owned(),
            "--keep-files-open-threshold=+50junk".to_owned(),
        ])
        .unwrap();
    assert!(!argv_job.keep_files_open_for_page_specs(&specs));

    let json = serde_json::json!({
        "inputFile": "input.pdf",
        "outputFile": "output.pdf",
        "keepFilesOpen": "n",
        "keepFilesOpenThreshold": "50junk"
    })
    .to_string();
    let mut json_job = QPDFJob::new();
    json_job.initialize_from_json(&json).unwrap();
    assert!(!json_job.keep_files_open_for_page_specs(&specs));
}

#[test]
fn argv_keep_files_open_rejects_an_unknown_choice() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            "--keep-files-open=maybe".to_owned(),
        ])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "invalid value for --keep-files-open: maybe"
    ));
}

#[test]
fn argv_expands_an_argument_file_before_option_parsing() {
    // QPDFArgParser::handleArgFileArguments (QPDFArgParser.cc:232-260) runs
    // one level of `@file` expansion before any option is inspected. CRLF
    // line endings are normalized the same way `QUtil::read_lines_from_file`
    // does (preserve_eol=false): a trailing `\r` before `\n` is dropped, and
    // the final line is still kept even without a trailing newline.
    let tempdir = tempfile::tempdir().unwrap();
    let argfile = tempdir.path().join("args");
    std::fs::write(&argfile, b"--deterministic-id\r\n--progress").unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let output = tempdir.path().join("argfile-output.pdf");

    let mut job = QPDFJob::new();
    job.initialize_from_argv(&[
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        format!("@{}", argfile.display()),
    ])
    .expect("an argument file's options should expand before parsing");

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
}

#[test]
fn argv_treats_an_unopenable_argument_file_token_as_a_literal_positional() {
    // qpdf treats an `@path` it cannot open as an ordinary argv token rather
    // than an error (QPDFArgParser.cc:238-243): parsing continues and the
    // token is later rejected as an unrecognized positional/extra argument
    // by whatever consumes it, not by the `@file` expansion step itself.
    let tempdir = tempfile::tempdir().unwrap();
    let missing = format!("@{}", tempdir.path().join("missing-args").display());

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            missing.clone(),
        ])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == format!("unknown argument {missing}")
    ));
}

#[test]
fn argv_top_level_double_dash_resumes_the_main_option_table() {
    // qpdf's top-level `--` resets to the main option table rather than
    // ending option parsing (QPDFArgParser.cc:447-451,543): an option
    // spelled after it is still recognized, unlike a conventional
    // end-of-options terminator.
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("reset-output.pdf");

    let mut job = QPDFJob::new();
    job.initialize_from_argv(&[
        "qpdfjob".to_owned(),
        "--".to_owned(),
        "--deterministic-id".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
    ])
    .expect("an option spelled after a top-level -- should still be recognized");

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
}

#[test]
fn argv_page_label_option_table_accepts_specs_until_its_terminator() {
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&[
        "qpdfjob".to_owned(),
        "input.pdf".to_owned(),
        "output.pdf".to_owned(),
        "--remove-page-labels".to_owned(),
        "--set-page-labels".to_owned(),
        "1:a".to_owned(),
        "r2:R/2/prefix".to_owned(),
        "z://end".to_owned(),
        "--".to_owned(),
    ])
    .expect("qpdf page-label option table should consume its positional specs");
}

#[test]
fn raw_argv_initializer_reaches_the_canonical_writer_for_qpdf_options() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("raw-argv-output.pdf");
    let args = vec![
        b"qpdfjob".to_vec(),
        input.as_os_str().to_string_lossy().as_bytes().to_vec(),
        output.as_os_str().to_string_lossy().as_bytes().to_vec(),
        b"--qdf".to_vec(),
        b"--no-original-object-ids".to_vec(),
        b"--object-streams=disable".to_vec(),
        b"--stream-data=preserve".to_vec(),
        b"--min-version=1.3".to_vec(),
        b"--force-version=1.3".to_vec(),
        b"--remove-unreferenced-resources=no".to_vec(),
        b"--warning-exit-0".to_vec(),
    ];

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&args)
        .expect("the library raw argv boundary should own qpdf options");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let written = std::fs::read(output).unwrap();
    assert!(written.starts_with(b"%PDF-1.3\n"));
    assert!(written
        .windows(b"%QDF-".len())
        .any(|window| window == b"%QDF-"));
}

#[test]
fn raw_argv_initializer_applies_job_json_files_at_their_argv_occurrence() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("job-json-output.pdf");
    let job_json = tempdir.path().join("job.json");
    std::fs::write(
        &job_json,
        serde_json::json!({
            "inputFile": input,
            "outputFile": output,
            "qdf": ""
        })
        .to_string(),
    )
    .unwrap();
    let args = vec![
        b"qpdfjob".to_vec(),
        format!("--job-json-file={}", job_json.display()).into_bytes(),
        b"--object-streams=disable".to_vec(),
    ];

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&args)
        .expect("job-json-file must layer into the same job configuration");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(std::fs::read(output)
        .unwrap()
        .windows(b"%QDF-".len())
        .any(|window| window == b"%QDF-"));
}

#[test]
fn raw_argv_initializer_parses_pages_and_encryption_segments() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("segments-output.pdf");
    let args = vec![
        b"qpdfjob".to_vec(),
        fixture.as_os_str().to_string_lossy().as_bytes().to_vec(),
        b"--allow-weak-crypto".to_vec(),
        b"--pages".to_vec(),
        b".".to_vec(),
        b"1".to_vec(),
        b"--".to_vec(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"--".to_vec(),
        output.as_os_str().to_string_lossy().as_bytes().to_vec(),
    ];

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&args)
        .expect("pages and encryption option tables should be library-owned");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(std::fs::read(output)
        .unwrap()
        .windows(b"/Encrypt".len())
        .any(|window| { window == b"/Encrypt" }));
}

#[test]
fn raw_argv_initializer_parses_attachment_and_overlay_segments() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let donor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("named-segments-output.pdf");
    let args = vec![
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--add-attachment".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--key=fixture".to_vec(),
        b"--filename=fixture.pdf".to_vec(),
        b"--mimetype=application/pdf".to_vec(),
        b"--description=fixture".to_vec(),
        b"--creationdate=D:20240101120000Z".to_vec(),
        b"--moddate=D:20240101120000Z".to_vec(),
        b"--replace".to_vec(),
        b"--".to_vec(),
        b"--copy-attachments-from".to_vec(),
        donor.to_string_lossy().into_owned().into_bytes(),
        b"--password=".to_vec(),
        b"--prefix=copied-".to_vec(),
        b"--".to_vec(),
        b"--overlay".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        b"--to=1".to_vec(),
        b"--from=1".to_vec(),
        b"--repeat=1".to_vec(),
        b"--".to_vec(),
        output.to_string_lossy().into_owned().into_bytes(),
    ];

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&args)
        .expect("all qpdf named segment tables should be library-owned");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn raw_argv_initializer_preserves_non_utf8_path_and_password_bytes() {
    use std::os::unix::ffi::OsStrExt;

    let tempdir = tempfile::tempdir().unwrap();
    let input = non_utf8_path(tempdir.path(), b"input-\x80.pdf");
    let output = non_utf8_path(tempdir.path(), b"output-\x81.pdf");
    let encrypted = non_utf8_path(tempdir.path(), b"encrypted-\x82.pdf");
    let decrypted = non_utf8_path(tempdir.path(), b"decrypted-\x83.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();
    let mut input_bytes = input.as_os_str().as_bytes().to_vec();
    let mut output_bytes = output.as_os_str().as_bytes().to_vec();
    let mut password = b"--password=".to_vec();
    password.extend_from_slice(b"pw-\xff");
    let args = vec![
        b"qpdfjob".to_vec(),
        std::mem::take(&mut input_bytes),
        std::mem::take(&mut output_bytes),
        password,
    ];

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&args)
        .expect("raw Unix argv bytes must not pass through String");
    assert_eq!(job.input_name_bytes(), input.as_os_str().as_bytes());
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.is_file());

    let mut source = Pdf::open(BufReader::new(File::open(&input).unwrap())).unwrap();
    let mut writer = PdfWriter::new(&mut source);
    writer.set_static_id(true);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(
        b"pw-\xff".to_vec(),
        b"owner".to_vec(),
    ));
    writer.set_output_file(&encrypted).unwrap();
    writer.write().unwrap();

    let mut encrypted_job = QPDFJob::new();
    encrypted_job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            encrypted.as_os_str().as_bytes().to_vec(),
            decrypted.as_os_str().as_bytes().to_vec(),
            b"--password-mode=bytes".to_vec(),
            b"--password=pw-\xff".to_vec(),
        ])
        .expect("raw password bytes must reach the reader unchanged");
    assert_eq!(encrypted_job.run().unwrap(), JobExitCode::Success);
    assert!(decrypted.is_file());
}

#[test]
fn raw_argv_initializer_owns_every_main_table_option() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let password_file = tempdir.path().join("password.txt");
    std::fs::write(&password_file, b"secret\nignored\n").unwrap();
    let pass1 = tempdir.path().join("pass1.pdf");
    let update = tempdir.path().join("update.json");
    std::fs::write(&update, b"{}").unwrap();

    let bare_options: &[&[u8]] = &[
        b"--allow-weak-crypto",
        b"--check",
        b"--check-linearization",
        b"--coalesce-contents",
        b"--decrypt",
        b"--deterministic-id",
        b"--externalize-inline-images",
        b"--filtered-stream-data",
        b"--flatten-rotation",
        b"--generate-appearances",
        b"--ignore-xref-streams",
        b"--is-encrypted",
        b"--json-input",
        b"--keep-inline-images",
        b"--linearize",
        b"--list-attachments",
        b"--newline-before-endstream",
        b"--no-original-object-ids",
        b"--no-warn",
        b"--optimize-images",
        b"--password-is-hex-key",
        b"--preserve-unreferenced",
        b"--preserve-unreferenced-resources",
        b"--progress",
        b"--qdf",
        b"--raw-stream-data",
        b"--recompress-flate",
        b"--remove-page-labels",
        b"--report-memory-usage",
        b"--requires-password",
        b"--remove-restrictions",
        b"--show-encryption",
        b"--show-encryption-key",
        b"--show-linearization",
        b"--show-npages",
        b"--show-pages",
        b"--show-xref",
        b"--static-aes-iv",
        b"--static-id",
        b"--suppress-password-recovery",
        b"--suppress-recovery",
        b"--test-json-schema",
        b"--verbose",
        b"--warning-exit-0",
        b"--with-images",
    ];
    for option in bare_options {
        let mut args = vec![
            b"qpdfjob".to_vec(),
            input.to_string_lossy().into_owned().into_bytes(),
            option.to_vec(),
        ];
        add_raw_argv_output_if_required(&mut args);
        let mut job = QPDFJob::new();
        job.set_logger(QPDFLogger::create());
        job.initialize_from_raw_argv(&args)
            .unwrap_or_else(|error| panic!("{option:?} must be accepted: {error}"));
    }

    let parameter_options: Vec<Vec<u8>> = vec![
        b"--compression-level=6".to_vec(),
        format!("--copy-encryption={}", input.display()).into_bytes(),
        b"--encryption-file-password=secret".to_vec(),
        b"--force-version=1.3".to_vec(),
        b"--ii-min-bytes=1024".to_vec(),
        format!("--linearize-pass1={}", pass1.display()).into_bytes(),
        b"--min-version=1.3".to_vec(),
        b"--oi-min-area=16384".to_vec(),
        b"--oi-min-height=128".to_vec(),
        b"--oi-min-width=128".to_vec(),
        b"--password=secret".to_vec(),
        format!("--password-file={}", password_file.display()).into_bytes(),
        b"--remove-attachment=missing-key".to_vec(),
        b"--rotate=+90:1".to_vec(),
        b"--show-attachment=missing-key".to_vec(),
        b"--show-object=trailer".to_vec(),
        b"--json-stream-prefix=stream".to_vec(),
        format!("--update-from-json={}", update.display()).into_bytes(),
        b"--collate=2,3".to_vec(),
        b"--split-pages=2".to_vec(),
        b"--compress-streams=y".to_vec(),
        b"--decode-level=generalized".to_vec(),
        b"--flatten-annotations=screen".to_vec(),
        b"--json-key=pages".to_vec(),
        b"--json-stream-data=inline".to_vec(),
        b"--keep-files-open=y".to_vec(),
        b"--normalize-content=n".to_vec(),
        b"--object-streams=preserve".to_vec(),
        b"--password-mode=bytes".to_vec(),
        b"--remove-unreferenced-resources=auto".to_vec(),
        b"--stream-data=preserve".to_vec(),
        b"--json=1".to_vec(),
        b"--json-output=2".to_vec(),
    ];
    for option in parameter_options {
        let mut args = vec![
            b"qpdfjob".to_vec(),
            input.to_string_lossy().into_owned().into_bytes(),
        ];
        if option == b"--json-key=pages" {
            args.push(b"--json=2".to_vec());
        }
        args.push(option.clone());
        add_raw_argv_output_if_required(&mut args);
        let mut job = QPDFJob::new();
        job.set_logger(QPDFLogger::create());
        job.initialize_from_raw_argv(&args)
            .unwrap_or_else(|error| panic!("{option:?} must be accepted: {error}"));
    }

    let mut empty_job = QPDFJob::new();
    let mut empty_args = vec![b"qpdfjob".to_vec(), b"--empty".to_vec()];
    add_raw_argv_output_if_required(&mut empty_args);
    empty_job
        .initialize_from_raw_argv(&empty_args)
        .expect("--empty with an output path should initialize");
    let mut replace_job = QPDFJob::new();
    replace_job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            input.to_string_lossy().into_owned().into_bytes(),
            b"--replace-input".to_vec(),
        ])
        .unwrap();
}

#[test]
fn raw_argv_initializer_preserves_qpdf_parameter_and_choice_boundaries() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            input.to_string_lossy().into_owned().into_bytes(),
            b"--rotate".to_vec(),
            b"+90:1".to_vec(),
        ])
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "--rotate must be given as --rotate=[+|-]angle"
    ));

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            input.to_string_lossy().into_owned().into_bytes(),
            b"--object-streams=maybe".to_vec(),
        ])
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "--object-streams must be given as --object-streams={disable,preserve,generate}"
    ));

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        b"-".to_vec(),
        b"-qdf".to_vec(),
        b"out.pdf".to_vec(),
    ])
    .expect("qpdf accepts single-dash spellings for registered options");
}

#[test]
fn raw_argv_initializer_accepts_the_qpdf_help_table_as_a_sole_option() {
    for option in [
        b"--version".to_vec(),
        b"--copyright".to_vec(),
        b"--show-crypto".to_vec(),
        b"--job-json-help".to_vec(),
        b"--json-help=latest".to_vec(),
        b"--help".to_vec(),
    ] {
        let mut job = QPDFJob::new();
        job.initialize_from_raw_argv(&[b"qpdfjob".to_vec(), option.clone()])
            .unwrap_or_else(|error| panic!("{option:?} must be accepted: {error}"));
        assert_eq!(job.run().unwrap(), JobExitCode::Success);
    }
}

#[test]
fn raw_argv_stdin_file_helper() {
    let Some(mode) = std::env::var_os("FLPDF_RAW_ARGV_STDIN_HELPER") else {
        return;
    };
    let input = std::env::var_os("FLPDF_RAW_ARGV_STDIN_INPUT")
        .expect("stdin helper input path must be provided")
        .into_string()
        .expect("stdin helper input path must be UTF-8");
    let args = match mode.to_str() {
        Some("argfile") => vec![b"qpdfjob".to_vec(), b"@-".to_vec()],
        Some("password") => vec![
            b"qpdfjob".to_vec(),
            input.into_bytes(),
            b"--password-file=-".to_vec(),
            b"-".to_vec(),
        ],
        _ => panic!("unknown stdin helper mode: {mode:?}"),
    };
    QPDFJob::new()
        .initialize_from_raw_argv(&args)
        .expect("stdin-backed argv file should initialize");
}

#[test]
fn raw_argv_initializer_reads_stdin_for_argfile_and_password_file() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = input.to_string_lossy().into_owned();
    let executable = std::env::current_exe().unwrap();
    for (mode, contents) in [
        ("argfile", format!("{input}\n--show-npages\n").into_bytes()),
        ("password", b"secret\r\nignored\n".to_vec()),
    ] {
        let mut child = Command::new(&executable)
            .args(["--exact", "raw_argv_stdin_file_helper", "--nocapture"])
            .env("FLPDF_RAW_ARGV_STDIN_HELPER", mode)
            .env("FLPDF_RAW_ARGV_STDIN_INPUT", &input)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .expect("stdin helper pipe should be available")
            .write_all(&contents)
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "stdin helper {mode} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn raw_argv_show_npages_matches_the_qpdf_11_9_0_boundary() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let qpdf = Command::new("qpdf")
        .arg("--show-npages")
        .arg(&input)
        .output();
    let Ok(qpdf) = qpdf else {
        return;
    };
    assert!(qpdf.status.success());

    let (logger, info) = logger_with_info_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        input.to_string_lossy().into_owned().into_bytes(),
        b"--show-npages".to_vec(),
    ])
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert_eq!(
        normalize_text_newlines(&info.lock().unwrap().bytes),
        normalize_text_newlines(&qpdf.stdout)
    );
}

#[test]
fn raw_argv_writer_matches_qpdf_process_contract() {
    let version = Command::new("qpdf").arg("--version").output();
    if !version.as_ref().is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).starts_with("qpdf version 11.9.0")
    }) {
        return;
    }

    let tempdir = tempfile::tempdir().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let qpdf_output = tempdir.path().join("qpdf-output.pdf");
    let flpdf_output = tempdir.path().join("flpdf-output.pdf");
    let flags = [
        "--static-id",
        "--qdf",
        "--object-streams=disable",
        "--stream-data=uncompress",
    ];
    let qpdf = Command::new("qpdf")
        .args(flags)
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .unwrap();
    assert!(qpdf.status.success(), "qpdf failed: {qpdf:?}");

    let args = vec![
        b"qpdfjob".to_vec(),
        input.as_os_str().to_string_lossy().as_bytes().to_vec(),
        flpdf_output
            .as_os_str()
            .to_string_lossy()
            .as_bytes()
            .to_vec(),
        b"--static-id".to_vec(),
        b"--qdf".to_vec(),
        b"--object-streams=disable".to_vec(),
        b"--stream-data=uncompress".to_vec(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&args)
        .expect("raw argv should configure the qpdf writer");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert_eq!(
        std::fs::read(flpdf_output).unwrap(),
        std::fs::read(qpdf_output).unwrap(),
        "raw QPDFJob argv writer must match qpdf 11.9.0"
    );
}

#[test]
fn raw_argv_initializer_covers_remaining_choice_and_segment_boundaries() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let input = input.to_string_lossy().into_owned().into_bytes();

    let accept = |mut args: Vec<Vec<u8>>| {
        args.insert(1, input.clone());
        add_raw_argv_output_if_required(&mut args);
        let mut job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        logger.set_save(Some(logger.discard()), false).unwrap();
        job.set_logger(logger);
        job.initialize_from_raw_argv(&args)
            .unwrap_or_else(|error| panic!("argv {:?} must be accepted: {error}", args));
    };

    for option in [
        b"--decode-level=none".to_vec(),
        b"--decode-level=specialized".to_vec(),
        b"--decode-level=all".to_vec(),
        b"--flatten-annotations=all".to_vec(),
        b"--flatten-annotations=print".to_vec(),
        b"--json-stream-data=none".to_vec(),
        b"--json-stream-data=file".to_vec(),
        b"--password-mode=hex-bytes".to_vec(),
        b"--password-mode=unicode".to_vec(),
        b"--password-mode=auto".to_vec(),
        b"--remove-unreferenced-resources=yes".to_vec(),
        b"--remove-unreferenced-resources=no".to_vec(),
        b"--stream-data=compress".to_vec(),
        b"--stream-data=uncompress".to_vec(),
        b"--compress-streams=n".to_vec(),
        b"--normalize-content=y".to_vec(),
        b"--json=latest".to_vec(),
        b"--json=2".to_vec(),
        b"--json-output".to_vec(),
        b"--json-output=latest".to_vec(),
        b"--json-object=trailer".to_vec(),
    ] {
        accept(vec![b"qpdfjob".to_vec(), option]);
    }
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--json=1".to_vec(),
        b"--json-key=objects".to_vec(),
    ]);

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf")
        .to_string_lossy()
        .into_owned()
        .into_bytes();
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--underlay".to_vec(),
        fixture.clone(),
        b"--".to_vec(),
    ]);
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--pages".to_vec(),
        b"--file=.".to_vec(),
        b"--range=1".to_vec(),
        b"--password=page-password".to_vec(),
        b"--".to_vec(),
    ]);

    for key_length in [b"40".as_slice(), b"128".as_slice(), b"256".as_slice()] {
        let mut args = vec![b"qpdfjob".to_vec(), b"--allow-weak-crypto".to_vec()];
        let print = if key_length == b"40" {
            b"--print=y".to_vec()
        } else {
            b"--print=low".to_vec()
        };
        let modify = if key_length == b"40" {
            b"--modify=y".to_vec()
        } else {
            b"--modify=none".to_vec()
        };
        let mut segment = vec![b"--encrypt".to_vec(), b"user".to_vec(), b"owner".to_vec()];
        segment.push(key_length.to_vec());
        segment.extend_from_slice(&[
            print,
            modify,
            b"--extract=n".to_vec(),
            b"--annotate=n".to_vec(),
        ]);
        if key_length == b"128" {
            segment.extend_from_slice(&[
                b"--force-V4".to_vec(),
                b"--use-aes=n".to_vec(),
                b"--form=n".to_vec(),
                b"--assemble=n".to_vec(),
                b"--accessibility=n".to_vec(),
                b"--modify-other=n".to_vec(),
            ]);
        } else if key_length == b"256" {
            segment.push(b"--force-R5".to_vec());
        }
        segment.push(b"--".to_vec());
        args.extend(segment);
        accept(args);
    }
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--encrypt".to_vec(),
        b"--user-password=user".to_vec(),
        b"--owner-password=owner".to_vec(),
        b"--bits=128".to_vec(),
        b"--use-aes=y".to_vec(),
        b"--".to_vec(),
    ]);

    let reject = |args: Vec<Vec<u8>>| {
        let mut job = QPDFJob::new();
        assert!(
            job.initialize_from_raw_argv(&args).is_err(),
            "argv should be rejected: {args:?}"
        );
    };
    reject(Vec::new());
    reject(vec![b"qpdfjob".to_vec(), b"-x".to_vec()]);
    reject(vec![b"qpdfjob".to_vec(), b"--pages".to_vec()]);
    reject(vec![b"qpdfjob".to_vec(), b"--encrypt".to_vec()]);
    reject(vec![b"qpdfjob".to_vec(), b"--underlay".to_vec()]);
    reject(vec![b"qpdfjob".to_vec(), b"--add-attachment".to_vec()]);
    reject(vec![
        b"qpdfjob".to_vec(),
        b"--copy-attachments-from".to_vec(),
    ]);
    reject(vec![b"qpdfjob".to_vec(), b"--set-page-labels".to_vec()]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--add-attachment".to_vec(),
        fixture.clone(),
        b"--mimetype=textplain".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--add-attachment".to_vec(),
        fixture.clone(),
        b"--creationdate=not-a-date".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--copy-attachments-from".to_vec(),
        b"--unknown".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--password=before-file".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"--use-aes".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![b"qpdfjob".to_vec(), input, b"--json=3".to_vec()]);
}

#[test]
fn raw_argv_initializer_covers_error_and_choice_boundaries() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/one-page.pdf")
        .to_string_lossy()
        .into_owned()
        .into_bytes();
    let accept = |mut args: Vec<Vec<u8>>| {
        args.insert(1, input.clone());
        add_raw_argv_output_if_required(&mut args);
        let mut job = QPDFJob::new();
        let logger = QPDFLogger::create();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_error(Some(logger.discard()));
        logger.set_save(Some(logger.discard()), false).unwrap();
        job.set_logger(logger);
        job.initialize_from_raw_argv(&args)
            .unwrap_or_else(|error| panic!("argv {:?} must be accepted: {error}", args));
    };
    let reject = |args: Vec<Vec<u8>>| {
        assert!(
            QPDFJob::new().initialize_from_raw_argv(&args).is_err(),
            "argv should be rejected: {args:?}"
        );
    };

    // Main-table value/choice branches not used by the broad acceptance
    // sweep above.
    for option in [
        b"--decode-level=none".to_vec(),
        b"--decode-level=specialized".to_vec(),
        b"--decode-level=all".to_vec(),
        b"--flatten-annotations=all".to_vec(),
        b"--flatten-annotations=print".to_vec(),
        b"--json-stream-data=none".to_vec(),
        b"--json-stream-data=file".to_vec(),
        b"--password-mode=hex-bytes".to_vec(),
        b"--password-mode=unicode".to_vec(),
        b"--password-mode=auto".to_vec(),
        b"--remove-unreferenced-resources=yes".to_vec(),
        b"--remove-unreferenced-resources=no".to_vec(),
        b"--stream-data=compress".to_vec(),
        b"--stream-data=uncompress".to_vec(),
        b"--compress-streams=n".to_vec(),
        b"--normalize-content=y".to_vec(),
        b"--json=latest".to_vec(),
        b"--json=2".to_vec(),
        b"--json-output".to_vec(),
        b"--json-output=latest".to_vec(),
        b"--json-object=trailer".to_vec(),
    ] {
        accept(vec![b"qpdfjob".to_vec(), option]);
    }
    for key in [
        b"acroform".as_slice(),
        b"attachments".as_slice(),
        b"encrypt".as_slice(),
        b"objectinfo".as_slice(),
        b"outlines".as_slice(),
        b"pagelabels".as_slice(),
        b"pages".as_slice(),
        b"qpdf".as_slice(),
    ] {
        let version = if matches!(key, b"objects" | b"objectinfo") {
            b"--json=1".to_vec()
        } else {
            b"--json=2".to_vec()
        };
        accept(vec![
            b"qpdfjob".to_vec(),
            version,
            [b"--json-key=".as_slice(), key].concat(),
        ]);
    }

    // Error branches at the raw argv boundary.
    reject(Vec::new());
    reject(vec![b"qpdfjob".to_vec(), b"-x".to_vec()]);
    reject(vec![
        b"qpdfjob".to_vec(),
        b"input.pdf".to_vec(),
        b"extra.pdf".to_vec(),
        b"x".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--empty".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--replace-input".to_vec(),
        b"out.pdf".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--json=3".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--json-output=1".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--compression-level=999999999999999999999999".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--password=x".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"file.pdf".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--range=bad".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"file.pdf".to_vec(),
        b"--range=1".to_vec(),
        b"--range=2".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--unknown".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--overlay".to_vec(),
        b"--file=a".to_vec(),
        b"--file=b".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--overlay".to_vec(),
        b"--to=bad".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--add-attachment".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--copy-attachments-from".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--set-page-labels".to_vec(),
        b"bad".to_vec(),
        b"--".to_vec(),
    ]);

    let password_dir = tempfile::tempdir().unwrap();
    let empty_password = password_dir.path().join("empty-password");
    std::fs::write(&empty_password, b"").unwrap();
    let mut password_file_args = vec![b"qpdfjob".to_vec()];
    password_file_args.push(format!("--password-file={}", empty_password.display()).into_bytes());
    accept(password_file_args);

    let missing_job_json = password_dir.path().join("missing-job.json");
    let error = QPDFJob::new()
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            format!("--job-json-file={}", missing_job_json.display()).into_bytes(),
        ])
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage) if usage.to_string().contains("error with job-json file")
    ));

    let pages_job_json = password_dir.path().join("pages-job.json");
    std::fs::write(
        &pages_job_json,
        serde_json::json!({
            "pages": [{"file": String::from_utf8_lossy(&input)}]
        })
        .to_string(),
    )
    .unwrap();
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        format!("--job-json-file={}", pages_job_json.display()).into_bytes(),
        b"--pages".to_vec(),
        b"--".to_vec(),
    ]);

    // Exercise the page-table file/range/password callbacks, including the
    // qpdf positional range-versus-file fallback and repeated-source path.
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--pages".to_vec(),
        b"--file=.".to_vec(),
        b"--".to_vec(),
    ]);
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--pages".to_vec(),
        b".".to_vec(),
        b"1".to_vec(),
        input.clone(),
        b"--".to_vec(),
    ]);
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--pages".to_vec(),
        b".".to_vec(),
        input.clone(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b".".to_vec(),
        vec![0xff],
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--file=.".to_vec(),
        b"--password=first".to_vec(),
        b"--password=second".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"--bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b"---bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--pages".to_vec(),
        b".".to_vec(),
        b"not-a-range-or-file".to_vec(),
        b"--".to_vec(),
    ]);

    // Underlay is the second half of the shared UO table; all option forms are
    // accepted before the terminator, and the same state reports its own
    // table-specific failures.
    reject(vec![
        b"qpdfjob".to_vec(),
        b"--underlay".to_vec(),
        b"--file=".to_vec(),
        b"--".to_vec(),
    ]);
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--underlay".to_vec(),
        format!("--file={}", String::from_utf8_lossy(&input)).into_bytes(),
        b"--password=pw".to_vec(),
        b"--to=1".to_vec(),
        b"--from=".to_vec(),
        b"--repeat=".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--file=a".to_vec(),
        b"--file=b".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"---bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--from=bad!".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--from=\xff".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--file=a".to_vec(),
        b"b".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--underlay".to_vec(),
        b"--".to_vec(),
    ]);

    // Cover the attachment defaults/date validator and copy-attachment
    // boundary without invoking the output stage.
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--add-attachment".to_vec(),
        input.clone(),
        b"--creationdate=D:20240101120000".to_vec(),
        b"--moddate=D:20240101120000+01'00'".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--add-attachment".to_vec(),
        input.clone(),
        b"--bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--add-attachment".to_vec(),
        input.clone(),
        b"---bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--copy-attachments-from".to_vec(),
        Vec::new(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--copy-attachments-from".to_vec(),
        b"---bogus".to_vec(),
        b"--".to_vec(),
    ]);

    // Encryption table selection is immediate after the key length. Exercise
    // the R=6 path, dashed form, mixed-mode errors, and dynamic-table errors.
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"256".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"--user-password=user".to_vec(),
        b"value".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"--user-password=user".to_vec(),
        b"--use-aes=y".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"extra".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"999".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"".to_vec(),
        b"256".to_vec(),
        b"--".to_vec(),
    ]);
    for key_length in [b"40".as_slice(), b"256".as_slice()] {
        reject(vec![
            b"qpdfjob".to_vec(),
            input.clone(),
            b"--encrypt".to_vec(),
            b"user".to_vec(),
            b"owner".to_vec(),
            key_length.to_vec(),
            b"--bogus".to_vec(),
            b"--".to_vec(),
        ]);
    }
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"--bits=256".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"---bogus".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"--".to_vec(),
    ]);
    accept(vec![
        b"qpdfjob".to_vec(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"--print=none".to_vec(),
        b"--modify=assembly".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--encrypt".to_vec(),
        b"user".to_vec(),
        b"owner".to_vec(),
        b"128".to_vec(),
        b"--use-aes=x".to_vec(),
        b"--".to_vec(),
    ]);
    reject(vec![
        b"qpdfjob".to_vec(),
        input.clone(),
        b"--split-pages=999999999999999999999999".to_vec(),
    ]);

    let mut empty_positional = QPDFJob::new();
    empty_positional
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            Vec::new(),
            b"raw-argv-output.pdf".to_vec(),
        ])
        .expect("an empty positional input with an output path should initialize");
    reject(vec![b"qpdfjob".to_vec(), b"---bad".to_vec()]);
    reject(vec![b"qpdfjob".to_vec(), b"--json-help=3".to_vec()]);
    for option in [b"--json-help=1".to_vec(), b"--json-help=2".to_vec()] {
        let mut job = QPDFJob::new();
        job.set_logger(QPDFLogger::create());
        job.initialize_from_raw_argv(&[b"qpdfjob".to_vec(), option])
            .unwrap();
    }
}

/// A missing `--job-json-file` path must render qpdf's portable
/// `strerror`-based wording (`QUtil::safe_fopen`, `libqpdf/QUtil.cc:490-519`;
/// `QPDFSystemError::createWhat`, `libqpdf/QPDFSystemError.cc:13-29`), not
/// Rust's `std::io::Error` text with its `(os error N)` suffix. Confirmed
/// against `/usr/bin/qpdf` 11.9.0:
/// `qpdf --job-json-file=<missing> in.pdf out.pdf` prints exactly
/// `error with job-json file <path>: open <path>: No such file or
/// directory\nRun qpdf --job-json-help for information on the file format.`
/// before the CLI's own `usageExit` footer is appended.
#[test]
fn raw_argv_missing_job_json_file_uses_qpdf_strerror_wording() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-job.json");

    let error = QPDFJob::new()
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            format!("--job-json-file={}", missing.display()).into_bytes(),
        ])
        .unwrap_err();
    let Error::Usage(usage) = error else {
        panic!("expected a usage error, got {error:?}");
    };
    assert_eq!(
        usage.to_string(),
        format!(
            "error with job-json file {}: open {}: {}\n\
             Run qpdfjob --job-json-help for information on the file format.",
            missing.display(),
            missing.display(),
            missing_file_strerror_text(&missing),
        )
    );
}

/// Render the same platform strerror wording [`crate::qutil::strerror_text`]
/// (private to the library crate) produces for a missing path, without
/// hardcoding a POSIX-only message that would fail on Windows.
fn missing_file_strerror_text(missing: &Path) -> String {
    let io_error = std::fs::File::open(missing).expect_err("path must not exist");
    let rendered = io_error.to_string();
    io_error
        .raw_os_error()
        .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
        .map_or(rendered.clone(), str::to_owned)
}

/// A nested `jobJsonFile` JSON key dispatches through the same
/// `Config::jobJsonFile` callback as `--job-json-file`
/// (`libqpdf/qpdf/auto_job_json_init.hh:472-474`), so a missing include must
/// use the same qpdf `strerror`-based wording as
/// `raw_argv_missing_job_json_file_uses_qpdf_strerror_wording` above.
/// Confirmed against `/usr/bin/qpdf` 11.9.0 (nested one layer via
/// `--job-json-file`): the inner "error with job-json file ...: open ...:
/// No such file or directory" text is unchanged; only the outer argv-level
/// wrapping (added by `apply_job_json_file`, not exercised by this
/// library-level call) differs.
#[test]
fn nested_missing_job_json_file_uses_qpdf_strerror_wording() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing-nested.json");
    let json = serde_json::json!({ "jobJsonFile": missing }).to_string();

    let error = QPDFJob::new()
        .initialize_from_json(&json)
        .expect_err("a missing nested jobJsonFile include must fail");
    assert_eq!(
        error.to_string(),
        format!(
            "error with job-json file {}: open {}: {}\n\
             Run qpdfjob json --job-json-help for information on the file format.",
            missing.display(),
            missing.display(),
            missing_file_strerror_text(&missing),
        )
    );
}

#[test]
fn page_label_order_errors_are_not_usage_errors() {
    // qpdf raises the three order/page-count failures with
    // `throw std::runtime_error(...)` (`libqpdf/QPDFJob.cc:2206,2211,2215`),
    // not `QPDFUsage`. Its CLI catches the two separately
    // (`qpdf/qpdf.cc:37-41`), so only a real usage error prints the banner.
    // Keeping these as usage errors adds nine lines qpdf never emits.
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    for (specs, expected) in [
        (
            vec!["2:D"],
            "the first page label specification must start with page 1",
        ),
        (
            vec!["1:D", "1:R"],
            "page label specifications must be in order by first page",
        ),
        (
            vec!["1:D", "9:D"],
            "page label spec: page 9 is more than the total number of pages (3)",
        ),
    ] {
        let (logger, errors) = logger_with_error_sink();
        logger.set_info(Some(logger.discard()));
        logger.set_warn(Some(logger.discard()));
        logger.set_save(Some(logger.discard()), false).unwrap();
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        job.initialize_from_json_partial(
            &serde_json::json!({
                "inputFile": path,
                "outputFile": "-",
                "setPageLabels": specs,
            })
            .to_string(),
        )
        .unwrap();
        // `run()` reports the failure and turns it into an exit status
        // (mirroring qpdf's CLI catch), so check what reached the error sink.
        assert_eq!(job.run().unwrap(), JobExitCode::Error);
        let reported = String::from_utf8_lossy(&errors.lock().unwrap().bytes).to_string();
        assert!(
            reported.contains(expected),
            "expected qpdf's own wording, got: {reported}"
        );
        assert!(
            !reported.contains("For help:"),
            "qpdf raises this with a plain runtime error, so no usage banner: {reported}"
        );
    }
}

fn page_label_fixture(root_body: Option<&str>) -> Vec<u8> {
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let root = root_body.unwrap_or("<< /Type /Catalog /Pages 2 0 R >>");
    for object in [
        format!("1 0 obj\n{root}\nendobj\n"),
        "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_string(),
    ] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object.as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    bytes
}

#[test]
fn page_labels_skip_a_catalog_that_is_not_a_dictionary() {
    // qpdf reaches `pdf.getRoot().replaceKey("/PageLabels", ...)`
    // (`libqpdf/QPDFJob.cc:2228`) only through a real Catalog; a `/Root` that
    // resolves to a non-dictionary leaves the label tree alone rather than
    // failing the job.
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("non-dict-root.pdf");
    let output = tempdir.path().join("out.pdf");
    std::fs::write(&input, page_label_fixture(Some("42"))).unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": input,
            "outputFile": output,
            "staticId": "",
            "setPageLabels": ["1:D"],
        })
        .to_string(),
    )
    .unwrap();
    let _ = job.run();

    if let Ok(written) = std::fs::read(&output) {
        assert!(
            !String::from_utf8_lossy(&written).contains("/PageLabels"),
            "a non-dictionary catalog must not receive a label tree"
        );
    }
}

#[test]
fn an_empty_page_label_spec_set_leaves_the_catalog_alone() {
    // qpdf guards the rebuild on a non-empty vector
    // (`if (!m->page_label_specs.empty())`, `libqpdf/QPDFJob.cc:2199`), so
    // `--set-page-labels --` with no specs must not install `<< /Nums [] >>`.
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.pdf");
    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": path,
            "outputFile": output,
            "staticId": "",
            "setPageLabels": Vec::<String>::new(),
        })
        .to_string(),
    )
    .unwrap();
    job.run()
        .expect("an empty spec set is a no-op, not an error");

    let written = std::fs::read(&output).unwrap();
    assert!(
        !String::from_utf8_lossy(&written).contains("/PageLabels"),
        "an empty spec set must leave the catalog untouched"
    );
}

#[test]
fn argv_page_label_config_rejects_invalid_specs_at_initialization() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            "--set-page-labels".to_owned(),
            "quack".to_owned(),
            "--".to_owned(),
        ])
        .expect_err("invalid page-label specs must fail in the Config boundary");
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "page label spec must be n:[D|a|A|r|R][/start[/prefix]]"
    ));
}

#[test]
fn argv_page_label_option_table_rejects_an_option_before_its_terminator() {
    for option in ["--remove-page-labels", "-1:D"] {
        let mut job = QPDFJob::new();
        let error = job
            .initialize_from_argv(&[
                "qpdfjob".to_owned(),
                "input.pdf".to_owned(),
                "output.pdf".to_owned(),
                "--set-page-labels".to_owned(),
                "1:D".to_owned(),
                option.to_owned(),
            ])
            .expect_err("page-label specs must reject options before --");
        assert!(matches!(
            &error,
            Error::Usage(usage)
                if usage.to_string()
                    == format!(
                        "unrecognized argument {option} (set page labels options must be terminated with --)"
                    )
        ));
    }
}

#[test]
fn argv_page_label_option_table_requires_a_terminator() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
            "--set-page-labels".to_owned(),
            "1:D".to_owned(),
        ])
        .expect_err("page-label specs must be terminated with --");
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "missing -- at end of set page labels options"
    ));
}

#[test]
fn argv_page_label_invalid_spec_without_terminator_reports_missing_terminator() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "--set-page-labels".to_owned(),
            "quack".to_owned(),
            "input.pdf".to_owned(),
            "output.pdf".to_owned(),
        ])
        .expect_err("missing terminator must win over deferred spec validation");
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "missing -- at end of set page labels options"
    ));
}

#[test]
fn argv_job_run_writes_output_and_reports_progress() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("argv-output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--deterministic-id".to_owned(),
        "--progress".to_owned(),
        "--password=unused".to_owned(),
        "--decrypt".to_owned(),
        "--object-streams=disable".to_owned(),
        "--".to_owned(),
    ];
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_for_job = Arc::clone(&progress);
    let mut job = QPDFJob::new();
    job.register_progress_reporter(move |percent| {
        progress_for_job.lock().unwrap().push(percent);
        Ok(())
    });
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
    let progress = progress.lock().unwrap();
    assert_eq!(progress.first(), Some(&0));
    assert_eq!(progress.last(), Some(&100));
}

#[test]
fn job_document_boundary_erases_file_empty_and_json_reader_kinds() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");

    let mut file_job = QPDFJob::new();
    let file_document: JobDocument = file_job
        .open_document(
            BufReader::new(File::open(&input).unwrap()),
            input.display().to_string(),
            PdfOpenOptions::default(),
        )
        .unwrap();

    let mut empty_job = QPDFJob::new();
    let empty_document: JobDocument = empty_job.create_empty_document().unwrap();

    let mut json_job = QPDFJob::new();
    let json_document: JobDocument = json_job
        .create_from_json_document(Cursor::new(COMPLETE_JSON), "input.json")
        .unwrap();

    assert_eq!(file_document.version(), "1.7");
    assert_eq!(empty_document.version(), "1.3");
    assert_eq!(json_document.version(), "1.3");
}

#[test]
fn json_job_empty_input_uses_the_job_document_boundary() {
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("empty.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": ""
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(pdf.version(), "1.3");
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 0);
}

#[test]
fn json_job_empty_encryption_status_returns_qpdf_exit_code() {
    for option in ["isEncrypted", "requiresPassword"] {
        let json = serde_json::json!({
            "empty": "",
            option: ""
        })
        .to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json(&json).unwrap();

        assert_eq!(job.run().unwrap(), JobExitCode::Error);
    }
}

#[test]
fn argv_job_run_returns_warning_status_for_repairable_input() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("warning-output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--static-id".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(output.exists());
}

#[test]
fn xref_stream_extra_data_produces_qpdf_warning_status() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("xref-extra.pdf");
    let output = tempdir.path().join("xref-extra-output.pdf");
    std::fs::write(&input, xref_stream_with_extra_data()).unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--static-id".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(output.exists());
}

#[test]
fn json_job_run_writes_output_with_static_id_and_generated_object_streams() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("json-output.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "staticId": "",
        "decrypt": "",
        "progress": "",
        "objectStreams": "generate"
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
    let bytes = std::fs::read(output).unwrap();
    assert!(bytes
        .windows(b"/Type /ObjStm".len())
        .any(|window| { window == b"/Type /ObjStm" }));
}

#[test]
fn json_job_run_applies_pages_and_attachments() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("pages-and-attachment.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "pages": [{"file": fixture}],
        "addAttachment": [{"file": fixture, "key": "fixture-key"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 1);
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"fixture-key")
        .unwrap()
        .is_some());
}

#[test]
fn json_job_nested_job_json_file_retains_outer_and_inner_attachments() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let nested = tempdir.path().join("nested.json");
    let output = tempdir.path().join("nested-attachments.pdf");
    std::fs::write(
        &nested,
        serde_json::json!({
            "addAttachment": [{"file": fixture, "key": "inner-key"}]
        })
        .to_string(),
    )
    .unwrap();
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "jobJsonFile": nested,
        "addAttachment": [{"file": fixture, "key": "outer-key"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"inner-key")
        .unwrap()
        .is_some());
    assert!(pdf
        .embedded_files()
        .get_embedded_file(b"outer-key")
        .unwrap()
        .is_some());
}

#[test]
fn json_job_nested_job_json_file_rejects_repeated_pages() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let nested = tempdir.path().join("nested-pages.json");
    let output = tempdir.path().join("nested-pages.pdf");
    std::fs::write(
        &nested,
        serde_json::json!({
            "pages": [{"file": fixture}]
        })
        .to_string(),
    )
    .unwrap();
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "jobJsonFile": nested,
        "pages": [{"file": fixture}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_json(&json)
        .expect_err("qpdf allows --pages only once across nested job JSON");
    assert!(matches!(
        error,
        Error::Usage(usage) if usage.to_string() == "--pages may only be specified one time"
    ));
}

/// qpdf's `handleRotations` resolves each `--rotate` range against the real
/// page count and then filters `0 <= pageno < npages` before touching
/// `pages`, so a `--collate=0`-produced empty document rotates nothing
/// without erroring (confirmed live: `--collate=0 --rotate=90` exits 0).
#[test]
fn json_job_run_applies_rotate_to_a_collate_zero_empty_page_selection() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("collate-zero-rotate.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "pages": [{"file": fixture}, {"file": fixture}],
        "collate": "0",
        "rotate": "+90"
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 0);
}

#[test]
fn json_job_run_applies_relative_rotation_to_a_real_page() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("relative-rotate.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "pages": [{"file": fixture}],
        "rotate": "+90"
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    page.try_is_scalar().unwrap();
    assert_eq!(page.try_get_key(b"/Rotate").unwrap().as_integer(), Some(90));
}

/// qpdf leaves `createQPDF` before any stage when the job only reports
/// encryption status: the `check_is_encrypted || check_requires_password`
/// early return (`libqpdf/QPDFJob.cc:455-456`) sits ahead of `updateFromJSON`
/// (`:462`) and `handleRotations` (`:470`). Configure both a rotation and an
/// update file that does not exist: without the guard the update would fail
/// the call outright, and a document meant only for inspection would come
/// back rotated.
#[test]
fn create_qpdf_skips_the_stages_for_an_encryption_status_job() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/complete.json");
    let tempdir = tempfile::tempdir().unwrap();
    let json = serde_json::json!({
        "inputFile": fixture,
        "jsonInput": "",
        "isEncrypted": "",
        "updateFromJson": tempdir.path().join("missing-update.json"),
        "rotate": "+90"
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(&json).unwrap();
    let mut pdf = job
        .create_qpdf()
        .expect("a status-only job must not fail on the unread update file")
        .expect("createQPDF should still return the document");

    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    page.try_is_scalar().unwrap();
    assert_eq!(
        page.try_get_key(b"/Rotate").unwrap().as_integer(),
        None,
        "an encryption-status job must not rotate the document it only inspects"
    );
}

#[test]
fn create_qpdf_returns_the_primary_after_rotation_transformation() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/complete.json");
    let update = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/update.json");
    let tempdir = tempfile::tempdir().unwrap();
    let json = serde_json::json!({
        "inputFile": fixture,
        "jsonInput": "",
        "outputFile": tempdir.path().join("later.pdf"),
        "updateFromJson": update,
        "rotate": "+90"
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(&json).unwrap();
    let mut pdf = job
        .create_qpdf()
        .unwrap()
        .expect("createQPDF should return the transformed document");

    let page_ref = flpdf::pages::page_refs(&mut pdf).unwrap()[0];
    let page = pdf.get_object_handle(page_ref);
    page.try_is_scalar().unwrap();
    assert_eq!(page.try_get_key(b"/Rotate").unwrap().as_integer(), Some(90));
}

#[test]
fn create_qpdf_applies_page_selection_before_returning() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let json = serde_json::json!({
        "inputFile": fixture,
        "outputFile": tempdir.path().join("selected.pdf"),
        "pages": [{"file": ".", "range": "1"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    let mut pdf = job
        .create_qpdf()
        .unwrap()
        .expect("createQPDF should return the page-selected document");

    assert_eq!(
        flpdf::pages::page_refs(&mut pdf).unwrap().len(),
        1,
        "qpdf createQPDF returns after handlePageSpecs, before writeQPDF"
    );
}

#[test]
fn create_qpdf_write_qpdf_preserves_a_between_stage_mutation() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("between-stage.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "rotate": "+90"
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    let mut pdf = job
        .create_qpdf()
        .unwrap()
        .expect("createQPDF should return the prepared document");

    let root = pdf.root_handle().unwrap();
    assert_eq!(
        flpdf::pages::page_refs(&mut pdf).unwrap().len(),
        1,
        "the create stage must have prepared the primary before returning"
    );
    root.replace_key(b"/BetweenStages", ObjectHandle::boolean(true))
        .unwrap();

    job.write_qpdf(&mut pdf).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);

    let mut written = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    let written_root = written.root_handle().unwrap();
    assert_eq!(
        written_root
            .try_get_key(b"/BetweenStages")
            .unwrap()
            .as_boolean(),
        Some(true),
        "writeQPDF must consume the document returned by createQPDF"
    );
}

#[test]
fn create_qpdf_returns_an_erased_multi_source_document_for_later_write() {
    let primary =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let secondary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("multi-source.pdf");
    let json = serde_json::json!({
        "inputFile": primary,
        "outputFile": output,
        "pages": [
            {"file": ".", "range": "1"},
            {"file": secondary, "range": "1"}
        ]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    let mut pdf = job
        .create_qpdf()
        .unwrap()
        .expect("createQPDF should return an erased merged document");
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 2);

    job.write_qpdf(&mut pdf).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);
    let mut written = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut written).unwrap().len(), 2);
}

/// A multi-source merge replaces `create_qpdf`'s returned document with a
/// fresh, unencrypted target (`docs/qpdf-correspondence.md`, `flpdf-clq9`),
/// so a caller that still needs the encrypted primary's own encryption bits
/// after `create_qpdf` returns cannot read them back from that document.
/// `QPDFJob::encryption_status`/`take_primary_copy_encryption` expose the
/// pre-merge snapshot for exactly that case (flpdf-3yn9.48.192).
#[test]
fn create_qpdf_captures_the_primary_encryption_snapshot_before_a_multi_source_merge() {
    let primary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
    let secondary =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("multi-source-encrypted.pdf");
    let json = serde_json::json!({
        "inputFile": primary,
        "outputFile": output,
        "pages": [
            {"file": ".", "range": "1"},
            {"file": secondary, "range": "1"}
        ]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    let mut pdf = job
        .create_qpdf()
        .unwrap()
        .expect("createQPDF should return an erased merged document");

    // The fresh merged target cannot answer the primary's own question.
    assert!(
        !pdf.is_encrypted(),
        "the multi-source merge target is always a fresh, unencrypted document"
    );

    let (encrypted, password_incorrect) = job.encryption_status();
    assert!(
        encrypted,
        "the primary's own encryption bit must survive the merge"
    );
    assert!(!password_incorrect);

    let donor = job.take_primary_copy_encryption().expect(
        "a multi-source merge of an encrypted primary must snapshot a copy-encryption donor",
    );
    assert!(
        job.take_primary_copy_encryption().is_none(),
        "the snapshot is consumed by the first take"
    );

    // Feed the snapshot to a *separate* job's writer configuration, the way
    // a caller that completes the write on a different `QPDFJob` instance
    // (rather than this same job's own `write_qpdf`) must.
    let mut writer_configuration = WriterConfiguration::default();
    writer_configuration.copy_encryption_parameters(donor);
    let mut write_job = QPDFJob::new();
    write_job.set_output_file(&output).unwrap();
    write_job
        .config()
        .writer_configuration(writer_configuration);
    write_job.write_qpdf(&mut pdf).unwrap();

    let written = Pdf::open(BufReader::new(File::open(&output).unwrap())).unwrap();
    assert!(
        written.is_encrypted(),
        "the copied donor must reproduce the primary's encryption on the output"
    );
}

#[test]
fn combined_inspection_completes_once_after_all_reports() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let (logger, warnings) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": input,
            "showNpages": "",
            "showXref": "",
            "showPages": ""
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    let summary = b"qpdfjob json: operation succeeded with warnings\n";
    let bytes = warnings.lock().unwrap().bytes.clone();
    assert_eq!(
        bytes
            .windows(summary.len())
            .filter(|window| *window == summary)
            .count(),
        1,
        "combined doInspection branches must share one completion summary"
    );
}

#[test]
fn no_output_write_reports_memory_usage_after_inspection() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let (logger, warnings) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": input,
            "showNpages": "",
            "reportMemoryUsage": ""
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(
        String::from_utf8_lossy(&warnings.lock().unwrap().bytes).contains("qpdf-max-memory-usage ")
    );
}

#[test]
fn get_exit_code_is_pure_before_and_after_completion() {
    let (logger, warnings) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.record_warnings();

    assert_eq!(job.get_exit_code(), JobExitCode::Warning);
    assert_eq!(job.get_exit_code(), JobExitCode::Warning);
    assert!(warnings.lock().unwrap().bytes.is_empty());

    job.complete(false).unwrap();
    let completed_bytes = warnings.lock().unwrap().bytes.clone();
    assert_eq!(job.get_exit_code(), JobExitCode::Warning);
    assert_eq!(warnings.lock().unwrap().bytes, completed_bytes);
}

#[test]
fn a_failed_reopen_clears_the_previous_encryption_status() {
    // `get_exit_code` is a public, side-effect-free query, so a reused job
    // must not answer it from the previous document's encryption bits. qpdf
    // never faces this because an open failure escapes `run()` as an
    // exception and the CLI exits from its catch without consulting
    // `getExitCode` (`qpdf/qpdf.cc:39-43`); flpdf converts that failure into
    // `JobExitCode::Error` instead, so the status has to be cleared up front.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let encrypted = root.join("encrypted/v4-aes-128-r4.pdf");

    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(
        &serde_json::json!({"inputFile": encrypted, "isEncrypted": ""}).to_string(),
    )
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert_eq!(job.get_exit_code(), JobExitCode::Success);

    let mut job = job;
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": root.join("this-file-does-not-exist.pdf"),
            "isEncrypted": "",
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Error);
    assert_eq!(
        job.get_exit_code(),
        JobExitCode::Error,
        "a failed open must not leave the previous document's encryption status behind"
    );
}

#[test]
fn encryption_status_exit_codes_match_qpdf_get_exit_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let encrypted = root.join("encrypted/v4-aes-128-r4.pdf");
    let plaintext = root.join("minimal.pdf");

    let cases = [
        (
            serde_json::json!({"inputFile": encrypted, "requiresPassword": ""}),
            JobExitCode::Success,
        ),
        (
            serde_json::json!({
                "inputFile": encrypted,
                "password": "user-v4-aes",
                "requiresPassword": ""
            }),
            JobExitCode::Warning,
        ),
        (
            serde_json::json!({"inputFile": encrypted, "isEncrypted": ""}),
            JobExitCode::Success,
        ),
        (
            serde_json::json!({"inputFile": plaintext, "isEncrypted": ""}),
            JobExitCode::Error,
        ),
    ];
    for (value, expected) in cases {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(&value.to_string())
            .unwrap();
        assert_eq!(job.run().unwrap(), expected);
        assert_eq!(job.get_exit_code(), expected);
    }
}

#[test]
fn create_qpdf_status_precedes_show_encryption_on_wrong_password() {
    let encrypted = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    for status_key in ["isEncrypted", "requiresPassword"] {
        let (logger, info_state) = logger_with_info_sink();
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        let mut configuration = serde_json::Map::new();
        configuration.insert("inputFile".to_owned(), serde_json::json!(encrypted));
        configuration.insert(
            "password".to_owned(),
            serde_json::Value::String("wrong-password".to_owned()),
        );
        configuration.insert(
            "showEncryption".to_owned(),
            serde_json::Value::String(String::new()),
        );
        configuration.insert(
            status_key.to_owned(),
            serde_json::Value::String(String::new()),
        );
        job.initialize_from_json_partial(&serde_json::Value::Object(configuration).to_string())
            .unwrap();

        assert!(job
            .create_qpdf()
            .expect("status query should handle the partial open")
            .is_none());
        assert_eq!(
            job.get_exit_code(),
            JobExitCode::Success,
            "{status_key} must win over showEncryption for a bad password"
        );
        assert!(
            info_state.lock().unwrap().bytes.is_empty(),
            "status query must not emit the show-encryption report"
        );
    }
}

#[test]
fn create_qpdf_status_handles_wrong_password_without_show_encryption() {
    let encrypted = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    for status_key in ["isEncrypted", "requiresPassword"] {
        let (logger, info_state) = logger_with_info_sink();
        let mut job = QPDFJob::new();
        job.set_logger(logger);
        let mut configuration = serde_json::Map::new();
        configuration.insert("inputFile".to_owned(), serde_json::json!(encrypted));
        configuration.insert(
            "password".to_owned(),
            serde_json::Value::String("wrong-password".to_owned()),
        );
        configuration.insert(
            status_key.to_owned(),
            serde_json::Value::String(String::new()),
        );
        job.initialize_from_json_partial(&serde_json::Value::Object(configuration).to_string())
            .unwrap();

        assert!(job
            .create_qpdf()
            .expect("status query should handle the partial open")
            .is_none());
        assert_eq!(
            job.get_exit_code(),
            JobExitCode::Success,
            "{status_key} must classify a bad password as a successful status query"
        );
        assert!(
            info_state.lock().unwrap().bytes.is_empty(),
            "status query must not emit a show-encryption report"
        );
    }
}

/// qpdf keys its opened-source cache by filename alone
/// (`page_spec_qpdfs.count(page_spec.filename) == 0`, `QPDFJob.cc:2389`),
/// reusing the same already-open QPDF for a repeated literal path rather
/// than reopening it — so a page spec's own `password` is only consulted
/// the first time a filename is seen. Encode this as an observable pass/fail:
/// the first spec references an encrypted fixture with its correct (empty)
/// password; a second spec repeats the exact same path with a wrong
/// password. If flpdf reopened the file for the second spec (the pre-fix
/// bug), the wrong password would fail the job; deduplicating by path
/// (matching qpdf) never attempts that second open, so the wrong password
/// is never consulted and the job succeeds.
#[test]
fn json_job_run_reuses_an_already_opened_page_source_for_a_repeated_filename() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("repeated-filename.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "staticId": "",
        "keepFilesOpenThreshold": "1",
        "pages": [
            {"file": &fixture, "range": "1"},
            {"file": &fixture, "password": "definitely-wrong", "range": "2"},
        ],
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    assert_eq!(
        job.run().unwrap(),
        JobExitCode::Success,
        "a repeated filename must reuse the first spec's already-open \
         source and never consult a later spec's password"
    );
    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 2);
}

/// 2-page document with an outline item pointing at page 2 (obj 4).
fn build_outline_fixture() -> Vec<u8> {
    use std::collections::BTreeMap;
    let mut objs: BTreeMap<u32, String> = BTreeMap::new();
    objs.insert(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".into(),
    );
    objs.insert(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".into());
    for n in 3..=4 {
        objs.insert(
            n,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        );
    }
    objs.insert(
        10,
        "<< /Type /Outlines /First 20 0 R /Last 20 0 R /Count 1 >>".into(),
    );
    objs.insert(
        20,
        "<< /Title (P2) /Parent 10 0 R /Dest [4 0 R /Fit] >>".into(),
    );

    let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
    let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in &objs {
        offs.insert(*n, raw.len());
        raw.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let max_num = *objs.keys().max().unwrap();
    let xref_pos = raw.len();
    raw.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
    for i in 1..=max_num {
        if let Some(&off) = offs.get(&i) {
            raw.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        } else {
            raw.extend_from_slice(b"0000000000 65535 f \n");
        }
    }
    raw.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
            max_num + 1
        )
        .as_bytes(),
    );
    raw
}

/// A single-source `--pages . 1` job (qpdf's own-file page selection) takes
/// the in-place `QPDFJob::handle_page_specs` route
/// (`PageSpecJobOutput::InPlace`). Before this test, `QPDFJob::run`'s
/// in-place branch pruned the subset without first calling
/// `remap_outline_and_dests`, leaving the outline's `/Dest` pointing at the
/// now-removed, ungutted page 2 object instead of the null-and-kept-live
/// value qpdf produces (`libqpdf/QPDF_optimization.cc`'s outline/dest sweep,
/// matched by the merged-source CLI route's own
/// `remap_outline_and_dests` call in `flpdf-cli`).
#[test]
fn json_job_run_in_place_page_subset_remaps_outline_dests() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("outline-fixture.pdf");
    std::fs::write(&input, build_outline_fixture()).unwrap();
    let output = tempdir.path().join("in-place-subset.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "pages": [{"file": ".", "range": "1"}]
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(flpdf::pages::page_refs(&mut pdf).unwrap().len(), 1);

    // Writing renumbers objects, so locate the removed page through the
    // surviving outline item's `/Dest` rather than assuming the original
    // fixture's object number 4 persists in the output.
    let root = pdf.trailer_key_handle(b"Root");
    root.try_is_scalar().unwrap();
    let outlines = root.as_dictionary().unwrap()[b"/Outlines".as_slice()].clone();
    outlines.try_is_scalar().unwrap();
    let first_item = outlines.as_dictionary().unwrap()[b"/First".as_slice()].clone();
    first_item.try_is_scalar().unwrap();
    let dest = first_item.as_dictionary().unwrap()[b"/Dest".as_slice()].clone();
    dest.try_is_scalar().unwrap();
    let target = dest.as_array().unwrap()[0].clone();
    let target_ref = target
        .object_ref()
        .expect("the outline dest target must still be an indirect reference");
    target.try_is_scalar().unwrap();

    assert!(
        target.is_null(),
        "the removed page kept alive by the outline dest must be nulled, not left as a \
         dangling /Type /Page dict"
    );
    assert!(
        pdf.get_all_objects()
            .unwrap()
            .into_iter()
            .filter_map(|handle| handle.object_ref())
            .any(|object_ref| object_ref == target_ref),
        "the nulled-but-referenced page must stay live"
    );
}

#[test]
fn json_job_run_applies_encryption_and_json_output_mode() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let pdf_output = tempdir.path().join("encrypted.pdf");
    let json_output = tempdir.path().join("encrypted.json");
    let json = serde_json::json!({
        "inputFile": fixture,
        "outputFile": pdf_output,
        "staticId": "",
        "encrypt": {
            "userPassword": "u",
            "ownerPassword": "o",
            "128bit": {"useAes": "y"}
        }
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let encrypted = Pdf::open_with_options(
        BufReader::new(File::open(pdf_output).unwrap()),
        PdfOpenOptions {
            password: b"u".to_vec(),
            ..PdfOpenOptions::default()
        },
    )
    .unwrap();
    assert!(encrypted.is_encrypted());

    let json = serde_json::json!({
        "inputFile": fixture,
        "outputFile": json_output,
        "jsonOutput": "2"
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let output = std::fs::read_to_string(json_output).unwrap();
    assert!(output.contains("\"jsonversion\": 2"));
    assert!(!output.contains("\"version\": 2"));
}

#[test]
fn json_job_partial_initialization_defers_missing_output_to_run() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input}).to_string();
    let mut job = QPDFJob::new();

    job.initialize_from_json_partial(&json).unwrap();
    let error = job.run().unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "an output file name is required; use - for standard output"
    ));
}

#[test]
fn json_job_partial_initialization_still_rejects_a_malformed_output_file() {
    // qpdf's JSONHandler dispatches `outputFile` to a string-only handler
    // (QPDFJob_json.cc:262-265) and rejects any other present type with a
    // usage error (JSONHandler.cc:186), regardless of partial-init mode: a
    // present-but-wrong-typed value is not the same as an absent key.
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input, "outputFile": 42}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json_partial(&json).unwrap_err();

    assert!(matches!(error, Error::Usage(ref message)
        if message.to_string().contains("outputFile") && message.to_string().contains("must be a string")));
}

#[test]
fn json_job_rejects_a_malformed_output_file_outside_partial_mode() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input, "outputFile": false}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json(&json).unwrap_err();

    assert!(matches!(error, Error::Usage(ref message)
        if message.to_string().contains("outputFile") && message.to_string().contains("must be a string")));
}

#[test]
fn json_job_progress_uses_the_qpdf_default_info_reporter() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("progress-output.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "progress": "",
    })
    .to_string();
    let (logger, state) = logger_with_info_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let info = state.lock().unwrap().bytes.clone();
    assert!(info
        .windows(b"write progress: 0%\n".len())
        .any(|window| { window == b"write progress: 0%\n" }));
    assert!(info
        .windows(b"write progress: 100%\n".len())
        .any(|window| { window == b"write progress: 100%\n" }));
}

#[test]
fn json_job_progress_logger_failures_abort_and_propagate_from_write() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("progress-output.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "progress": "",
    })
    .to_string();
    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn json_job_progress_labels_dash_output_as_standard_output() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "-",
        "progress": "",
    })
    .to_string();
    let (logger, info_state) = logger_with_info_sink();
    let save_state = Arc::new(Mutex::new(SinkState::default()));
    logger
        .set_save(
            Some(PipelineHandle::new(RecordingSink {
                state: Arc::clone(&save_state),
            })),
            false,
        )
        .unwrap();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let info = info_state.lock().unwrap().bytes.clone();
    let output = save_state.lock().unwrap().bytes.clone();
    assert!(info
        .windows(b"standard output: write progress: 0%\n".len())
        .any(|window| window == b"standard output: write progress: 0%\n"));
    assert!(output.starts_with(b"%PDF-"));
}

#[test]
fn json_job_rejects_same_input_output_before_truncating_a_hard_link() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("hard-link.pdf");
    std::fs::copy(fixture, &input).unwrap();
    std::fs::hard_link(&input, &output).unwrap();
    let before = std::fs::read(&input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
    })
    .to_string();

    let mut full_job = QPDFJob::new();
    let error = full_job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string()
                == "input file and output file are the same; use --replace-input to intentionally overwrite the input file"
    ));

    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(&json).unwrap();

    let error = match job.create_qpdf() {
        Err(error) => error,
        Ok(_) => panic!("same-file configuration must fail before opening the input"),
    };
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string()
                == "input file and output file are the same; use --replace-input to intentionally overwrite the input file"
    ));
    assert_eq!(std::fs::read(&input).unwrap(), before);
}

#[test]
fn create_qpdf_reports_an_ordinary_output_configuration_failure() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let (logger, error_state) = logger_with_error_sink();
    logger.info(Vec::<u8>::new()).unwrap();

    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "-",
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(&json).unwrap();

    assert!(job.create_qpdf().unwrap().is_none());
    assert!(error_state
        .lock()
        .unwrap()
        .bytes
        .windows(b"QPDFLogger: called setSave on standard output after standard output has already been used".len())
        .any(|window| {
            window
                == b"QPDFLogger: called setSave on standard output after standard output has already been used"
        }));
}

#[test]
fn json_job_dash_output_uses_the_job_save_pipeline() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    std::fs::copy(fixture, &input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "-",
    })
    .to_string();
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    logger
        .set_save(
            Some(PipelineHandle::new(RecordingSink {
                state: Arc::clone(&state),
            })),
            false,
        )
        .unwrap();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();
    let dash = Path::new("-");
    let _ = std::fs::remove_file(dash);

    let status = job.run().unwrap();
    let bytes = state.lock().unwrap().bytes.clone();
    let literal_dash_exists = dash.exists();
    let _ = std::fs::remove_file(dash);

    assert_eq!(status, JobExitCode::Success);
    assert!(
        bytes.starts_with(b"%PDF-"),
        "stdout sink must receive PDF bytes"
    );
    assert!(
        !literal_dash_exists,
        "dash must not be opened as a literal output path"
    );
}

#[test]
fn json_job_without_output_is_a_usage_error() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let json = serde_json::json!({"inputFile": input}).to_string();
    let mut job = QPDFJob::new();

    let error = job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "an output file name is required; use - for standard output"
    ));

    let error = job.run().unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage)
            if usage.to_string() == "an output file name is required; use - for standard output"
    ));
}

#[test]
fn json_job_accepts_linearize_configuration() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": tempdir.path().join("out.pdf"),
        "linearize": ""
    })
    .to_string();
    let mut job = QPDFJob::new();

    job.initialize_from_json(&json)
        .expect("linearize is part of the full job JSON schema");
}

#[test]
fn json_job_deterministic_id_repeats_identical_output() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let first = tempdir.path().join("first.pdf");
    let second = tempdir.path().join("second.pdf");

    for output in [&first, &second] {
        let json = serde_json::json!({
            "inputFile": input,
            "outputFile": output,
            "deterministicId": ""
        })
        .to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json(&json).unwrap();
        assert_eq!(job.run().unwrap(), JobExitCode::Success);
    }

    assert_eq!(
        std::fs::read(first).unwrap(),
        std::fs::read(second).unwrap()
    );
}

#[test]
fn create_qpdf_and_write_qpdf_are_separate_job_boundaries() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("separate-output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--static-id".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    job.write_qpdf(&mut pdf).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);
    assert!(output.exists());
}

#[test]
fn argv_replace_input_uses_the_canonical_write_boundary() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("入力.pdf");
    std::fs::copy(&fixture, &input).unwrap();
    let original = std::fs::read(&input).unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        "--deterministic-id".to_owned(),
        "--object-streams=generate".to_owned(),
        "--replace-input".to_owned(),
    ];

    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    job.write_qpdf(&mut pdf).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);
    assert_ne!(std::fs::read(&input).unwrap(), original);
    assert!(input.is_file());
    assert!(!input.with_extension("pdf.~qpdf-orig").exists());
    assert!(!input.with_file_name("入力.pdf.~qpdf-orig#").exists());
    assert!(!input.with_file_name("入力.pdf.~qpdf-temp#").exists());
}

#[test]
fn argv_replace_input_rejects_an_output_file() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
        "--replace-input".to_owned(),
    ];

    let mut job = QPDFJob::new();
    let error = job.initialize_from_argv(&args).unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "replace-input can't be used since output file has already been given"
    ));
}

/// `ArgParser::argReplaceInput` calls `Config::replaceInput` for every
/// occurrence of the flag (`libqpdf/QPDFJob_argv.cc:91-96`), and that setter
/// treats an already-set `replace_input` as an output that has been given
/// (`libqpdf/QPDFJob_config.cc:53-61`). Probed with qpdf 11.9.0:
/// `qpdf --replace-input --replace-input in.pdf` exits 2 with
/// `replace-input can't be used since output file has already been given`.
#[test]
fn argv_replace_input_rejects_a_duplicate_flag() {
    let args = vec![
        "qpdfjob".to_owned(),
        "input.pdf".to_owned(),
        "--replace-input".to_owned(),
        "--replace-input".to_owned(),
    ];
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&args)
        .expect_err("a repeated --replace-input is a usage error");
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "replace-input can't be used since output file has already been given"
    ));
}

#[test]
fn config_replace_input_rejects_existing_output_and_duplicate_configuration() {
    let output = Path::new("output.pdf");
    let mut with_output = QPDFJob::new();
    with_output.set_output_file(output).unwrap();
    let output_error = match with_output.config().replace_input() {
        Ok(_) => panic!("replace-input must reject an existing output file"),
        Err(error) => error,
    };
    assert!(matches!(
        output_error,
        Error::Usage(usage)
            if usage.to_string() == "replace-input can't be used since output file has already been given"
    ));

    let mut duplicate = QPDFJob::new();
    duplicate.config().replace_input().unwrap();
    let duplicate_error = match duplicate.config().replace_input() {
        Ok(_) => panic!("replace-input must reject duplicate configuration"),
        Err(error) => error,
    };
    assert!(matches!(
        duplicate_error,
        Error::Usage(usage)
            if usage.to_string() == "replace-input can't be used since output file has already been given"
    ));
}

#[test]
fn write_qpdf_failure_returns_an_error() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        tempdir.path().to_string_lossy().into_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    assert!(job.write_qpdf(&mut pdf).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn write_qpdf_output_sink_error_does_not_prefix_the_input_name() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/objstm-lin-outlines-80-200.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        "/dev/full".to_owned(),
    ];
    let (logger, state) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    let error = job
        .write_qpdf(&mut pdf)
        .expect_err("/dev/full must reject the sufficiently large output");
    // qpdf 11.9.0 prints exactly this for the same input and output:
    // `qpdf: qpdf output: Pl_StdioFile::write: No space left on device`.
    assert!(
        matches!(
            &error,
            Error::SystemBytes(message)
                if message == b"qpdf output: Pl_StdioFile::write: No space left on device"
        ),
        "{error:?}"
    );
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdfjob: qpdf output: Pl_StdioFile::write: No space left on device\n"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linearized_output_sink_error_reports_the_qpdf_output_pipeline() {
    // The linearized route hands the finished document to the sink in one
    // piece, so it carries the failure through a different call than the
    // streaming route above. qpdf 11.9.0 prints the same line for both:
    // `qpdf --linearize <input> /dev/full`.
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/objstm-lin-outlines-80-200.pdf");
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": "/dev/full",
        "linearize": ""
    })
    .to_string();
    let (logger, state) = logger_with_error_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    let error = job
        .write_qpdf(&mut pdf)
        .expect_err("/dev/full must reject the linearized output");
    assert!(
        matches!(
            &error,
            Error::SystemBytes(message)
                if message == b"qpdf output: Pl_StdioFile::write: No space left on device"
        ),
        "{error:?}"
    );
    // The JSON entry point carries qpdf's own `qpdfjob json` message prefix
    // (`qpdfjob-c.cc:82`); only the prefix differs from the argv route.
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdfjob json: qpdf output: Pl_StdioFile::write: No space left on device\n"
    );
}

#[test]
fn write_qpdf_without_an_output_runs_inspection() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        "--check".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");

    job.write_qpdf(&mut pdf).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);
}

#[test]
fn missing_input_returns_qpdf_error_status_without_panicking() {
    let tempdir = tempfile::tempdir().unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        tempdir
            .path()
            .join("missing.pdf")
            .to_string_lossy()
            .into_owned(),
        tempdir
            .path()
            .join("output.pdf")
            .to_string_lossy()
            .into_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn unknown_qpdf_job_argv_is_a_usage_error() {
    let args = vec!["qpdfjob".to_owned(), "--not-a-qpdf-option".to_owned()];
    let mut job = QPDFJob::new();

    let error = job.initialize_from_argv(&args).unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "unrecognized argument --not-a-qpdf-option"
    ));
}

#[test]
fn argv_usage_rejects_short_options_too_many_positionals_and_missing_input() {
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&["qpdfjob".to_owned(), "-x".to_owned()])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "unrecognized argument -x"
    ));

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&[
            "qpdfjob".to_owned(),
            "a.pdf".to_owned(),
            "b.pdf".to_owned(),
            "c.pdf".to_owned(),
        ])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "unknown argument c.pdf"
    ));

    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_argv(&["qpdfjob".to_owned()])
        .unwrap_err();
    assert!(matches!(
        &error,
        Error::Usage(usage) if usage.to_string() == "an input file name is required"
    ));
}

#[test]
fn create_qpdf_reports_unconfigured_and_malformed_inputs() {
    let mut job = QPDFJob::new();
    assert!(job.create_qpdf().unwrap().is_none());

    let tempdir = tempfile::tempdir().unwrap();
    let malformed = tempdir.path().join("malformed.pdf");
    std::fs::write(&malformed, b"not a PDF").unwrap();
    let args = vec![
        "qpdfjob".to_owned(),
        malformed.to_string_lossy().into_owned(),
        tempdir
            .path()
            .join("output.pdf")
            .to_string_lossy()
            .into_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    assert!(job.create_qpdf().unwrap().is_none());
}

#[test]
fn run_check_and_check_operation_failure_map_to_error_status() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let args = vec![
        "qpdfjob".to_owned(),
        input.to_string_lossy().into_owned(),
        "--check".to_owned(),
    ];
    let mut job = QPDFJob::new();
    job.initialize_from_argv(&args).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_argv(&args).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn invalid_qpdf_job_json_is_rejected_at_initialization() {
    let mut job = QPDFJob::new();
    assert!(job.initialize_from_json("[]").is_err());
    assert!(job
        .initialize_from_json("{\"outputFile\":\"out.pdf\"}")
        .is_err());
    assert!(job
        .initialize_from_json("{\"inputFile\":\"input.pdf\",\"objectStreams\":\"unknown\"}")
        .is_err());
}

#[test]
fn job_json_rejects_v1_only_keys_under_json_version_2() {
    // qpdf validates jsonKey/version compatibility unconditionally in
    // checkConfiguration (`QPDFJob.cc:630-637`), confirmed against live
    // qpdf 11.9.0: `--json-key=objects` errors with this exact message even
    // without `--json` at all, since `m->json_version` defaults to 0, which
    // falls into the "not version 1" branch.
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.json");
    let json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": output,
        "json": "2",
        "jsonKey": ["objects"]
    })
    .to_string();
    let mut job = QPDFJob::new();
    let error = job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "json keys \"objects\" and \"objectinfo\" are only valid for json version 1"
    ));
}

#[test]
fn job_json_rejects_qpdf_key_under_json_version_1() {
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.json");
    let json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": output,
        "json": "1",
        "jsonKey": ["qpdf"]
    })
    .to_string();
    let mut job = QPDFJob::new();
    let error = job.initialize_from_json(&json).unwrap_err();
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "json key \"qpdf\" is only valid for json version > 1"
    ));
}

#[test]
fn job_json_password_file_reads_raw_bytes_and_keeps_only_the_first_line() {
    // The discarded second line contains a raw non-UTF-8 byte (0xFF is never
    // a valid UTF-8 lead byte on its own): `read_to_string` rejects the
    // whole file before authentication even gets a chance to run, while the
    // byte-preserving first-line split (`QUtil::read_lines_from_file` +
    // `lines.front()`, `QUtil.cc:1231-1286`) only ever looks at the first
    // line's bytes, so the password authenticates successfully. The first
    // line itself ends in `\r\n` to also exercise the trailing-`\r` strip.
    let encrypted_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let password_file = tempdir.path().join("password.txt");
    std::fs::write(
        &password_file,
        b"user-v4-aes\r\nignored line with \xffinvalid utf-8\n",
    )
    .unwrap();
    let output = tempdir.path().join("out.pdf");
    let json = serde_json::json!({
        "inputFile": encrypted_pdf,
        "outputFile": output,
        "passwordFile": password_file,
        "decrypt": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let written = std::fs::read(&output).unwrap();
    let mut opened = Pdf::open(Cursor::new(written))
        .expect("output must be decrypted and openable without a password");
    let _ = opened.root_handle().unwrap();
}

#[test]
fn job_json_no_warn_suppresses_document_open_warnings_not_just_the_summary() {
    // qpdf's noWarn (`Config::noWarn`, `QPDFJob_config.cc:407-410`) applies
    // `pdf.setSuppressWarnings(true)` to every QPDF the job opens
    // (`QPDFJob.cc:663-665`), not just the final completion summary.
    let (logger, state) = logger_with_warning_sink();
    let damaged = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("out.pdf");
    let json = serde_json::json!({
        "inputFile": damaged,
        "outputFile": output,
        "noWarn": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json(&json).unwrap();
    // The repaired document still recorded a warning internally (qpdf's
    // `QPDF::warn` always pushes to `m->warnings` regardless of
    // `suppress_warnings`, which only gates whether it is printed), so the
    // job's own exit code is still `Warning` even though nothing is printed.
    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(
        state.lock().unwrap().bytes.is_empty(),
        "noWarn must suppress recovery warnings emitted while opening the document, not just \
         the final completion summary"
    );
}

#[test]
fn completion_emits_one_qpdf_warning_summary_and_warning_status() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_message_prefix("flpdf");
    job.record_warnings();

    job.complete(true).unwrap();
    assert_eq!(
        job.get_exit_code(),
        JobExitCode::Warning,
        "qpdf uses status 3 for recoverable warnings"
    );
    assert_eq!(
        state.lock().unwrap().bytes,
        b"flpdf: operation succeeded with warnings; resulting file may have some problems\n"
    );
}

#[test]
fn warning_suppression_keeps_warning_status_but_suppresses_summary() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_suppress_warnings(true);
    job.record_warnings();

    job.complete(false).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Warning);
    assert!(state.lock().unwrap().bytes.is_empty());
}

#[test]
fn warnings_exit_zero_changes_only_the_exit_status() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_warnings_exit_zero(true);
    job.record_warnings();

    job.complete(false).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: operation succeeded with warnings\n"
    );
}

#[test]
fn warning_sink_errors_are_returned_to_the_caller() {
    let logger = QPDFLogger::create();
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.record_warnings();

    assert!(matches!(
        job.complete(false),
        Err(Error::System(message)) if message == "warning sink failed"
    ));
}

#[test]
fn a_check_failure_is_not_reported_twice_at_the_write_boundary() {
    // qpdf's `doCheck` throws `std::runtime_error("errors detected")`
    // (`libqpdf/QPDFJob.cc:793`) and its CLI catch prints
    // `qpdf: errors detected` exactly once (`qpdf/qpdf.cc:39-41`). flpdf's
    // check consumer writes that line itself, so the write boundary must not
    // repeat it.
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("bad-stream.pdf");
    // A page whose content stream declares /FlateDecode but holds raw bytes:
    // qpdf reports `ERROR: page 1: content stream ...` and then the single
    // `errors detected` line.
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for object in [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n"
            .to_vec(),
        b"4 0 obj\n<< /Length 16 /Filter /FlateDecode >>\nstream\nNOT-DEFLATE-DATA\nendstream\nendobj\n"
            .to_vec(),
    ] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(&object);
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len() + 1
        )
        .as_bytes(),
    );
    std::fs::write(&path, &bytes).unwrap();

    let (logger, errors) = logger_with_error_sink();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(logger.discard()));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(
        &serde_json::json!({"inputFile": path, "check": ""}).to_string(),
    )
    .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
    let reported = String::from_utf8_lossy(&errors.lock().unwrap().bytes).to_string();
    assert_eq!(
        reported.matches("errors detected").count(),
        1,
        "the check consumer already wrote qpdf's single line: {reported}"
    );
    assert!(
        !reported.contains("unsupported PDF feature"),
        "qpdf never prefixes this diagnostic: {reported}"
    );
}

#[test]
fn an_unreported_inspection_failure_is_reported_once_at_the_write_boundary() {
    // The counterpart of the check case: a failure that no inspection step has
    // already announced must still get qpdf's single `qpdf: <what()>` line,
    // which qpdf's CLI prints from its catch (`qpdf/qpdf.cc:39-41`). Here the
    // info sink fails while `--show-npages` writes, so `write_qpdf` owes the
    // diagnostic.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    let (logger, errors) = logger_with_error_sink();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    logger.set_warn(Some(logger.discard()));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(
        &serde_json::json!({"inputFile": path, "showNpages": ""}).to_string(),
    )
    .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Error);
    let reported = String::from_utf8_lossy(&errors.lock().unwrap().bytes).to_string();
    assert!(
        reported.contains("warning sink failed"),
        "the boundary must report a failure no inspection step announced: {reported}"
    );
    assert_eq!(
        reported.matches("warning sink failed").count(),
        1,
        "qpdf prints exactly one line for it: {reported}"
    );
}

#[test]
fn show_linearization_propagates_custom_info_sink_failure() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    let logger = QPDFLogger::create();
    logger.set_info(Some(PipelineHandle::new(FailingSink)));
    logger.set_warn(Some(logger.discard()));
    logger.set_error(Some(logger.discard()));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let mut pdf = job
        .open(
            BufReader::new(File::open(&path).unwrap()),
            path.display().to_string(),
            PdfOpenOptions::default(),
        )
        .unwrap();

    let error = job
        .show_linearization(&mut pdf)
        .expect_err("custom info sink failure must propagate");
    assert!(matches!(
        error,
        Error::System(message) if message == "warning sink failed"
    ));
}

#[test]
fn show_linearization_propagates_custom_warning_sink_failure() {
    let mut bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/linearized-one-page.pdf"),
    )
    .unwrap();
    let marker = b"/O 6 /E";
    let replacement = b"/O 7 /E";
    let offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("linearized fixture should contain /O");
    bytes[offset..offset + marker.len()].copy_from_slice(replacement);

    let logger = QPDFLogger::create();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
    logger.set_error(Some(logger.discard()));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let mut pdf = job
        .open(
            Cursor::new(bytes),
            "linearized-mismatch.pdf",
            PdfOpenOptions::default(),
        )
        .unwrap();

    let error = job
        .show_linearization(&mut pdf)
        .expect_err("custom warning sink failure must propagate");
    assert!(matches!(
        error,
        Error::System(message) if message == "warning sink failed"
    ));
}

/// `write_qpdf` emits the "supplied password looks like a Unicode password"
/// warning through the custom *error* sink (qpdf's `--password-mode=auto`
/// warning, `QPDF_encryption.cc`), not the warning sink used by the tests
/// above. This pins that its `?` propagates a custom error-sink failure the
/// same way.
#[test]
fn write_qpdf_propagates_custom_error_sink_failure_for_auto_password_warning() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();
    let output = directory.path().join("output.pdf");

    let logger = QPDFLogger::create();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(logger.discard()));
    logger.set_error(Some(PipelineHandle::new(FailingSink)));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": output,
        "passwordMode": "auto",
        "encrypt": {
            "userPassword": "😀",
            "ownerPassword": "owner",
            "128bit": {"useAes": "y"}
        }
    })
    .to_string();
    job.initialize_from_json_partial(&json).unwrap();

    let mut pdf = job
        .open(
            BufReader::new(File::open(&input).unwrap()),
            input.display().to_string(),
            PdfOpenOptions::default(),
        )
        .unwrap();

    let error = job
        .write_qpdf(&mut pdf)
        .expect_err("custom error sink failure must propagate");
    assert!(matches!(
        error,
        Error::System(message) if message == "warning sink failed"
    ));
}

#[test]
fn document_repair_warnings_feed_the_shared_completion_state() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let pdf = Pdf::open_with_repair(BufReader::new(File::open(path).unwrap())).unwrap();
    assert!(!pdf.repair_diagnostics().entries().is_empty());

    let mut job = QPDFJob::new();
    job.record_document_warnings(&pdf);

    assert!(job.has_warnings());
}

#[test]
fn job_open_installs_logger_before_open_warnings() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let (job_logger, job_state) = logger_with_warning_sink();
    let (option_logger, option_state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(job_logger);

    let pdf = job
        .open(
            BufReader::new(File::open(path).unwrap()),
            "repairable.pdf",
            PdfOpenOptions {
                repair: true,
                logger: Some(option_logger),
                ..PdfOpenOptions::default()
            },
        )
        .expect("repairable input should open");

    assert!(!pdf.repair_diagnostics().entries().is_empty());
    assert!(job.has_warnings());
    assert!(!job_state.lock().unwrap().bytes.is_empty());
    assert!(
        option_state.lock().unwrap().bytes.is_empty(),
        "open diagnostics must use the job logger rather than caller options"
    );
}

#[test]
fn document_warning_api_drains_without_logger_redelivery() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let (logger, state) = logger_with_warning_sink();
    let pdf = Pdf::open_with_options(
        BufReader::new(File::open(path).unwrap()),
        PdfOpenOptions {
            repair: true,
            logger: Some(logger),
            ..PdfOpenOptions::default()
        },
    )
    .expect("repairable input should open");
    let logged_before_drain = state.lock().unwrap().bytes.clone();

    assert!(pdf.any_warnings());
    assert!(pdf.num_warnings() > 0);
    let drained = pdf.get_warnings();
    assert!(!drained.is_empty());
    assert_eq!(
        state.lock().unwrap().bytes,
        logged_before_drain,
        "get_warnings must drain collection without replaying logger output"
    );
    assert!(!pdf.any_warnings());
    assert_eq!(pdf.num_warnings(), 0);
    assert!(pdf.get_warnings().is_empty());
}

#[test]
fn inspection_completion_drains_document_warnings_into_job_state() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let mut pdf = job
        .open(
            BufReader::new(File::open(path).unwrap()),
            "repairable.pdf",
            PdfOpenOptions {
                repair: true,
                ..PdfOpenOptions::default()
            },
        )
        .expect("repairable input should open");
    assert!(pdf.any_warnings());

    let status = job
        .inspect(&mut pdf, |_| -> flpdf::Result<()> { Ok(()) })
        .expect("inspection completion");

    assert_eq!(status, JobExitCode::Warning);
    assert!(!pdf.any_warnings());
    assert_eq!(pdf.num_warnings(), 0);
    let summary = b"qpdf: operation succeeded with warnings\n";
    let bytes = state.lock().unwrap().bytes.clone();
    assert_eq!(
        bytes
            .windows(summary.len())
            .filter(|window| *window == summary)
            .count(),
        1,
        "completion must emit one summary after draining document warnings"
    );
}

#[test]
fn inspection_completes_through_the_shared_warning_boundary() {
    let mut job = QPDFJob::new();
    let (logger, state) = logger_with_warning_sink();
    job.set_logger(logger);
    let mut pdf = job
        .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
        .expect("complete JSON input");
    job.record_warnings();

    let status = job
        .inspect(&mut pdf, |document| -> flpdf::Result<()> {
            assert_eq!(document.root_ref(), Some(flpdf::ObjectRef::new(1, 0)));
            Ok(())
        })
        .expect("inspection completion");

    assert_eq!(status, JobExitCode::Warning);
    assert_eq!(
        state.lock().unwrap().bytes,
        b"qpdf: operation succeeded with warnings\n"
    );
}

#[test]
fn registered_progress_reporter_is_attached_to_each_writer() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let progress = Arc::new(Mutex::new(Vec::new()));
    let mut job = QPDFJob::new();
    let progress_for_callback = Arc::clone(&progress);
    job.register_progress_reporter(move |percent| {
        progress_for_callback.lock().unwrap().push(percent);
        Ok(())
    });

    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().unwrap();
    job.configure_writer_progress(&mut writer);
    writer.write().unwrap();
    assert!(!writer.get_buffer().unwrap().is_empty());

    let progress = progress.lock().unwrap();
    assert_eq!(progress.first(), Some(&0));
    assert_eq!(progress.last(), Some(&100));
}

#[test]
fn missing_progress_reporter_is_a_noop() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_memory().unwrap();
    QPDFJob::new().configure_writer_progress(&mut writer);
    writer.write().unwrap();
    assert!(!writer.get_buffer().unwrap().is_empty());
}

#[test]
fn json_create_update_and_write_share_one_job_lifecycle() {
    let mut job = QPDFJob::new();
    let mut pdf = job
        .create_from_json(Cursor::new(COMPLETE_JSON), "input.json")
        .expect("complete JSON input");
    assert_eq!(pdf.root_ref(), Some(flpdf::ObjectRef::new(1, 0)));

    job.update_from_json(&mut pdf, Cursor::new(UPDATE_JSON), "update.json")
        .expect("partial JSON update");
    assert_eq!(
        pdf.get_object_handle(flpdf::ObjectRef::new(1, 0))
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
                decode_level: DecodeLevel::None,
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
    let (logger, state) = logger_with_info_sink();
    let directory = tempfile::tempdir().expect("tempdir");
    let output_path = directory.path().join("out.json");
    let mut file = std::fs::File::create(&output_path).expect("create output");
    let mut pdf = Pdf::open(BufReader::new(
        File::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.pdf"),
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
                decode_level: DecodeLevel::None,
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
    let (logger, state) = logger_with_warning_sink();
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
                decode_level: DecodeLevel::None,
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
    logger.set_warn(Some(PipelineHandle::new(FailingSink)));
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
                decode_level: DecodeLevel::None,
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
        flpdf::job::JsonJobError::Completion(Error::System(message))
            if message == "warning sink failed"
    ));
}

#[test]
fn json_create_installs_job_logger_before_import_warnings() {
    let (logger, state) = logger_with_warning_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);

    let error = match job.create_from_json(Cursor::new(b"{}"), "input.json") {
        Ok(_) => panic!("invalid JSON must fail"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("errors found in JSON"));
    assert!(
        !state.lock().unwrap().bytes.is_empty(),
        "import-time diagnostic must use the job logger"
    );
}

#[test]
fn json_write_failure_does_not_emit_completion_summary() {
    let (logger, state) = logger_with_warning_sink();
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
                decode_level: DecodeLevel::None,
                stream_data: JsonStreamData::None,
                stream_prefix: None,
                keys: &[],
                objects: &[],
            },
            JsonJobOutput::Stdout(&mut output),
        )
        .expect_err("serializer failure must abort before completion");

    assert!(matches!(error, flpdf::job::JsonJobError::Output(_)));
    assert!(state.lock().unwrap().bytes.is_empty());
}

#[test]
fn json_job_output_matches_qpdf_11_9_json_input_route() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/complete.json");
    let expected = match Command::new("qpdf")
        .args(["--json-input", "--json=2"])
        .arg(&path)
        .arg("-")
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => panic!(
            "qpdf JSON route failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => {
            eprintln!("skipping qpdf differential: {error}");
            return;
        }
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
            decode_level: DecodeLevel::Generalized,
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

#[test]
fn json_job_parser_accepts_all_covered_qpdf_handler_shapes() {
    let valid_documents = [
        r#"{"inputFile":""}"#,
        r#"{"streamData":"compress"}"#,
        r#"{"streamData":"preserve"}"#,
        r#"{"streamData":"uncompress"}"#,
        r#"{"jsonStreamData":"none"}"#,
        r#"{"jsonStreamData":"inline"}"#,
        r#"{"jsonStreamData":"file","jsonStreamPrefix":"streams"}"#,
        r#"{"removeUnreferencedResources":"auto"}"#,
        r#"{"removeUnreferencedResources":"yes"}"#,
        r#"{"removeUnreferencedResources":"no"}"#,
        r#"{"allowWeakCrypto":"","encrypt":{"userPassword":"u","ownerPassword":"o","128bit":{"modify":"all"}}}"#,
        r#"{"pages":{"file":"page.pdf","password":"p","range":"1-2"}}"#,
        r#"{"overlay":{"file":"overlay.pdf","from":"1","to":"1","repeat":"1"},"underlay":{"file":"underlay.pdf"}}"#,
        r#"{"addAttachment":{"file":"attachment.bin","filename":"shown.bin","key":"shown-key","replace":""}}"#,
        r#"{"copyAttachmentsFrom":{"file":"copy.pdf","password":"p","prefix":"copy-"}}"#,
        r#"{"removeAttachment":["old-key"],"setPageLabels":["1:D"]}"#,
        r#"{"jsonKey":["pages","qpdf"],"jsonObject":["trailer","1 0 R"]}"#,
    ];

    for json in valid_documents {
        let mut job = QPDFJob::new();
        job.initialize_from_json_partial(json)
            .unwrap_or_else(|error| panic!("valid job JSON rejected: {json}: {error}"));
    }

    let invalid_documents = [
        r#"{"addAttachment":[{"file":"/"}]}"#,
        r#"{"overlay":[{}]}"#,
        r#"{"pages":[{}]}"#,
        r#"{"jsonKey":[1]}"#,
        r#"{"jsonKey":["unknown"]}"#,
        r#"{"jsonObject":[1]}"#,
        r#"{"removeAttachment":[1]}"#,
        r#"{"setPageLabels":[1]}"#,
        r#"{"inputFile":"input.pdf","empty":""}"#,
    ];
    for json in invalid_documents {
        let mut job = QPDFJob::new();
        assert!(
            job.initialize_from_json_partial(json).is_err(),
            "invalid job JSON unexpectedly accepted: {json}"
        );
    }
}

#[test]
fn json_job_copies_attachments_from_every_donor_before_reporting_conflicts() {
    // qpdf's copyAttachments visits every configured donor and reports the
    // conflicting keys once after the last one (QPDFJob.cc:2089-2135), so a
    // conflict in the first donor must not stop the second from being
    // processed and listed.
    let tempdir = tempfile::tempdir().unwrap();
    let minimal = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
    )
    .unwrap();
    let write_with_attachment = |name: &str| -> std::path::PathBuf {
        let payload = tempdir.path().join(format!("{name}.bin"));
        std::fs::write(&payload, name.as_bytes()).unwrap();
        let mut job = QPDFJob::new();
        let mut pdf = job
            .open(
                Cursor::new(minimal.clone()),
                "fixture.pdf",
                PdfOpenOptions::default(),
            )
            .unwrap();
        job.add_attachments(
            &mut pdf,
            &[flpdf::job::AttachmentAddOptions {
                path: payload,
                key: b"shared".to_vec(),
                filename: b"shared".to_vec(),
                mimetype: None,
                description: None,
                creation_date: None,
                modification_date: None,
                replace: false,
                verbose: false,
            }],
        )
        .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        let path = tempdir.path().join(format!("{name}.pdf"));
        std::fs::write(&path, writer.get_buffer().unwrap()).unwrap();
        path
    };
    let target = write_with_attachment("target");
    let donor_a = write_with_attachment("donor-a");
    let donor_b = write_with_attachment("donor-b");
    let output = tempdir.path().join("output.pdf");

    let (logger, errors) = logger_with_error_sink();
    logger.set_info(Some(logger.discard()));
    logger.set_warn(Some(logger.discard()));
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": target.display().to_string(),
            "outputFile": output.display().to_string(),
            "copyAttachmentsFrom": [
                {"file": donor_a.display().to_string()},
                {"file": donor_b.display().to_string()},
            ],
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(job.run().unwrap(), flpdf::job::JobExitCode::Error);
    let message = String::from_utf8_lossy(&errors.lock().unwrap().bytes).into_owned();
    assert!(
        message.contains("donor-a.pdf, key: shared")
            && message.contains("donor-b.pdf, key: shared"),
        "both donors must be processed before the aggregate error: {message}"
    );
    assert!(
        !output.exists(),
        "a conflicting copy must not write the output"
    );
}

#[test]
fn json_job_opens_each_attachment_donor_at_its_verbose_boundary() {
    let tempdir = tempfile::tempdir().unwrap();
    let minimal = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
    )
    .unwrap();
    let write_with_attachment = |name: &str, key: &[u8]| -> std::path::PathBuf {
        let payload = tempdir.path().join(format!("{name}.bin"));
        std::fs::write(&payload, name.as_bytes()).unwrap();
        let mut job = QPDFJob::new();
        let mut pdf = job
            .open(
                Cursor::new(minimal.clone()),
                "fixture.pdf",
                PdfOpenOptions::default(),
            )
            .unwrap();
        job.add_attachments(
            &mut pdf,
            &[flpdf::job::AttachmentAddOptions {
                path: payload,
                key: key.to_vec(),
                filename: key.to_vec(),
                mimetype: None,
                description: None,
                creation_date: None,
                modification_date: None,
                replace: false,
                verbose: false,
            }],
        )
        .unwrap();
        let mut writer = PdfWriter::new(&mut pdf);
        writer.set_output_memory().unwrap();
        writer.write().unwrap();
        let path = tempdir.path().join(format!("{name}.pdf"));
        std::fs::write(&path, writer.get_buffer().unwrap()).unwrap();
        path
    };
    let first = write_with_attachment("json-first", b"first");
    let second = write_with_attachment("json-second", b"second");
    let damaged = tempdir.path().join("json-damaged-second.pdf");
    let mut damaged_bytes = std::fs::read(&second).unwrap();
    let start = damaged_bytes
        .windows(b"startxref\n".len())
        .rposition(|window| window == b"startxref\n")
        .unwrap();
    let digits_start = start + b"startxref\n".len();
    let digits_end = digits_start
        + damaged_bytes[digits_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap();
    damaged_bytes[digits_start..digits_end].fill(b'0');
    std::fs::write(&damaged, damaged_bytes).unwrap();

    let target = tempdir.path().join("json-target.pdf");
    std::fs::write(&target, &minimal).unwrap();
    let output = tempdir.path().join("json-output.pdf");
    let logger = QPDFLogger::create();
    let state = Arc::new(Mutex::new(SinkState::default()));
    let sink = PipelineHandle::new(RecordingSink {
        state: Arc::clone(&state),
    });
    logger.set_info(Some(sink.clone()));
    logger.set_error(Some(sink));

    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.initialize_from_json_partial(
        &serde_json::json!({
            "inputFile": target,
            "outputFile": output,
            "verbose": "",
            "copyAttachmentsFrom": [
                {"file": first},
                {"file": damaged},
            ],
        })
        .to_string(),
    )
    .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    let diagnostics = state.lock().unwrap().bytes.clone();
    let copy_first = format!(
        "qpdfjob json: copying attachments from {}\n",
        first.display()
    );
    let copy_second = format!(
        "qpdfjob json: copying attachments from {}\n",
        damaged.display()
    );
    let warning = format!("WARNING: {}: file is damaged", damaged.display());
    let key_first = b"  first -> first\n";
    let key_second = b"  second -> second\n";
    let positions = [
        copy_first.as_bytes(),
        key_first,
        copy_second.as_bytes(),
        warning.as_bytes(),
        key_second,
    ]
    .map(|needle| {
        diagnostics
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| {
                panic!(
                    "diagnostic {:?} missing:\n{}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&diagnostics)
                )
            })
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "Job JSON donor diagnostics must be verbose, open warning, then copy: {}",
        String::from_utf8_lossy(&diagnostics)
    );
}

#[test]
fn json_job_run_covers_update_page_labels_and_linearized_writer_stages() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/complete.json");
    let update = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/json-input/update.json");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("input.pdf");
    let output = tempdir.path().join("output.pdf");
    let pass1 = tempdir.path().join("pass1.pdf");
    std::fs::copy(fixture, &input).unwrap();

    let json = serde_json::json!({
        "inputFile": input,
        "jsonInput": "",
        "outputFile": output,
        "updateFromJson": update,
        "pages": [{"file": ".", "range": "1"}],
        "removePageLabels": "",
        "setPageLabels": ["1:D"],
        "linearize": "",
        "linearizePass1": pass1,
        "verbose": "",
        "staticId": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists());
}

#[test]
fn json_job_run_covers_overlay_attachment_and_copy_stages() {
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let attachment_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let overlay_output = tempdir.path().join("overlay.pdf");
    let overlay_json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": overlay_output,
        "overlay": {"file": one_page, "from": "1", "to": "1", "repeat": "1"},
        "underlay": [{"file": one_page}],
        "staticId": ""
    })
    .to_string();
    let mut overlay_job = QPDFJob::new();
    overlay_job.initialize_from_json(&overlay_json).unwrap();
    assert_eq!(overlay_job.run().unwrap(), JobExitCode::Success);

    let attachment_output = tempdir.path().join("attachments.pdf");
    let attachment_json = serde_json::json!({
        "inputFile": attachment_pdf,
        "outputFile": attachment_output,
        "removeAttachment": ["attachment.txt"],
        "copyAttachmentsFrom": [{"file": attachment_pdf, "prefix": "copy-"}],
        "verbose": "",
        "staticId": ""
    })
    .to_string();
    let mut attachment_job = QPDFJob::new();
    attachment_job
        .initialize_from_json(&attachment_json)
        .unwrap();
    assert_eq!(attachment_job.run().unwrap(), JobExitCode::Success);
    assert!(attachment_output.exists());
}

#[test]
fn json_job_output_covers_v1_sections_images_outlines_encryption_and_schema() {
    let one_page =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let image_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/shared-stream-objstm.pdf");
    let outline_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/json-diff/direct-outlines.pdf");
    let encrypted_pdf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let v1_output = tempdir.path().join("v1.json");
    let v1_json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": v1_output,
        "json": "1",
        "jsonKey": ["objects", "objectinfo"],
        "staticId": ""
    })
    .to_string();
    let mut v1_job = QPDFJob::new();
    v1_job.initialize_from_json(&v1_json).unwrap();
    assert_eq!(v1_job.run().unwrap(), JobExitCode::Success);
    let v1_text = std::fs::read_to_string(v1_output).unwrap();
    assert!(v1_text.contains("\"objects\""));
    assert!(v1_text.contains("\"objectinfo\""));

    for (name, input) in [("images", image_pdf), ("outlines", outline_pdf.clone())] {
        let output = tempdir.path().join(format!("{name}.json"));
        let json = serde_json::json!({
            "inputFile": input,
            "outputFile": output,
            "json": "2",
            "jsonKey": ["pages"],
        })
        .to_string();
        let mut job = QPDFJob::new();
        job.initialize_from_json(&json).unwrap();
        assert_eq!(job.run().unwrap(), JobExitCode::Success);
        assert!(std::fs::metadata(output).unwrap().len() > 0);
    }

    let outline_output = tempdir.path().join("outline-section.json");
    let outline_json = serde_json::json!({
        "inputFile": outline_pdf,
        "outputFile": outline_output,
        "json": "2",
        "jsonKey": ["outlines"],
    })
    .to_string();
    let mut outline_job = QPDFJob::new();
    outline_job.initialize_from_json(&outline_json).unwrap();
    assert_eq!(outline_job.run().unwrap(), JobExitCode::Success);
    assert!(std::fs::metadata(outline_output).unwrap().len() > 0);

    let encrypted_output = tempdir.path().join("encrypted.json");
    let encrypted_json = serde_json::json!({
        "inputFile": encrypted_pdf,
        "outputFile": encrypted_output,
        "password": "user-v4-aes",
        "json": "2",
        "jsonKey": ["encrypt"],
        "showEncryptionKey": ""
    })
    .to_string();
    let mut encrypted_job = QPDFJob::new();
    encrypted_job.initialize_from_json(&encrypted_json).unwrap();
    assert_eq!(encrypted_job.run().unwrap(), JobExitCode::Success);

    let schema_output = tempdir.path().join("schema.json");
    let schema_json = serde_json::json!({
        "inputFile": one_page,
        "outputFile": schema_output,
        "jsonOutput": "2",
        "testJsonSchema": ""
    })
    .to_string();
    let mut schema_job = QPDFJob::new();
    schema_job.initialize_from_json(&schema_json).unwrap();
    assert_eq!(schema_job.run().unwrap(), JobExitCode::Success);
}

#[test]
fn json_job_stdout_schema_uses_the_job_save_pipeline() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let (logger, state) = logger_with_info_sink();
    let save_state = Arc::new(Mutex::new(SinkState::default()));
    logger
        .set_save(
            Some(PipelineHandle::new(RecordingSink {
                state: Arc::clone(&save_state),
            })),
            false,
        )
        .unwrap();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    let json = serde_json::json!({
        "inputFile": input,
        "jsonOutput": "2",
        "testJsonSchema": ""
    })
    .to_string();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(save_state.lock().unwrap().bytes.starts_with(b"{\n"));
    assert!(state.lock().unwrap().bytes.is_empty());
}

#[test]
fn json_job_json_input_and_replace_input_cover_success_and_failure_boundaries() {
    let minimal = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let json_input = tempdir.path().join("input.json");
    std::fs::write(&json_input, COMPLETE_JSON).unwrap();
    let created_output = tempdir.path().join("created.pdf");
    let create_json = serde_json::json!({
        "inputFile": json_input,
        "jsonInput": "",
        "outputFile": created_output,
        "staticId": ""
    })
    .to_string();
    let mut create_job = QPDFJob::new();
    create_job.initialize_from_json(&create_json).unwrap();
    assert_eq!(create_job.run().unwrap(), JobExitCode::Success);

    let bad_json_input = tempdir.path().join("bad.json");
    std::fs::write(&bad_json_input, b"{").unwrap();
    let bad_output = tempdir.path().join("bad.pdf");
    let bad_json = serde_json::json!({
        "inputFile": bad_json_input,
        "jsonInput": "",
        "outputFile": bad_output
    })
    .to_string();
    let mut bad_job = QPDFJob::new();
    bad_job.initialize_from_json(&bad_json).unwrap();
    assert_eq!(bad_job.run().unwrap(), JobExitCode::Error);

    let missing_update = tempdir.path().join("missing-update.json");
    let missing_update_output = tempdir.path().join("missing-update.pdf");
    let missing_update_json = serde_json::json!({
        "inputFile": minimal,
        "outputFile": missing_update_output,
        "updateFromJson": missing_update,
        "pages": [{"file": "."}]
    })
    .to_string();
    let mut missing_update_job = QPDFJob::new();
    missing_update_job
        .initialize_from_json(&missing_update_json)
        .unwrap();
    assert_eq!(missing_update_job.run().unwrap(), JobExitCode::Error);

    let missing_update_primary = serde_json::json!({
        "inputFile": minimal,
        "outputFile": tempdir.path().join("missing-update-primary.pdf"),
        "updateFromJson": missing_update
    })
    .to_string();
    let mut missing_update_primary_job = QPDFJob::new();
    missing_update_primary_job
        .initialize_from_json(&missing_update_primary)
        .unwrap();
    assert_eq!(
        missing_update_primary_job.run().unwrap(),
        JobExitCode::Error
    );

    let json_primary = tempdir.path().join("json-primary.json");
    std::fs::write(&json_primary, COMPLETE_JSON).unwrap();
    let json_missing_update = serde_json::json!({
        "inputFile": json_primary,
        "jsonInput": "",
        "outputFile": tempdir.path().join("json-missing-update.pdf"),
        "updateFromJson": missing_update
    })
    .to_string();
    let mut json_missing_update_job = QPDFJob::new();
    json_missing_update_job
        .initialize_from_json(&json_missing_update)
        .unwrap();
    assert_eq!(json_missing_update_job.run().unwrap(), JobExitCode::Error);

    let empty_missing_update = serde_json::json!({
        "empty": "",
        "outputFile": tempdir.path().join("empty-missing-update.pdf"),
        "updateFromJson": missing_update
    })
    .to_string();
    let mut empty_missing_update_job = QPDFJob::new();
    empty_missing_update_job
        .initialize_from_json(&empty_missing_update)
        .unwrap();
    assert_eq!(empty_missing_update_job.run().unwrap(), JobExitCode::Error);

    let replace_input = tempdir.path().join("replace.pdf");
    std::fs::copy(&minimal, &replace_input).unwrap();
    let replace_json = serde_json::json!({
        "inputFile": replace_input,
        "replaceInput": "",
        "staticId": ""
    })
    .to_string();
    let mut replace_job = QPDFJob::new();
    replace_job.initialize_from_json(&replace_json).unwrap();
    assert_eq!(replace_job.run().unwrap(), JobExitCode::Success);
    assert!(replace_input.exists());

    let failed_replace_input = tempdir.path().join("failed-replace.pdf");
    std::fs::copy(&minimal, &failed_replace_input).unwrap();
    let failed_replace_json = serde_json::json!({
        "inputFile": failed_replace_input,
        "replaceInput": "",
        "removeAttachment": ["missing"]
    })
    .to_string();
    let mut failed_replace_job = QPDFJob::new();
    failed_replace_job
        .initialize_from_json(&failed_replace_json)
        .unwrap();
    assert_eq!(failed_replace_job.run().unwrap(), JobExitCode::Error);
    assert!(failed_replace_input.exists());
}

#[test]
fn json_job_show_encryption_and_setter_boundaries_are_reachable() {
    let encrypted = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let (logger, info_state) = logger_with_info_sink();
    let mut job = QPDFJob::new();
    job.set_logger(logger);
    job.set_input_file("placeholder.pdf").unwrap();
    assert!(job.set_input_file("second.pdf").is_err());
    job.set_output_file("placeholder-output.pdf").unwrap();
    assert!(job.set_output_file("second-output.pdf").is_err());
    job.set_password(b"placeholder-password".to_vec());

    let json = serde_json::json!({
        "inputFile": encrypted,
        "password": "user-v4-aes",
        "showEncryption": ""
    })
    .to_string();
    let mut inspection_job = QPDFJob::new();
    inspection_job.set_logger(job.logger());
    inspection_job.initialize_from_json(&json).unwrap();
    assert_eq!(inspection_job.run().unwrap(), JobExitCode::Success);
    assert!(info_state
        .lock()
        .unwrap()
        .bytes
        .windows(b"R = 4".len())
        .any(|window| { window == b"R = 4" }));
}

#[test]
fn json_job_rejects_conflicting_output_configuration_and_bad_page_labels() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();

    let conflicting = [
        serde_json::json!({
            "inputFile": input,
            "outputFile": tempdir.path().join("out.pdf"),
            "replaceInput": ""
        }),
        serde_json::json!({"empty": "", "replaceInput": ""}),
        serde_json::json!({"inputFile": input, "replaceInput": "", "json": "2"}),
        serde_json::json!({
            "inputFile": input,
            "outputFile": tempdir.path().join("out.pdf"),
            "check": ""
        }),
    ];
    for value in conflicting {
        let mut job = QPDFJob::new();
        assert!(job.initialize_from_json(&value.to_string()).is_err());
    }

    let bad_labels = serde_json::json!({
        "inputFile": input,
        "outputFile": tempdir.path().join("bad-labels.pdf"),
        "setPageLabels": ["1:D", "2:D"]
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&bad_labels).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Error);
}

#[test]
fn json_job_replace_input_keeps_original_when_input_has_warnings() {
    let repairable = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("warning-replace.pdf");
    std::fs::copy(repairable, &input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "replaceInput": "",
        "staticId": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    assert!(input.exists());
    assert!(Path::new(&format!("{}.~qpdf-orig", input.display())).exists());
}

#[test]
fn write_qpdf_completes_replace_input_without_run() {
    let minimal = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join("direct-replace.pdf");
    std::fs::copy(&minimal, &input).unwrap();
    let json = serde_json::json!({
        "inputFile": input,
        "replaceInput": "",
        "staticId": ""
    })
    .to_string();
    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();

    // Bypass `run()` deliberately: qpdf's `writeOutfile` completes the
    // replace-input rename itself for every caller that reaches it
    // (`libqpdf/QPDFJob.cc:3057-3086`), so a consumer using the public
    // `create_qpdf()`/`write_qpdf()` two-stage contract directly must see
    // the same completed rename `run()` would have produced.
    let mut pdf = job.create_qpdf().unwrap().expect("input should open");
    job.write_qpdf(&mut pdf).unwrap();
    assert_eq!(job.get_exit_code(), JobExitCode::Success);

    assert!(
        input.exists(),
        "the temporary output must be renamed onto the input path"
    );
    assert!(
        !Path::new(&format!("{}.~qpdf-temp#", input.display())).exists(),
        "the temporary output must be renamed away, not left behind"
    );
    assert!(
        !Path::new(&format!("{}.~qpdf-orig", input.display())).exists(),
        "a warning-free replace-input deletes its backup"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_input_file_opens_a_literal_non_utf8_path() {
    let directory = tempfile::tempdir().unwrap();
    let input = non_utf8_path(directory.path(), b"input-\x80.pdf");
    let output = directory.path().join("output.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_json_bytes(&job_json_with_paths(&input, &output))
        .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(output.exists(), "qpdf job must open the exact input path");
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_output_file_creates_a_literal_non_utf8_path() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.pdf");
    let output = non_utf8_path(directory.path(), b"output-\x80.pdf");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf"),
        &input,
    )
    .unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_json_bytes(&job_json_with_paths(&input, &output))
        .unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(
        output.exists(),
        "qpdf job must create the exact output path"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn job_json_replace_input_preserves_non_utf8_derived_backup_path() {
    let repairable = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver/repairable_input.pdf");
    let directory = tempfile::tempdir().unwrap();
    let input = non_utf8_path(directory.path(), b"warning-replace-\x80.pdf");
    std::fs::copy(repairable, &input).unwrap();

    let mut job = QPDFJob::new();
    let mut json = b"{\"inputFile\":\"".to_vec();
    json.extend_from_slice(input.as_os_str().as_bytes());
    json.extend_from_slice(b"\",\"replaceInput\":\"\"}");
    job.initialize_from_json_bytes(&json).unwrap();

    assert_eq!(job.run().unwrap(), JobExitCode::Warning);
    let mut backup = input.as_os_str().to_os_string();
    backup.push(".~qpdf-orig");
    assert!(PathBuf::from(backup).exists());
}

/// `Config::pages().pageSpec()` (`QPDFJob_config.cc`'s pages sub-config) and
/// the JSON `pages` array both populate the same `page_specs` queue consumed
/// by `QPDFJob::handlePageSpecs` (`QPDFJob.cc:2359-2434`). Confirm
/// `QPDFJobConfig::add_page_spec` reaches identical byte output to the
/// already-covered JSON path for a single-source in-place page selection, so
/// a CLI consumer can build a job through the programmatic Config surface
/// instead of encoding an argv/JSON string.
#[test]
fn config_add_page_spec_matches_the_json_configured_path_single_source() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let via_config = tempdir.path().join("via-config.pdf");
    let via_json = tempdir.path().join("via-json.pdf");

    let mut config_job = QPDFJob::new();
    config_job
        .config()
        .input_file(&input)
        .unwrap()
        .output_file(&via_config)
        .unwrap()
        .deterministic_id()
        .add_page_spec(".", "2-3,1", None)
        .unwrap();
    assert_eq!(config_job.run().unwrap(), JobExitCode::Success);

    // Both jobs must use qpdf's `/ID` -> deterministic mode
    // (`--deterministic-id`, `QPDFWriter.cc`'s hash-of-content ID instead of
    // random bytes) since two independently run jobs otherwise differ only
    // in the random trailer `/ID`, which is not what this test checks.
    let mut json_job = QPDFJob::new();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": via_json,
        "deterministicId": "",
        "pages": [{"file": ".", "range": "2-3,1"}],
    })
    .to_string();
    json_job.initialize_from_json(&json).unwrap();
    assert_eq!(json_job.run().unwrap(), JobExitCode::Success);

    assert_eq!(
        std::fs::read(&via_config).unwrap(),
        std::fs::read(&via_json).unwrap(),
        "the Config builder and the JSON path must produce byte-identical output"
    );

    // The two routes push the same `JobPageConfig` and then merge, so their
    // agreement alone cannot catch a shared downstream error. Anchor the
    // result against the selection qpdf's `1-z`-style range actually means:
    // `2-3,1` must emit the source's pages 2, 3, 1 in that order.
    let source_order = {
        let mut source = Pdf::open(BufReader::new(File::open(&input).unwrap()))
            .expect("three-page fixture opens");
        let refs = flpdf::pages::page_refs(&mut source).expect("source pages resolve");
        assert_eq!(refs.len(), 3, "the fixture has three pages");
        [refs[1], refs[2], refs[0]]
            .into_iter()
            .map(|page| {
                flpdf::pages::page_content_bytes(&mut source, page)
                    .expect("source page content decodes")
            })
            .collect::<Vec<_>>()
    };
    let mut selected =
        Pdf::open(BufReader::new(File::open(&via_config).unwrap())).expect("selected output opens");
    let selected_refs = flpdf::pages::page_refs(&mut selected).expect("selected pages resolve");
    assert_eq!(selected_refs.len(), 3, "`2-3,1` selects three pages");
    for (index, page) in selected_refs.into_iter().enumerate() {
        assert_eq!(
            flpdf::pages::page_content_bytes(&mut selected, page)
                .expect("selected page content decodes"),
            source_order[index],
            "selected page {index} must be the source page `2-3,1` names"
        );
    }
}

/// qpdf's Config pageSpec accepts range syntax and defers page-count bounds
/// until the page job resolves the source document.
#[test]
fn config_add_page_spec_defers_range_bounds_like_qpdf() {
    let mut job = QPDFJob::new();
    job.config()
        .add_page_spec(".", "0", None)
        .expect("qpdf's PagesConfig::pageSpec accepts syntax-only range bounds");
}

#[test]
fn config_page_specs_reject_a_json_pages_group_after_config_pages() {
    let mut job = QPDFJob::new();
    job.config()
        .add_page_spec("first.pdf", "1", None)
        .expect("the first Config page group is accepted");

    let error = job
        .initialize_from_json_partial(r#"{"pages":[{"file":"second.pdf"}]}"#)
        .expect_err("qpdf's Config::pages() guard must reject a second pages group");
    assert!(matches!(
        error,
        Error::Usage(usage) if usage.to_string() == "--pages may only be specified one time"
    ));
}

/// `Config::emptyInput` selects the empty primary before qpdf's create/write
/// lifecycle runs, so a caller must be able to configure it without encoding
/// an equivalent job-JSON document.
#[test]
fn config_empty_input_runs_the_canonical_inspection_lifecycle() {
    let mut job = QPDFJob::new();
    job.config()
        .empty_input()
        .expect("empty input should be a valid primary selector")
        .show_pages();

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
}

#[test]
fn config_empty_input_rejects_a_preselected_input() {
    let mut job = QPDFJob::new();
    job.config()
        .input_file("input.pdf")
        .expect("the first input selector should be accepted");

    let error = job
        .config()
        .empty_input()
        .err()
        .expect("qpdf rejects empty input after an input file");
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "empty input can't be used since input file has already been given"
    ));
}

#[test]
fn config_empty_input_rejects_a_duplicate_empty_selector() {
    let mut job = QPDFJob::new();
    job.config()
        .empty_input()
        .expect("the first empty selector should be accepted");

    let error = job
        .config()
        .empty_input()
        .err()
        .expect("qpdf rejects a duplicate empty selector");
    assert!(matches!(
        error,
        Error::Usage(usage)
            if usage.to_string() == "empty input can't be used since input file has already been given"
    ));
}

#[test]
fn config_collate_zero_selects_no_pages() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("collate-zero.pdf");

    let mut job = QPDFJob::new();
    job.config()
        .input_file(&input)
        .unwrap()
        .output_file(&output)
        .unwrap()
        .add_page_spec(".", "1-2", None)
        .unwrap()
        .add_page_spec(".", "3", None)
        .unwrap()
        .collate("0")
        .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert!(
        flpdf::pages::page_refs(&mut pdf).unwrap().is_empty(),
        "qpdf collate=0 must produce an empty selected-page result"
    );
}

#[test]
fn config_collate_appends_values_in_declaration_order() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let output = tempdir.path().join("collate-append.pdf");

    let mut job = QPDFJob::new();
    job.config()
        .input_file(&input)
        .unwrap()
        .output_file(&output)
        .unwrap()
        .add_page_spec(".", "1", None)
        .unwrap()
        .add_page_spec(".", "2", None)
        .unwrap()
        .collate("0")
        .unwrap()
        .collate("1")
        .unwrap();
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    let mut pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert_eq!(
        flpdf::pages::page_refs(&mut pdf).unwrap().len(),
        1,
        "collate values must append as [0, 1], not replace the first group"
    );
}

#[test]
fn json_page_specs_reject_a_config_pages_group_after_json_pages() {
    let mut job = QPDFJob::new();
    job.initialize_from_json_partial(r#"{"pages":[{"file":"first.pdf"}]}"#)
        .expect("the first JSON page group is accepted");

    let error = match job.config().add_page_spec("second.pdf", "1", None) {
        Ok(_) => panic!("qpdf's Config::pages() guard must reject a second pages group"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        Error::Usage(usage) if usage.to_string() == "--pages may only be specified one time"
    ));
}

#[test]
fn config_plaintext_copy_encryption_is_a_noop_for_an_encrypted_primary() {
    let tempdir = tempfile::tempdir().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let donor = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let output = tempdir.path().join("plaintext-output.pdf");

    let mut job = QPDFJob::new();
    job.set_password(b"user-v4-aes".to_vec());
    job.config()
        .input_file(&input)
        .unwrap()
        .output_file(&output)
        .unwrap()
        .copy_encryption(&donor, Vec::new());

    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    let pdf = Pdf::open(BufReader::new(File::open(output).unwrap())).unwrap();
    assert!(!pdf.is_encrypted());
}

#[test]
fn json_page_spec_uses_copy_encryption_password_when_page_password_is_unspecified() {
    let tempdir = tempfile::tempdir().unwrap();
    let plaintext =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/three-page.pdf");
    let encrypted = tempdir.path().join("encrypted-three-page.pdf");
    let mut source = Pdf::open(BufReader::new(File::open(&plaintext).unwrap())).unwrap();
    let mut writer = PdfWriter::new(&mut source);
    writer.set_static_id(true);
    writer.set_encryption_parameters(EncryptParams::v4_aes128(
        b"user-v4-aes".to_vec(),
        b"owner-v4-aes".to_vec(),
    ));
    writer.set_output_file(&encrypted).unwrap();
    writer.write().unwrap();

    let output = tempdir.path().join("pages-from-encryption-file.pdf");
    let json = serde_json::json!({
        "empty": "",
        "outputFile": output,
        "copyEncryption": encrypted,
        "encryptionFilePassword": "user-v4-aes",
        "pages": [{"file": encrypted, "range": "1"}],
        "staticId": ""
    })
    .to_string();

    let mut job = QPDFJob::new();
    job.initialize_from_json(&json).unwrap();
    assert_eq!(
        job.run().unwrap(),
        JobExitCode::Success,
        "an omitted page password must fall back to --encryption-file-password"
    );
    assert!(output.is_file());

    let explicit_empty_output = tempdir.path().join("explicit-empty-password.pdf");
    let explicit_empty_json = serde_json::json!({
        "empty": "",
        "outputFile": explicit_empty_output,
        "copyEncryption": encrypted,
        "encryptionFilePassword": "user-v4-aes",
        "pages": [{"file": encrypted, "password": "", "range": "1"}],
        "staticId": ""
    })
    .to_string();
    let mut explicit_empty_job = QPDFJob::new();
    explicit_empty_job
        .initialize_from_json(&explicit_empty_json)
        .unwrap();
    assert_eq!(
        explicit_empty_job.run().unwrap(),
        JobExitCode::Error,
        "an explicit empty page password must bypass the encryption-file fallback"
    );
}

#[test]
fn apply_transformations_preserves_typed_copy_donor_password_error() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/minimal.pdf");
    let donor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/encrypted/v4-aes-128-r4.pdf");
    let mut primary = Pdf::open(BufReader::new(File::open(&input).unwrap())).unwrap();

    let mut job = QPDFJob::new();
    job.config()
        .copy_attachments_from(donor, b"wrong-password".to_vec(), Vec::new());
    let error = job
        .apply_transformations(&mut primary)
        .expect_err("the encrypted donor must reject the wrong password");

    let typed_bad_password = match &error {
        Error::Encrypted(EncryptedError::BadPassword) => true,
        Error::OpenFailure { source, .. } => {
            matches!(
                source.as_ref(),
                Error::Encrypted(EncryptedError::BadPassword)
            )
        }
        _ => false,
    };
    assert!(
        typed_bad_password,
        "copy donor authentication must remain typed, got {error:?}"
    );
}

/// `Config::addAttachment` (`QPDFJob_config.cc:894-936`) and the JSON
/// `addAttachment` array both populate the same `attachments_to_add` queue
/// consumed by `QPDFJob::addAttachments` (`QPDFJob.cc:2044-2083`). Confirm
/// `QPDFJobConfig::add_attachment` reaches the identical byte output as the
/// already-covered JSON path, so a CLI consumer can build a job through the
/// programmatic Config surface instead of encoding an argv/JSON string.
#[test]
fn config_add_attachment_matches_the_json_configured_path() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let attachment = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let via_config = tempdir.path().join("via-config.pdf");
    let via_json = tempdir.path().join("via-json.pdf");

    let mut config_job = QPDFJob::new();
    config_job
        .config()
        .input_file(&input)
        .unwrap()
        .output_file(&via_config)
        .unwrap()
        .deterministic_id()
        .add_attachment(AttachmentAddOptions {
            path: attachment.clone(),
            key: b"a.pdf".to_vec(),
            filename: b"a.pdf".to_vec(),
            mimetype: None,
            description: None,
            creation_date: None,
            modification_date: None,
            replace: false,
            verbose: false,
        });
    assert_eq!(config_job.run().unwrap(), JobExitCode::Success);

    // Both jobs must use qpdf's `/ID` -> deterministic mode
    // (`--deterministic-id`, `QPDFWriter.cc`'s hash-of-content ID instead of
    // random bytes) since two independently run jobs otherwise differ only
    // in the random trailer `/ID`, which is not what this test checks.
    let mut json_job = QPDFJob::new();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": via_json,
        "deterministicId": "",
        "addAttachment": [{"file": attachment, "key": "a.pdf", "filename": "a.pdf"}],
    })
    .to_string();
    json_job.initialize_from_json(&json).unwrap();
    assert_eq!(json_job.run().unwrap(), JobExitCode::Success);

    assert_eq!(
        std::fs::read(&via_config).unwrap(),
        std::fs::read(&via_json).unwrap(),
        "the Config builder and the JSON path must produce byte-identical output"
    );
}

/// `Config::removeAttachment` (`QPDFJob_config.cc:938-947`) queues the same
/// key removal `QPDFJob::handleTransformations` applies for the JSON
/// `removeAttachment` array (`QPDFJob.cc:2223-2233`).
#[test]
fn config_remove_attachment_matches_the_json_configured_path() {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let via_config = tempdir.path().join("via-config.pdf");
    let via_json = tempdir.path().join("via-json.pdf");

    let key = {
        let file = File::open(&input).unwrap();
        let mut pdf = Pdf::open(BufReader::new(file)).unwrap();
        let names = pdf.embedded_files().get_embedded_files().unwrap();
        names
            .into_keys()
            .next()
            .expect("fixture must have at least one attachment")
    };

    let mut config_job = QPDFJob::new();
    config_job
        .config()
        .input_file(&input)
        .unwrap()
        .output_file(&via_config)
        .unwrap()
        .deterministic_id()
        .remove_attachment(key.clone());
    assert_eq!(config_job.run().unwrap(), JobExitCode::Success);

    let mut json_job = QPDFJob::new();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": via_json,
        "deterministicId": "",
        "removeAttachment": [String::from_utf8_lossy(&key)],
    })
    .to_string();
    json_job.initialize_from_json(&json).unwrap();
    assert_eq!(json_job.run().unwrap(), JobExitCode::Success);

    assert_eq!(
        std::fs::read(&via_config).unwrap(),
        std::fs::read(&via_json).unwrap(),
        "the Config builder and the JSON path must produce byte-identical output"
    );
}

/// `Config::copyAttachmentsFrom` (`QPDFJob_config.cc:948-967`) queues the
/// same donor `QPDFJob::copyAttachments` copies for the JSON
/// `copyAttachmentsFrom` array (`QPDFJob.cc:2089-2135`).
#[test]
fn config_copy_attachments_from_matches_the_json_configured_path() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let donor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/attachment-two-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let via_config = tempdir.path().join("via-config.pdf");
    let via_json = tempdir.path().join("via-json.pdf");

    let mut config_job = QPDFJob::new();
    config_job
        .config()
        .input_file(&input)
        .unwrap()
        .output_file(&via_config)
        .unwrap()
        .deterministic_id()
        .copy_attachments_from(donor.clone(), Vec::new(), b"donor-".to_vec());
    assert_eq!(config_job.run().unwrap(), JobExitCode::Success);

    let mut json_job = QPDFJob::new();
    let json = serde_json::json!({
        "inputFile": input,
        "outputFile": via_json,
        "deterministicId": "",
        "copyAttachmentsFrom": [{"file": donor, "prefix": "donor-"}],
    })
    .to_string();
    json_job.initialize_from_json(&json).unwrap();
    assert_eq!(json_job.run().unwrap(), JobExitCode::Success);

    assert_eq!(
        std::fs::read(&via_config).unwrap(),
        std::fs::read(&via_json).unwrap(),
        "the Config builder and the JSON path must produce byte-identical output"
    );
}

/// A missing `--password-file` must surface as a usage error.
///
/// qpdf's `Config::passwordFile` lets `QUtil::read_lines_from_file` throw a
/// `QPDFSystemError` (`QPDFJob_config.cc:661-668`); `ArgParser::parseOptions`
/// catches every callback `runtime_error` and routes it through `usage()`
/// (`QPDFJob_argv.cc:407-415`). A bare file-I/O error would give CLI-style
/// consumers the wrong class and drop the usage framing.
#[test]
fn missing_password_file_is_a_usage_error() {
    let tempdir = tempfile::tempdir().unwrap();
    let missing = tempdir.path().join("absent-password.txt");
    let mut job = QPDFJob::new();
    let error = job
        .initialize_from_raw_argv(&[
            b"qpdfjob".to_vec(),
            b"in.pdf".to_vec(),
            format!("--password-file={}", missing.display()).into_bytes(),
            b"out.pdf".to_vec(),
        ])
        .expect_err("a missing password file must fail");
    assert!(
        matches!(error, flpdf::Error::Usage(_)),
        "expected a usage error, got {error:?}"
    );
}

/// A second argv initialization restarts the lifecycle.
///
/// `initialize_from_json_with_partial` layers onto the existing configuration
/// only while the job has not run. Without clearing that flag here, a
/// `--job-json-file` occurrence in the second argv would take the post-run
/// fresh-configuration branch and discard the input/output parsed before it.
#[test]
fn argv_initialization_after_a_run_keeps_later_job_json_layering() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let tempdir = tempfile::tempdir().unwrap();
    let first_output = tempdir.path().join("first.pdf");
    let second_output = tempdir.path().join("second.pdf");
    let job_json = tempdir.path().join("job.json");
    std::fs::write(&job_json, br#"{"qdf": ""}"#).unwrap();

    let mut job = QPDFJob::new();
    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        first_output.to_string_lossy().into_owned().into_bytes(),
    ])
    .expect("first initialization");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);

    job.initialize_from_raw_argv(&[
        b"qpdfjob".to_vec(),
        fixture.to_string_lossy().into_owned().into_bytes(),
        format!("--job-json-file={}", job_json.display()).into_bytes(),
        second_output.to_string_lossy().into_owned().into_bytes(),
    ])
    .expect("the input and output parsed before --job-json-file must survive");
    assert_eq!(job.run().unwrap(), JobExitCode::Success);
    assert!(
        second_output.exists(),
        "the second run must write its output"
    );
}
