//! Run context: event-derived values in CI mode, env-derived values in RUNNING_LOCAL mode (spec §4.3).

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::inputs::{Env, Inputs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunContext {
    pub user: String,
    pub repository: String,
    pub is_pr: bool,
    pub pr_number: Option<String>,
    pub actor: String,
    pub git_ref: String,
    pub sha: String,
    pub branch: String,
    pub is_fork: bool,
    pub log_url: String,
    pub production: bool,
    pub trim_commit_message: bool,
}

impl RunContext {
    /// The ref without `refs/heads/` or `refs/tags/` (F11: sent as `githubCommitRef`).
    pub fn ref_name(&self) -> String {
        ref_name(&self.git_ref)
    }
}

pub fn ref_name(git_ref: &str) -> String {
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/tags/"))
        .unwrap_or(git_ref)
        .to_string()
}

pub fn event_is_pr(env: &dyn Env) -> bool {
    matches!(
        env.var("GITHUB_EVENT_NAME").as_deref(),
        Some("pull_request" | "pull_request_target")
    )
}

/// The event payload at `GITHUB_EVENT_PATH`, or `None` if it is unset, missing, or not JSON.
pub fn load_event_payload(env: &dyn Env) -> Option<Value> {
    let path = env.var("GITHUB_EVENT_PATH").filter(|p| !p.is_empty())?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn resolve(env: &dyn Env, inputs: &Inputs, payload: Option<&Value>) -> Result<RunContext> {
    let mut parts = inputs.github_repository.split('/');
    let user = parts.next().unwrap_or_default().to_string();
    let repository = parts.next().unwrap_or_default().to_string();
    if env.var("RUNNING_LOCAL").as_deref() == Some("true") {
        Ok(running_local(env, user, repository))
    } else {
        ci(env, inputs, payload, user, repository)
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

fn running_local(env: &dyn Env, user: String, repository: String) -> RunContext {
    let is_pr = env.var("IS_PR").as_deref() == Some("true");
    RunContext {
        sha: non_empty(env.var("SHA")).unwrap_or_else(|| "XXXXXXX".into()),
        pr_number: non_empty(env.var("PR_NUMBER")),
        git_ref: non_empty(env.var("REF")).unwrap_or_else(|| "refs/heads/master".into()),
        branch: non_empty(env.var("BRANCH")).unwrap_or_else(|| "master".into()),
        production: match env.var("PRODUCTION") {
            Some(value) => value == "true",
            None => !is_pr,
        },
        log_url: non_empty(env.var("LOG_URL"))
            .unwrap_or_else(|| format!("https://github.com/{user}/{repository}")),
        actor: non_empty(env.var("ACTOR")).unwrap_or_else(|| user.clone()),
        is_fork: env.var("IS_FORK").as_deref() == Some("true"),
        trim_commit_message: env.var("TRIM_COMMIT_MESSAGE").as_deref() == Some("true"),
        is_pr,
        user,
        repository,
    }
}

fn ci(
    env: &dyn Env,
    inputs: &Inputs,
    payload: Option<&Value>,
    user: String,
    repository: String,
) -> Result<RunContext> {
    let log_url = format!(
        "https://github.com/{user}/{repository}/actions/runs/{}",
        env.var("GITHUB_RUN_ID").unwrap_or_default()
    );
    let mut ctx = RunContext {
        user,
        repository,
        is_pr: event_is_pr(env),
        pr_number: None,
        actor: env.var("GITHUB_ACTOR").unwrap_or_default(),
        git_ref: env.var("GITHUB_REF").unwrap_or_default(),
        sha: env.var("GITHUB_SHA").unwrap_or_default(),
        branch: String::new(),
        is_fork: false,
        log_url,
        production: inputs.production,
        trim_commit_message: inputs.trim_commit_message,
    };
    if !ctx.is_pr {
        ctx.branch = ref_name(&ctx.git_ref); // F4: v1 used ref.substr(11)
        return Ok(ctx);
    }

    let payload = payload.ok_or_else(|| {
        Error::msg("GITHUB_EVENT_PATH payload is required for pull_request events")
    })?;
    let field = |pointer: &str, name: &str| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::msg(format!("pull_request event payload is missing {name}")))
    };
    ctx.actor = field("/pull_request/user/login", "pull_request.user.login")?;
    ctx.git_ref = field("/pull_request/head/ref", "pull_request.head.ref")?;
    ctx.sha = field("/pull_request/head/sha", "pull_request.head.sha")?;
    ctx.branch = ctx.git_ref.clone();
    ctx.pr_number = payload.get("number").map(|number| match number {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    ctx.is_fork = match payload
        .pointer("/pull_request/head/repo/full_name")
        .and_then(Value::as_str)
    {
        Some(full_name) => full_name != inputs.github_repository,
        None => true, // F15: deleted head repository
    };
    Ok(ctx)
}

/// Pretty JSON of inputs plus context for `::debug::`, tokens redacted.
pub fn debug_dump(inputs: &Inputs, ctx: &RunContext) -> String {
    let mut value = serde_json::to_value(inputs).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("GITHUB_TOKEN".into(), json!("***"));
        map.insert("VERCEL_TOKEN".into(), json!("***"));
        map.insert("USER".into(), json!(ctx.user));
        map.insert("REPOSITORY".into(), json!(ctx.repository));
        map.insert("IS_PR".into(), json!(ctx.is_pr));
        map.insert("PR_NUMBER".into(), json!(ctx.pr_number));
        map.insert("ACTOR".into(), json!(ctx.actor));
        map.insert("REF".into(), json!(ctx.git_ref));
        map.insert("SHA".into(), json!(ctx.sha));
        map.insert("BRANCH".into(), json!(ctx.branch));
        map.insert("IS_FORK".into(), json!(ctx.is_fork));
        map.insert("LOG_URL".into(), json!(ctx.log_url));
        map.insert("PRODUCTION".into(), json!(ctx.production));
        map.insert("TRIM_COMMIT_MESSAGE".into(), json!(ctx.trim_commit_message));
    }
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn test_push_context() -> RunContext {
    RunContext {
        user: "octo".into(),
        repository: "repo".into(),
        is_pr: false,
        pr_number: None,
        actor: "octocat".into(),
        git_ref: "refs/heads/main".into(),
        sha: "0123456789abcdef".into(),
        branch: "main".into(),
        is_fork: false,
        log_url: "https://github.com/octo/repo/actions/runs/99".into(),
        production: true,
        trim_commit_message: false,
    }
}

#[cfg(test)]
pub(crate) fn test_pr_context() -> RunContext {
    RunContext {
        user: "octo".into(),
        repository: "repo".into(),
        is_pr: true,
        pr_number: Some("7".into()),
        actor: "contributor".into(),
        git_ref: "feature/x".into(),
        sha: "abcdef0123456789".into(),
        branch: "feature/x".into(),
        is_fork: false,
        log_url: "https://github.com/octo/repo/actions/runs/99".into(),
        production: false,
        trim_commit_message: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inputs::{MapEnv, test_inputs};
    use serde_json::json;

    fn ci_env(event: &str, extra: &[(&str, &str)]) -> MapEnv {
        let mut env = MapEnv::from_pairs([
            ("GITHUB_EVENT_NAME", event),
            ("GITHUB_RUN_ID", "99"),
            ("GITHUB_ACTOR", "octocat"),
            ("GITHUB_SHA", "0123456789abcdef"),
        ]);
        for (k, v) in extra {
            env.0.insert(k.to_string(), v.to_string());
        }
        env
    }

    fn pr_payload(full_name: serde_json::Value) -> serde_json::Value {
        json!({
            "number": 7,
            "pull_request": {
                "user": { "login": "contributor" },
                "head": { "ref": "feature/x", "sha": "abcdef0123456789", "repo": { "full_name": full_name } }
            }
        })
    }

    #[test]
    fn push_event_uses_runner_env() {
        let env = ci_env("push", &[("GITHUB_REF", "refs/heads/main")]);
        let ctx = resolve(&env, &test_inputs(), None).unwrap();
        assert_eq!(ctx, test_push_context());
    }

    #[test]
    fn tag_push_keeps_first_character_of_branch() {
        let env = ci_env("push", &[("GITHUB_REF", "refs/tags/v1.2.3")]);
        let ctx = resolve(&env, &test_inputs(), None).unwrap();
        assert_eq!(ctx.branch, "v1.2.3");
        assert_eq!(ctx.ref_name(), "v1.2.3");
    }

    #[test]
    fn pull_request_uses_payload_head() {
        let mut inputs = test_inputs();
        inputs.production = false;
        let ctx = resolve(
            &ci_env("pull_request", &[]),
            &inputs,
            Some(&pr_payload(json!("octo/repo"))),
        )
        .unwrap();
        assert_eq!(ctx, test_pr_context());
    }

    #[test]
    fn pull_request_target_is_a_pr() {
        let ctx = resolve(
            &ci_env("pull_request_target", &[]),
            &test_inputs(),
            Some(&pr_payload(json!("octo/repo"))),
        )
        .unwrap();
        assert!(ctx.is_pr);
    }

    #[test]
    fn fork_detection() {
        let fork = resolve(
            &ci_env("pull_request", &[]),
            &test_inputs(),
            Some(&pr_payload(json!("someone/repo"))),
        )
        .unwrap();
        assert!(fork.is_fork);
        let mut deleted = pr_payload(json!(null));
        deleted["pull_request"]["head"]["repo"] = json!(null);
        let deleted_fork =
            resolve(&ci_env("pull_request", &[]), &test_inputs(), Some(&deleted)).unwrap();
        assert!(
            deleted_fork.is_fork,
            "F15: deleted head repo counts as a fork"
        );
    }

    #[test]
    fn pr_event_without_payload_is_an_error() {
        let err = resolve(&ci_env("pull_request", &[]), &test_inputs(), None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "GITHUB_EVENT_PATH payload is required for pull_request events"
        );
        let err = resolve(
            &ci_env("pull_request", &[]),
            &test_inputs(),
            Some(&json!({})),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "pull_request event payload is missing pull_request.user.login"
        );
    }

    #[test]
    fn running_local_defaults() {
        let env = MapEnv::from_pairs([("RUNNING_LOCAL", "true"), ("GITHUB_EVENT_NAME", "push")]);
        let ctx = resolve(&env, &test_inputs(), None).unwrap();
        assert_eq!(ctx.sha, "XXXXXXX");
        assert_eq!(ctx.git_ref, "refs/heads/master");
        assert_eq!(ctx.branch, "master");
        assert!(!ctx.is_pr);
        assert!(ctx.production);
        assert_eq!(ctx.log_url, "https://github.com/octo/repo");
        assert_eq!(ctx.actor, "octo");
        assert!(!ctx.is_fork);
    }

    #[test]
    fn running_local_stack_prod_env_overrides_inputs() {
        let env = MapEnv::from_pairs([
            ("RUNNING_LOCAL", "true"),
            ("SHA", "feedface"),
            ("REF", "main"),
            ("PRODUCTION", "false"),
            ("TRIM_COMMIT_MESSAGE", "true"),
        ]);
        let mut inputs = test_inputs();
        inputs.production = true;
        inputs.trim_commit_message = false;
        let ctx = resolve(&env, &inputs, None).unwrap();
        assert_eq!(
            (ctx.sha.as_str(), ctx.git_ref.as_str(), ctx.branch.as_str()),
            ("feedface", "main", "master")
        );
        assert!(!ctx.production, "raw env PRODUCTION wins over the input");
        assert!(
            ctx.trim_commit_message,
            "raw env TRIM_COMMIT_MESSAGE wins over the input"
        );
    }

    #[test]
    fn running_local_empty_production_is_false() {
        let env = MapEnv::from_pairs([("RUNNING_LOCAL", "true"), ("PRODUCTION", "")]);
        assert!(!resolve(&env, &test_inputs(), None).unwrap().production);
    }

    #[test]
    fn ref_name_strips_heads_and_tags() {
        assert_eq!(ref_name("refs/heads/feature/x"), "feature/x");
        assert_eq!(ref_name("refs/tags/v1"), "v1");
        assert_eq!(ref_name("main"), "main");
        assert_eq!(ref_name("refs/pull/7/merge"), "refs/pull/7/merge");
    }

    #[test]
    fn unreadable_event_payload_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("event.json");
        std::fs::write(&bad, "{not json").unwrap();
        let path = bad.display().to_string();
        assert_eq!(
            load_event_payload(&MapEnv::from_pairs([("GITHUB_EVENT_PATH", path.as_str())])),
            None
        );
        assert_eq!(
            load_event_payload(&MapEnv::from_pairs([(
                "GITHUB_EVENT_PATH",
                "/nonexistent/event.json"
            )])),
            None
        );
        let good = dir.path().join("good.json");
        std::fs::write(&good, r#"{"number": 7}"#).unwrap();
        let path = good.display().to_string();
        assert_eq!(
            load_event_payload(&MapEnv::from_pairs([("GITHUB_EVENT_PATH", path.as_str())])),
            Some(json!({"number": 7}))
        );
    }

    #[test]
    fn debug_dump_redacts_tokens() {
        let dump = debug_dump(&test_inputs(), &test_push_context());
        assert!(!dump.contains("gh-token") && !dump.contains("vercel-token"));
        assert!(dump.contains("\"GITHUB_TOKEN\": \"***\""));
        assert!(dump.contains("\"BRANCH\": \"main\""));
    }
}
