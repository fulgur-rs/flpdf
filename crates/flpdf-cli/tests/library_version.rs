use assert_cmd::Command;
use predicates::prelude::*;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

fn qpdf_version_output() -> String {
    format!(
        "qpdf version 11.9.0{EOL}Run qpdf --copyright to see copyright and license information.{EOL}"
    )
}

fn qpdf_copyright_output() -> String {
    format!(
        "qpdf version 11.9.0{EOL}\
         {EOL}\
         Copyright (c) 2005-2024 Jay Berkenbilt{EOL}\
         QPDF is licensed under the Apache License, Version 2.0 (the \"License\");{EOL}\
         you may not use this file except in compliance with the License.{EOL}\
         You may obtain a copy of the License at{EOL}{EOL}  http://www.apache.org/licenses/LICENSE-2.0{EOL}\
         {EOL}\
         Unless required by applicable law or agreed to in writing, software{EOL}\
         distributed under the License is distributed on an \"AS IS\" BASIS,{EOL}\
         WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.{EOL}\
         See the License for the specific language governing permissions and{EOL}\
         limitations under the License.{EOL}\
         {EOL}\
         Versions of qpdf prior to version 7 were released under the terms{EOL}\
         of version 2.0 of the Artistic License. At your option, you may{EOL}\
         continue to consider qpdf to be licensed under those terms. Please{EOL}\
         see the manual for additional information.{EOL}"
    )
}

#[test]
fn qpdf_version_prints_the_pinned_qpdf_version_without_opening_input() {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--version")
        .assert()
        .success()
        .stdout(qpdf_version_output())
        .stderr("");
}

#[test]
fn qpdf_copyright_prints_the_pinned_license_text_without_opening_input() {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--copyright")
        .assert()
        .success()
        .stdout(qpdf_copyright_output())
        .stderr("");
}

#[test]
fn qpdf_version_from_argfile_is_handled_after_expansion() {
    let directory = tempfile::tempdir().expect("argument-file directory");
    let path = directory.path().join("version.args");
    std::fs::write(&path, b"--version\n").expect("write version argument file");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(format!("@{}", path.display()))
        .assert()
        .success()
        .stdout(qpdf_version_output())
        .stderr("");
}

#[test]
fn version_is_not_a_sole_option_when_a_named_group_precedes_it() {
    // qpdf's sole-option check uses argc == 2 on the fully expanded argv,
    // before named-group parsing (QPDFArgParser.cc:437,478-483). A --version
    // that is not the sole expanded token (e.g. after an --overlay group) must
    // not be treated as the version request; qpdf rejects it as an
    // unrecognized argument (exit 2) rather than printing the version.
    let directory = tempfile::tempdir().expect("argument-file directory");
    let source = directory.path().join("source.pdf");
    std::fs::write(&source, b"%PDF-1.4\n%%EOF\n").expect("write overlay source");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args([
            "--overlay".to_string(),
            source.display().to_string(),
            "--".to_string(),
            "--version".to_string(),
        ])
        .assert()
        .failure()
        .code(2)
        .stdout(predicates::str::contains("qpdf version").not());
}

#[test]
fn qpdf_copyright_from_argfile_is_handled_after_expansion() {
    let directory = tempfile::tempdir().expect("argument-file directory");
    let path = directory.path().join("copyright.args");
    std::fs::write(&path, b"--copyright\n").expect("write copyright argument file");

    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg(format!("@{}", path.display()))
        .assert()
        .success()
        .stdout(qpdf_copyright_output())
        .stderr("");
}
