use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::harness::{GitHubFixture, Scenario, diff};

pub const PUSH_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
pub const HEAD_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

pub fn var(key: &'static str, value: &str) -> (&'static str, String) {
    (key, value.to_string())
}

fn tokens() -> Vec<(&'static str, String)> {
    vec![
        var("INPUT_GITHUB_TOKEN", "gh-secret"),
        var("INPUT_VERCEL_TOKEN", "vercel-secret"),
        var("INPUT_VERCEL_ORG_ID", "team_org"),
        var("INPUT_VERCEL_PROJECT_ID", "prj_1"),
    ]
}

pub fn push(name: &'static str, git_ref: &str, extra: Vec<(&'static str, String)>) -> Scenario {
    let mut env = tokens();
    env.extend([
        var("GITHUB_REF", git_ref),
        var("GITHUB_SHA", PUSH_SHA),
        var("GITHUB_ACTOR", "octocat"),
    ]);
    env.extend(extra);
    Scenario {
        name,
        event_name: "push",
        payload: json!({}),
        env,
        github: GitHubFixture::default(),
        vercel_fail: false,
    }
}

pub fn pull_request(
    name: &'static str,
    head_ref: &str,
    head_repo: Value,
    extra: Vec<(&'static str, String)>,
) -> Scenario {
    let mut env = tokens();
    env.extend([
        var("GITHUB_REF", "refs/pull/7/merge"),
        var("GITHUB_SHA", PUSH_SHA),
        var("GITHUB_ACTOR", "contributor"),
        var("INPUT_PRODUCTION", "false"),
    ]);
    env.extend(extra);
    let payload = json!({
        "number": 7,
        "pull_request": {"user": {"login": "contributor"}, "head": {"ref": head_ref, "sha": HEAD_SHA, "repo": head_repo}}
    });
    Scenario {
        name,
        event_name: "pull_request",
        payload,
        env,
        github: GitHubFixture::default(),
        vercel_fail: false,
    }
}

pub fn same_repo() -> Value {
    json!({"full_name": "octo/repo"})
}

#[allow(dead_code)] // used from Task 15
pub fn outputs(items: &[(&str, &str)]) -> BTreeMap<String, String> {
    items
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn s01_push_production_with_aliases_build_env_scope_and_working_directory() {
    let scenario = push(
        "s01",
        "refs/heads/main",
        vec![
            var(
                "INPUT_ALIAS_DOMAINS",
                "{BRANCH}.example.com\napp.example.com\n",
            ),
            var(
                "INPUT_BUILD_ENV",
                "NEXT_PUBLIC_STAGE_NAME=dev\nNEXT_PUBLIC_API_URL=https://dev.api.example.com\n",
            ),
            var("INPUT_VERCEL_SCOPE", "truckup-591e6e55"),
            var("INPUT_WORKING_DIRECTORY", "app"),
        ],
    );
    let rust = diff(&scenario, |_| {}).await;
    assert_eq!(rust.trace.exit_code, Some(0));
    assert_eq!(
        rust.trace.aliases,
        BTreeSet::from([
            "app.example.com".to_string(),
            "main.example.com".to_string()
        ])
    );
    assert_eq!(rust.trace.deploy_env[0].cwd, "app");
    assert_eq!(
        rust.trace.outputs["PREVIEW_URL"],
        "https://main.example.com"
    );
    assert_eq!(
        rust.trace.outputs["DEPLOYMENT_URLS"],
        r#"["https://main.example.com","https://app.example.com","https://proj-abc123.vercel.app"]"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn s02_push_hotfix_non_production_single_alias() {
    let scenario = push(
        "s02",
        "refs/heads/hotfix",
        vec![
            var("INPUT_PRODUCTION", "false"),
            var("INPUT_GITHUB_DEPLOYMENT_ENV", "hotfix"),
            var("INPUT_ALIAS_DOMAINS", "hotfix.app.example.com\n"),
        ],
    );
    let rust = diff(&scenario, |_| {}).await;
    assert!(!rust.trace.deploys[0].contains(&"--prod".to_string()));
    assert_eq!(
        rust.trace.github["POST /repos/octo/repo/deployments"][0]["environment"],
        "hotfix"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn s03_pr_preview_domain_labels_disabled_empty_alias_domains() {
    let scenario = pull_request(
        "s03",
        "feature/login-form",
        same_repo(),
        vec![
            var("INPUT_PR_LABELS", "false"),
            var("INPUT_ALIAS_DOMAINS", "\n"),
            var("INPUT_PR_PREVIEW_DOMAIN", "pr{PR}.app.example.com"),
            var("INPUT_GITHUB_DEPLOYMENT_ENV", "pr7"),
            var("INPUT_VERCEL_SCOPE", "truckup-591e6e55"),
        ],
    );
    let rust = diff(&scenario, |_| {}).await;
    assert_eq!(
        rust.trace.aliases,
        BTreeSet::from(["pr7.app.example.com".to_string()])
    );
    assert!(
        !rust
            .trace
            .github
            .contains_key("POST /repos/octo/repo/issues/7/labels")
    );
    assert_eq!(
        rust.trace.github["POST /repos/octo/repo/issues/7/comments"].len(),
        1
    );
    assert_eq!(rust.trace.outputs["COMMENT_CREATED"], "true");
}

#[tokio::test(flavor = "multi_thread")]
async fn s12_no_github_deployment_no_metadata() {
    let scenario = push(
        "s12",
        "refs/heads/main",
        vec![
            var("INPUT_GITHUB_DEPLOYMENT", "false"),
            var("INPUT_ATTACH_COMMIT_METADATA", "false"),
        ],
    );
    let rust = diff(&scenario, |_| {}).await;
    assert!(rust.trace.github.is_empty());
    assert_eq!(rust.trace.deploys[0], ["--token=vercel-secret", "--prod"]);
}
