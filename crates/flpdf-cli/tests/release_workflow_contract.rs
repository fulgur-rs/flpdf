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
fn workflow_runs_are_serialized_without_dropping_pending_pushes() {
    let preamble = workflow_preamble().replace("\r\n", "\n");

    assert!(preamble.contains(
        "\nconcurrency:\n  group: release-plz-${{ github.ref }}\n  cancel-in-progress: false\n  queue: max"
    ));
}

#[test]
fn release_pr_yields_to_older_queued_workflow_runs() {
    let gate = job_block("check-release-pr-turn");
    let release_pr = job_block("release-pr");

    assert!(gate.contains("needs: [check-releases, release]"));
    assert!(gate.contains("permissions:\n      actions: read\n    outputs:"));
    assert!(gate.contains("outputs:\n      should_run: ${{ steps.turn.outputs.should_run }}"));
    assert!(gate.contains("set -euo pipefail"));
    assert!(gate.contains("CURRENT_RUN_NUMBER: ${{ github.run_number }}"));
    assert!(gate.contains(
        "actions/workflows/release-plz.yml/runs?branch=$BRANCH&status=queued&per_page=100"
    ));
    assert!(gate.contains(".run_number < $current"));
    assert!(gate.contains("echo \"should_run=false\" >> \"$GITHUB_OUTPUT\""));
    assert!(gate.contains("echo \"should_run=true\" >> \"$GITHUB_OUTPUT\""));

    assert!(release_pr.contains("needs: [check-releases, release, check-release-pr-turn]"));
    assert!(release_pr.contains("needs.check-release-pr-turn.result == 'success'"));
    assert!(release_pr.contains("needs.check-release-pr-turn.outputs.should_run == 'true'"));
}

#[test]
fn release_pr_waits_for_publish_when_a_release_is_detected() {
    let block = job_block("release-pr");

    assert!(block.contains("needs: [check-releases, release, check-release-pr-turn]"));
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
