use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::harness::{GitHubFixture, Impl, STATUSES, Scenario, diff, run};

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

#[tokio::test(flavor = "multi_thread")]
async fn s04_pr_existing_comment_on_page_2() {
    let mut scenario = pull_request("s04", "feature/login-form", same_repo(), vec![]);
    scenario.github.comment_on_page_2 = true;
    let rust = diff(&scenario, |expected| {
        // F2: v2 follows the Link header to page 2 and deletes the old deploy comment there.
        expected
            .github
            .get_mut("GET /repos/octo/repo/issues/7/comments")
            .unwrap()
            .push(Value::Null);
        expected.github.insert(
            "DELETE /repos/octo/repo/issues/comments/55".into(),
            vec![Value::Null],
        );
    })
    .await;
    assert_eq!(
        rust.trace.github["POST /repos/octo/repo/issues/7/labels"][0],
        json!({"labels": ["deployed"]})
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn s05_long_preview_alias_is_truncated() {
    let scenario = pull_request(
        "s05",
        "feature/this-is-a-very-long-branch-name-that-keeps-going-and-going",
        same_repo(),
        vec![var("INPUT_PR_PREVIEW_DOMAIN", "{REPO}-{BRANCH}.vercel.app")],
    );
    let rust = diff(&scenario, |_| {}).await;
    let alias = rust.trace.aliases.iter().next().unwrap();
    assert_eq!(alias.len(), 55 + 1 + 6 + ".vercel.app".len(), "{alias}");
    assert!(rust.stdout.contains("::warning::The alias"));
}

#[tokio::test(flavor = "multi_thread")]
async fn s06_fork_pr_is_refused() {
    let scenario = pull_request(
        "s06",
        "feature/login-form",
        json!({"full_name": "someone/repo"}),
        vec![],
    );
    let rust = diff(&scenario, |_| {}).await;
    assert_eq!(rust.trace.exit_code, Some(0));
    assert!(rust.trace.deploys.is_empty());
    assert_eq!(
        rust.trace.outputs,
        outputs(&[("COMMENT_CREATED", "true"), ("DEPLOYMENT_CREATED", "false")])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn s07_running_local_stack_prod() {
    let mut scenario = push(
        "s07",
        "refs/heads/main",
        vec![
            var("RUNNING_LOCAL", "true"),
            var("SHA", HEAD_SHA),
            var("REF", "main"),
            var("PRODUCTION", "true"),
            var("INPUT_PRODUCTION", "true"),
            var("INPUT_GITHUB_DEPLOYMENT_ENV", "prod"),
            var("INPUT_ALIAS_DOMAINS", "ops.example.com\napp.example.com\n"),
            var("INPUT_VERCEL_SCOPE", "truckup-591e6e55"),
        ],
    );
    scenario.event_name = "deployment_status";
    let rust = diff(&scenario, |_| {}).await;
    let deployment = &rust.trace.github["POST /repos/octo/repo/deployments"][0];
    assert_eq!(
        (
            deployment["ref"].as_str(),
            deployment["environment"].as_str()
        ),
        (Some("main"), Some("prod"))
    );
    assert_eq!(
        rust.trace.github[STATUSES][0]["log_url"],
        "https://github.com/octo/repo"
    );
    assert!(rust.trace.deploys[0].contains(&"--prod".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn s08_commit_author_without_github_account() {
    let mut scenario = push("s08", "refs/heads/main", vec![]);
    scenario.github.author_null = true;
    let legacy = run(Impl::Legacy, &scenario).await;
    assert_eq!(
        legacy.trace.exit_code,
        Some(1),
        "v1 crashes on a null commit author"
    );
    assert!(legacy.trace.deploys.is_empty());
    // F1: v2 deploys and sends an empty login.
    let rust = run(Impl::Rust, &scenario).await;
    assert_eq!(rust.trace.exit_code, Some(0), "{}", rust.stdout);
    assert!(rust.trace.deploys[0].contains(&"githubCommitAuthorLogin=".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn s09_vercel_cli_failure() {
    let mut scenario = push("s09", "refs/heads/main", vec![]);
    scenario.vercel_fail = true;
    let rust = diff(&scenario, |_| {}).await;
    assert_eq!(rust.trace.exit_code, Some(1));
    let states: Vec<&str> = rust.trace.github[STATUSES]
        .iter()
        .map(|s| s["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["pending", "failure"]);
    assert!(rust.trace.outputs.is_empty());
    assert!(
        rust.stdout
            .contains("::error::Error: Command \"npm run build\" exited with 1")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn s10_comment_creation_fails_after_deploy() {
    let mut scenario = pull_request("s10", "feature/login-form", same_repo(), vec![]);
    scenario.github.fail_create_comment = true;
    let rust = diff(&scenario, |expected| {
        // F14: post-deploy failures keep the success status, labels still run, outputs are already written.
        expected
            .github
            .get_mut(STATUSES)
            .unwrap()
            .retain(|status| status["state"] != "failure");
        expected.github.insert(
            "POST /repos/octo/repo/issues/7/labels".into(),
            vec![json!({"labels": ["deployed"]})],
        );
        expected.outputs = outputs(&[
            ("PREVIEW_URL", "https://proj-abc123.vercel.app"),
            ("DEPLOYMENT_URLS", r#"["https://proj-abc123.vercel.app"]"#),
            ("DEPLOYMENT_UNIQUE_URL", "https://proj-abc123.vercel.app"),
            ("DEPLOYMENT_ID", "dpl_1"),
            (
                "DEPLOYMENT_INSPECTOR_URL",
                "https://vercel.com/octo/repo/dpl1",
            ),
            ("DEPLOYMENT_CREATED", "true"),
            ("COMMENT_CREATED", "true"),
        ]);
    })
    .await;
    assert_eq!(rust.trace.exit_code, Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn s11_tag_push_keeps_branch_first_character() {
    let scenario = push(
        "s11",
        "refs/tags/v1.2.3",
        vec![var("INPUT_ALIAS_DOMAINS", "{BRANCH}.example.com")],
    );
    let rust = diff(&scenario, |expected| {
        // F4: v1 computed BRANCH as ref.substr(11), dropping the "v" of tag refs.
        expected.aliases = BTreeSet::from(["v1-2-3.example.com".to_string()]);
        expected
            .outputs
            .insert("PREVIEW_URL".into(), "https://v1-2-3.example.com".into());
        expected.outputs.insert(
            "DEPLOYMENT_URLS".into(),
            r#"["https://v1-2-3.example.com","https://proj-abc123.vercel.app"]"#.into(),
        );
        for status in expected.github.get_mut(STATUSES).unwrap() {
            if status["state"] == "success" {
                status["environment_url"] = json!("https://v1-2-3.example.com");
            }
        }
    })
    .await;
    assert!(rust.trace.deploys[0].contains(&"githubCommitRef=v1.2.3".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn s13_invalid_boolean_input() {
    let scenario = push(
        "s13",
        "refs/heads/main",
        vec![var("INPUT_PRODUCTION", "yes")],
    );
    let rust = diff(&scenario, |_| {}).await;
    assert_eq!(rust.trace.exit_code, Some(1));
    assert!(rust.stdout.contains(
        "::error::boolean input has to be one of `true | True | TRUE | false | False | FALSE`"
    ));
}
