use assert_cmd::Command;

const QPDF_VERSION_OUTPUT: &str =
    "qpdf version 11.9.0\nRun qpdf --copyright to see copyright and license information.\n";

const QPDF_COPYRIGHT_OUTPUT: &str = "qpdf version 11.9.0\n\
\n\
Copyright (c) 2005-2024 Jay Berkenbilt\n\
QPDF is licensed under the Apache License, Version 2.0 (the \"License\");\n\
you may not use this file except in compliance with the License.\n\
You may obtain a copy of the License at\n\
\n\
  http://www.apache.org/licenses/LICENSE-2.0\n\
\n\
Unless required by applicable law or agreed to in writing, software\n\
distributed under the License is distributed on an \"AS IS\" BASIS,\n\
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.\n\
See the License for the specific language governing permissions and\n\
limitations under the License.\n\
\n\
Versions of qpdf prior to version 7 were released under the terms\n\
of version 2.0 of the Artistic License. At your option, you may\n\
continue to consider qpdf to be licensed under those terms. Please\n\
see the manual for additional information.\n";

#[test]
fn qpdf_version_prints_the_pinned_qpdf_version_without_opening_input() {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--version")
        .assert()
        .success()
        .stdout(QPDF_VERSION_OUTPUT)
        .stderr("");
}

#[test]
fn qpdf_copyright_prints_the_pinned_license_text_without_opening_input() {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .arg("--copyright")
        .assert()
        .success()
        .stdout(QPDF_COPYRIGHT_OUTPUT)
        .stderr("");
}
