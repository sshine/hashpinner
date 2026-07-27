//! End-to-end tests of the binary's contract: exit codes, and what it writes.
//!
//! Every test here is offline. `--check` on its own needs no network by design, so
//! it is the mode these exercise; the pin and bump paths are covered against a fake
//! resolver in `hashpinner-core`.

use std::path::Path;

use assert_cmd::Command;
use tempfile::TempDir;

/// Exit code when files do not meet the criteria.
const FAILED: i32 = 1;
/// Exit code when hashpinner itself cannot run.
const TOOL_ERROR: i32 = 2;

/// A repository containing one workflow with the given `uses:` values.
fn repo(uses: &[&str]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let workflows = dir.path().join(".github/workflows");
    std::fs::create_dir_all(&workflows).expect("mkdir");

    let mut src = String::from("name: ci\non: push\njobs:\n  build:\n    steps:\n");
    for u in uses {
        src.push_str(&format!("      - uses: {u}\n"));
    }
    std::fs::write(workflows.join("ci.yml"), src).expect("write");
    dir
}

/// Add a composite action at `path` with the given `uses:` values.
fn local_action(dir: &Path, path: &str, uses: &[&str]) {
    let action = dir.join(path);
    std::fs::create_dir_all(&action).expect("mkdir");

    let mut src = String::from("name: build\nruns:\n  using: composite\n  steps:\n");
    for u in uses {
        src.push_str(&format!("      - uses: {u}\n"));
    }
    std::fs::write(action.join("action.yml"), src).expect("write");
}

fn hashpinner(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("hashpinner").expect("binary");
    cmd.current_dir(dir);
    cmd
}

fn workflow(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".github/workflows/ci.yml")).expect("read")
}

const PINNED: &str =
    "actions/checkout@8e8c483db84b4bee98b60c0593521ed34d9990e8 # v6.0.1, 2025-12-02";

#[test]
fn list_is_the_default_and_succeeds() {
    let dir = repo(&["actions/checkout@v4"]);
    hashpinner(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("actions/checkout"));
}

#[test]
fn check_fails_on_an_unpinned_third_party_action() {
    let dir = repo(&["softprops/action-gh-release@v2"]);
    hashpinner(dir.path())
        .arg("--check")
        .assert()
        .code(FAILED)
        .stdout(predicates::str::contains("not pinned"));
}

#[test]
fn check_passes_on_a_pinned_action() {
    let dir = repo(&[PINNED]);
    hashpinner(dir.path()).arg("--check").assert().success();
}

#[test]
fn the_default_allowlist_covers_actions_org() {
    let dir = repo(&["actions/checkout@v4"]);
    hashpinner(dir.path()).arg("--check").assert().success();
}

#[test]
fn no_allow_makes_it_strict() {
    let dir = repo(&["actions/checkout@v4"]);
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .code(FAILED);
}

#[test]
fn an_explicit_allowlist_replaces_the_default() {
    let dir = repo(&["actions/checkout@v4"]);
    hashpinner(dir.path())
        .args(["--check", "--allow", "someone/else"])
        .assert()
        .code(FAILED);
}

#[test]
fn a_mutable_docker_tag_fails() {
    let dir = repo(&["docker://alpine:3.8"]);
    hashpinner(dir.path())
        .arg("--check")
        .assert()
        .code(FAILED)
        .stdout(predicates::str::contains("mutable image reference"));
}

#[test]
fn a_digest_pinned_image_passes() {
    let dir = repo(&["docker://alpine@sha256:9c6f0724472873bb50a2ae67a9e7adcb"]);
    hashpinner(dir.path()).arg("--check").assert().success();
}

#[test]
fn a_local_action_never_fails() {
    let dir = repo(&["./.github/actions/build"]);
    local_action(dir.path(), ".github/actions/build", &[PINNED]);
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .success();
}

/// The reason a local action is allowed to pass without being pinned is that its
/// contents are checked instead. When they are not, `./path` launders an unpinned
/// third-party action through a reference that looks in-repo and reviewed.
#[test]
fn a_local_action_is_followed_into_its_manifest() {
    let dir = repo(&["./.github/actions/build"]);
    local_action(
        dir.path(),
        ".github/actions/build",
        &["softprops/action-gh-release@v2"],
    );
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .code(FAILED)
        .stdout(predicates::str::contains("not pinned"));
}

