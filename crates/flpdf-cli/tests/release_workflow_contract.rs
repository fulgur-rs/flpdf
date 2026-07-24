const RELEASE_WORKFLOW: &str = include_str!("../../../.github/workflows/release-plz.yml");

fn workflow_preamble() -> &'static str {
    RELEASE_WORKFLOW
        .split_once("\njobs:")
        .map(|(preamble, _)| preamble)
        .expect("release-plz.yml must contain a top-level jobs mapping")
}

fn job_block(name: &str) -> String {
    let marker = format!("  {name}:");
    let mut found = false;
    let mut block = Vec::new();

    for line in RELEASE_WORKFLOW.lines() {
        if line == marker {
            found = true;
        } else if found
            && line.starts_with("  ")
            && !line.starts_with("    ")
            && line.ends_with(':')
        {
            break;
        }

        if found {
            block.push(line);
        }
    }

    assert!(found, "job {name:?} is absent from release-plz.yml");
    block.join("\n")
}

#[test]
fn workflow_keeps_release_plz_two_job_boilerplate() {
    let preamble = workflow_preamble().replace("\r\n", "\n");
    let release = job_block("release");
    let release_pr = job_block("release-pr");

    assert!(!preamble.contains("\nconcurrency:"));
    assert!(!RELEASE_WORKFLOW.contains("check-release-pr-turn:"));
    assert!(!RELEASE_WORKFLOW.contains("actions/workflows/release-plz.yml/runs"));
    assert!(release.contains("command: release"));
    assert!(release_pr.contains("command: release-pr"));
    assert!(release_pr.contains("group: release-plz-pr-${{ github.ref }}"));
    assert!(release_pr.contains("cancel-in-progress: false"));
}

#[test]
fn release_pr_does_not_reimplement_registry_readiness() {
    let release_pr = job_block("release-pr");

    assert!(!release_pr.contains("crates.io/api/"));
    assert!(!release_pr.contains("cargo metadata"));
    assert!(!release_pr.contains("curl "));
    assert!(!release_pr.contains("sleep "));
}

#[test]
fn release_pr_waits_for_publish_when_a_release_is_detected() {
    let block = job_block("release-pr");

    assert!(block.contains("needs: [check-releases, release]"));
    assert!(block.contains("always()"));
    assert!(block.contains("!cancelled()"));
    assert!(block.contains("needs.check-releases.result == 'success'"));
    assert!(block.contains("needs.check-releases.outputs.has_releases != 'true'"));
    assert!(block.contains("needs.release.result == 'success'"));
}

#[test]
fn publishing_remains_independent_of_next_release_pr_maintenance() {
    let block = job_block("release");

    assert!(block.contains("needs: [check-releases]"));
    assert!(!block.contains("needs: [check-releases, release-pr]"));
}
