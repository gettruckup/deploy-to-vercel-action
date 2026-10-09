//! End-to-end checks of the binary's startup contract (masking, input errors, dotenv gating).

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_deploy-to-vercel");
const REQUIRED: [(&str, &str); 5] = [
    ("INPUT_GITHUB_TOKEN", "gh-secret"),
    ("INPUT_VERCEL_TOKEN", "vercel-secret"),
    ("INPUT_VERCEL_ORG_ID", "team_org"),
    ("INPUT_VERCEL_PROJECT_ID", "prj_1"),
    ("GITHUB_REPOSITORY", "octo/repo"),
];

fn run(envs: &[(&str, &str)], cwd: &Path) -> Output {
    Command::new(BIN)
        .env_clear()
        .envs(envs.iter().copied())
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn missing_required_input_fails_with_v1_message() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        &[("GITHUB_ACTIONS", "true"), ("INPUT_GITHUB_TOKEN", "x")],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout(&out).contains("::error::Input `VERCEL_TOKEN` is required but was not provided."),
        "{}",
        stdout(&out)
    );
}

#[test]
fn invalid_boolean_fails_with_v1_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut envs = REQUIRED.to_vec();
    envs.extend([("GITHUB_ACTIONS", "true"), ("INPUT_PRODUCTION", "yes")]);
    let out = run(&envs, dir.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains(
        "::error::boolean input has to be one of `true | True | TRUE | false | False | FALSE`"
    ));
}

#[test]
fn masks_tokens_before_any_other_output() {
    let dir = tempfile::tempdir().unwrap();
    let output_file = dir.path().join("output");
    let env_file = dir.path().join("env");
    let (output_path, env_path) = (
        output_file.display().to_string(),
        env_file.display().to_string(),
    );
    let mut envs = REQUIRED.to_vec();
    envs.extend([
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_REF", "refs/heads/main"),
        ("GITHUB_SHA", "0123456789abcdef"),
        ("INPUT_GITHUB_DEPLOYMENT", "false"),
        ("INPUT_ATTACH_COMMIT_METADATA", "false"),
        ("GITHUB_OUTPUT", output_path.as_str()),
        ("GITHUB_ENV", env_path.as_str()),
        ("PATH", ""),
    ]);
    let out = run(&envs, dir.path());
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "::add-mask::gh-secret");
    assert_eq!(lines[1], "::add-mask::vercel-secret");
    assert_eq!(
        lines[2],
        format!("deploy-to-vercel-action v{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        text.matches("gh-secret").count(),
        1,
        "token leaked after masking:\n{text}"
    );
    assert_eq!(
        text.matches("vercel-secret").count(),
        2,
        "only the mask line and the masked EXEC debug line:\n{text}"
    );
    assert!(text.contains("::error::Failed to run `vercel`"), "{text}");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        std::fs::read_to_string(&env_file)
            .unwrap()
            .contains("VERCEL_ORG_ID<<ghadelimiter_")
    );
}

#[test]
fn dotenv_is_ignored_inside_github_actions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "INPUT_VERCEL_TOKEN=from-dotenv\n").unwrap();
    let out = run(
        &[("GITHUB_ACTIONS", "true"), ("INPUT_GITHUB_TOKEN", "x")],
        dir.path(),
    );
    assert!(
        stdout(&out).contains("::error::Input `VERCEL_TOKEN` is required but was not provided.")
    );
}

#[test]
fn dotenv_is_loaded_outside_github_actions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "INPUT_VERCEL_TOKEN=from-dotenv\n").unwrap();
    let out = run(&[("INPUT_GITHUB_TOKEN", "x")], dir.path());
    assert!(
        stdout(&out).contains("::error::Input `VERCEL_ORG_ID` is required but was not provided."),
        "{}",
        stdout(&out)
    );
}