/// A local action may live anywhere in the repository, so following references is
/// the only way to reach one outside the directories that are walked.
#[test]
fn a_local_action_outside_the_workflow_directories_is_still_followed() {
    let dir = repo(&["./ci/shared"]);
    local_action(dir.path(), "ci/shared", &["softprops/action-gh-release@v2"]);
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .code(FAILED)
        .stdout(predicates::str::contains("ci/shared/action.yml"));
}

#[test]
fn a_local_action_that_is_not_there_fails() {
    let dir = repo(&["./.github/actions/missing"]);
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .code(FAILED)
        .stdout(predicates::str::contains("nothing at that path"));
}

#[test]
fn a_local_path_leaving_the_repository_fails() {
    let dir = repo(&["../outside/action"]);
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .code(FAILED)
        .stdout(predicates::str::contains("leaves the repository"));
}

/// Two local actions referring to each other must not walk forever.
#[test]
fn a_cycle_between_local_actions_terminates() {
    let dir = repo(&["./ci/a"]);
    local_action(dir.path(), "ci/a", &["./ci/b"]);
    local_action(dir.path(), "ci/b", &["./ci/a"]);
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .success();
}

#[test]
fn check_writes_nothing() {
    let dir = repo(&["actions/checkout@v4"]);
    let before = workflow(dir.path());
    hashpinner(dir.path())
        .args(["--check", "--no-allow"])
        .assert()
        .code(FAILED);
    assert_eq!(workflow(dir.path()), before);
}

#[test]
fn json_output_is_machine_readable() {
    let dir = repo(&["softprops/action-gh-release@v2"]);
    let out = hashpinner(dir.path())
        .args(["--check", "--format", "json"])
        .assert()
        .code(FAILED);
    let text = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid json");

    assert_eq!(doc["failed"], true);
    assert_eq!(doc["files"][0]["entries"][0]["ref"], "v2");
    assert_eq!(doc["files"][0]["entries"][0]["level"], "fail");
}

#[test]
fn quiet_hides_passing_entries_but_keeps_the_exit_code() {
    let dir = repo(&[PINNED]);
    hashpinner(dir.path())
        .args(["--check", "--quiet"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn a_missing_path_is_a_tool_error_not_a_failure() {
    let dir = repo(&[PINNED]);
    hashpinner(dir.path())
        .args(["--check", "no/such/file.yml"])
        .assert()
        .code(TOOL_ERROR);
}

#[test]
fn a_directory_with_no_workflows_is_a_tool_error() {
    let dir = TempDir::new().expect("tempdir");
    hashpinner(dir.path())
        .arg("--check")
        .assert()
        .code(TOOL_ERROR);
}

#[test]
fn malformed_yaml_is_reported_without_stopping_the_run() {
    let dir = repo(&[PINNED]);
    std::fs::write(
        dir.path().join(".github/workflows/broken.yml"),
        "jobs:\n  - [unclosed\n",
    )
    .expect("write");

    // broken.yml sorts first, so this also proves the run continued past it.
    hashpinner(dir.path())
        .arg("--check")
        .assert()
        .stderr(predicates::str::contains("broken.yml"))
        .stdout(predicates::str::contains("actions/checkout"));
}

#[test]
fn mutually_exclusive_modes_are_rejected_by_clap() {
    let dir = repo(&[PINNED]);
    hashpinner(dir.path())
        .args(["--list", "--check"])
        .assert()
        .code(TOOL_ERROR);
    hashpinner(dir.path())
        .args(["--check", "--pin"])
        .assert()
        .code(TOOL_ERROR);
}

#[test]
fn dry_run_reports_without_writing() {
    // An unpinned reference the offline resolver cannot resolve still exercises the
    // "would write" path guard; nothing may reach disk either way.
    let dir = repo(&["actions/checkout@v4"]);
    let before = workflow(dir.path());
    hashpinner(dir.path())
        .args(["--pin", "--dry-run", "--offline"])
        .assert()
        .code(predicates::ord::ne(TOOL_ERROR));
    assert_eq!(workflow(dir.path()), before);
}

#[test]
fn both_workflow_directories_present_warns() {
    let dir = repo(&[PINNED]);
    let forgejo = dir.path().join(".forgejo/workflows");
    std::fs::create_dir_all(&forgejo).expect("mkdir");
    std::fs::write(forgejo.join("ci.yml"), "on: push\n").expect("write");

    hashpinner(dir.path())
        .arg("--check")
        .assert()
        .stderr(predicates::str::contains("Forgejo reads only"));
}
