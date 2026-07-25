use std::fs;
use std::path::Path;

const CI_WORKFLOW: &str = include_str!("../../../.github/workflows/ci.yml");
const WHOLE_FILE_ZLIB_GATE: &str = "#![cfg(feature = \"qpdf-zlib-compat\")]";

#[test]
fn ci_runs_every_whole_file_qpdf_zlib_compat_test() {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("flpdf-cli must live under the workspace crates directory");
    let mut missing = Vec::new();

    for crate_entry in
        fs::read_dir(crates_dir).expect("workspace crates directory must be readable")
    {
        let crate_entry = crate_entry.expect("workspace crate entry must be readable");
        let tests_dir = crate_entry.path().join("tests");
        if !tests_dir.is_dir() {
            continue;
        }

        let crate_name = crate_entry.file_name();
        let crate_name = crate_name.to_string_lossy();
        for test_entry in fs::read_dir(tests_dir).expect("crate tests directory must be readable") {
            let test_entry = test_entry.expect("test entry must be readable");
            let path = test_entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }

            let source = fs::read_to_string(&path).expect("integration test must be readable");
            if !source.lines().any(|line| line == WHOLE_FILE_ZLIB_GATE) {
                continue;
            }

            let test_name = path
                .file_stem()
                .expect("integration test must have a file stem")
                .to_string_lossy();
            let command = format!(
                "cargo test -p {crate_name} --features qpdf-zlib-compat --test {test_name}"
            );
            if !CI_WORKFLOW.contains(&command) {
                missing.push(format!("{crate_name}/{test_name}"));
            }
        }
    }

    missing.sort();
    assert!(
        missing.is_empty(),
        "ci.yml does not run whole-file qpdf-zlib-compat tests: {}",
        missing.join(", ")
    );
}
