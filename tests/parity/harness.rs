use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use deploy_to_vercel::actions_io::parse_file_commands;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub const STATUSES: &str = "POST /repos/octo/repo/deployments/42/statuses";
const REPO: &str = "/repos/octo/repo";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impl {
    Legacy,
    Rust,
}

#[derive(Clone, Debug, Default)]
pub struct GitHubFixture {
    pub author_null: bool,
    pub comment_on_page_2: bool,
    pub fail_create_comment: bool,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub event_name: &'static str,
    pub payload: Value,
    pub env: Vec<(&'static str, String)>,
    pub github: GitHubFixture,
    pub vercel_fail: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeployEnv {
    pub cwd: String,
    pub org_id: String,
    pub project_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trace {
    pub exit_code: Option<i32>,
    pub deploys: Vec<Vec<String>>,
    pub deploy_env: Vec<DeployEnv>,
    pub aliases: BTreeSet<String>,
    pub lookups: Vec<String>,
    pub github: BTreeMap<String, Vec<Value>>,
    pub outputs: BTreeMap<String, String>,
    pub exported: BTreeMap<String, String>,
}

pub struct RawRun {
    pub trace: Trace,
    pub github_order: Vec<String>,
    pub stdout: String,
    /// (request line, `authorization` header) per GitHub request.
    pub github_auth: Vec<(String, Option<String>)>,
    /// (request line, `authorization` header, query pairs) per Vercel REST request.
    pub vercel_requests: Vec<VercelRequest>,
}

pub struct VercelRequest {
    pub line: String,
    pub authorization: Option<String>,
    pub query: Vec<(String, String)>,
}

fn authorization(request: &Request) -> Option<String> {
    request
        .headers
        .get("authorization")
        .map(|v| v.to_str().unwrap_or_default().to_string())
}

#[derive(Default)]
struct Invocation {
    cwd: String,
    org_id: String,
    project_id: String,
    args: Vec<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(binary))
        .find(|p| p.is_file())
}

fn node() -> PathBuf {
    find_on_path("node").expect("parity tests need `node` (20+) on PATH")
}

fn child_path() -> String {
    let dirs = [
        repo_root().join("tests/parity/bin"),
        node().parent().unwrap().to_path_buf(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];
    std::env::join_paths(dirs).unwrap().into_string().unwrap()
}

fn legacy_script(dir: &Path, vercel_uri: &str) -> PathBuf {
    let source = std::fs::read_to_string(repo_root().join("tests/parity/legacy/index.js")).unwrap();
    assert!(
        source.contains("https://api.vercel.com/v11/now/deployments/get"),
        "unexpected legacy bundle"
    );
    let script = dir.join("legacy.js");
    std::fs::write(
        &script,
        source.replace("https://api.vercel.com", vercel_uri),
    )
    .unwrap();
    script
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap_or(Value::Null)
}

async fn mount_github(server: &MockServer, fixture: &GitHubFixture) {
    let ok = |status: u16, body: Value| ResponseTemplate::new(status).set_body_json(body);
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/deployments")))
        .respond_with(ok(201, json!({"id": 42})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/deployments/42/statuses")))
        .respond_with(ok(201, json!({"id": 1})))
        .mount(server)
        .await;
    let author = if fixture.author_null {
        Value::Null
    } else {
        json!({"login": "jane"})
    };
    Mock::given(method("GET")).and(path_regex(format!("^{REPO}/commits/.+$")))
        .respond_with(ok(200, json!({
            "commit": {"author": {"name": "Jane Doe"}, "message": "feat: add thing\n\nlonger body"},
            "author": author
        })))
        .mount(server).await;

    let marker = json!({"id": 55, "body": "\nThis pull request has been deployed to Vercel.\n"});
    let other = json!({"id": 54, "body": "LGTM"});
    let comments = format!("{REPO}/issues/7/comments");
    if fixture.comment_on_page_2 {
        Mock::given(method("GET"))
            .and(path(comments.as_str()))
            .and(query_param("page", "2"))
            .respond_with(ok(200, json!([marker])))
            .with_priority(1)
            .mount(server)
            .await;
        let link = format!(
            "<{}{comments}?per_page=100&page=2>; rel=\"next\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path(comments.as_str()))
            .respond_with(ok(200, json!([other])).insert_header("link", link.as_str()))
            .with_priority(2)
            .mount(server)
            .await;
    } else {
        Mock::given(method("GET"))
            .and(path(comments.as_str()))
            .respond_with(ok(200, json!([other, marker])))
            .mount(server)
            .await;
    }
    Mock::given(method("DELETE"))
        .and(path(format!("{REPO}/issues/comments/55")))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
    let comment_status = if fixture.fail_create_comment {
        500
    } else {
        201
    };
    Mock::given(method("POST"))
        .and(path(comments.as_str()))
        .respond_with(ok(
            comment_status,
            json!({"id": 99, "html_url": "https://github.com/octo/repo/pull/7#issuecomment-99"}),
        ))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{REPO}/issues/7/labels")))
        .respond_with(ok(200, json!([{"name": "deployed"}])))
        .mount(server)
        .await;
}

async fn mount_vercel(server: &MockServer) {
    let deployment = json!({"id": "dpl_1", "url": "proj-abc123.vercel.app", "inspectorUrl": "https://vercel.com/octo/repo/dpl1"});
    Mock::given(method("GET"))
        .and(path("/v11/now/deployments/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(deployment.clone()))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/v13/deployments/.+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(deployment))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("^/v2/deployments/.+/aliases$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"uid": "a1", "alias": "x", "created": "2026-10-09T00:00:00Z"}),
            ),
        )
        .mount(server)
        .await;
}

fn parse_vercel_log(log: &Path, root: &Path) -> Vec<Invocation> {
    let Ok(content) = std::fs::read_to_string(log) else {
        return Vec::new();
    };
    let mut invocations = Vec::new();
    let mut current = Invocation::default();
    for line in content.lines() {
        if let Some(cwd) = line.strip_prefix("cwd=") {
            current.cwd = Path::new(cwd)
                .strip_prefix(root)
                .map_or_else(|_| cwd.to_string(), |p| p.display().to_string());
        } else if let Some(value) = line.strip_prefix("VERCEL_ORG_ID=") {
            current.org_id = value.to_string();
        } else if let Some(value) = line.strip_prefix("VERCEL_PROJECT_ID=") {
            current.project_id = value.to_string();
        } else if let Some(value) = line.strip_prefix("arg=") {
            current.args.push(value.replace("\\n", "\n"));
        } else if line == "end" {
            invocations.push(std::mem::take(&mut current));
        }
    }
    invocations
}

pub async fn run(implementation: Impl, scenario: &Scenario) -> RawRun {
    let github = MockServer::start().await;
    let vercel = MockServer::start().await;
    mount_github(&github, &scenario.github).await;
    mount_vercel(&vercel).await;

    let work = tempfile::tempdir().unwrap();
    let root = work.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("app")).unwrap();
    let (output, env_file, event, log) = (
        root.join("github_output"),
        root.join("github_env"),
        root.join("event.json"),
        root.join("vercel.log"),
    );
    std::fs::write(&output, "").unwrap();
    std::fs::write(&env_file, "").unwrap();
    std::fs::write(&event, scenario.payload.to_string()).unwrap();

    let mut command = match implementation {
        Impl::Legacy => {
            let mut command = Command::new(node());
            command.arg(legacy_script(&root, &vercel.uri()));
            command
        }
        Impl::Rust => Command::new(env!("CARGO_BIN_EXE_deploy-to-vercel")),
    };
    command
        .env_clear()
        .current_dir(&root)
        .env("PATH", child_path())
        .env("HOME", &root)
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "octo/repo")
        .env("GITHUB_RUN_ID", "99")
        .env("GITHUB_API_URL", github.uri())
        .env("VERCEL_API_URL", vercel.uri())
        .env("GITHUB_EVENT_NAME", scenario.event_name)
        .env("GITHUB_EVENT_PATH", event.as_path())
        .env("GITHUB_OUTPUT", &output)
        .env("GITHUB_ENV", &env_file)
        .env("FAKE_VERCEL_LOG", &log);
    if scenario.vercel_fail {
        command.env("FAKE_VERCEL_FAIL", "1");
    }
    for (key, value) in &scenario.env {
        command.env(key, value);
    }
    let out = tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap();

    let mut trace = Trace {
        exit_code: out.status.code(),
        ..Trace::default()
    };
    for invocation in parse_vercel_log(&log, &root) {
        if invocation.args.get(1).map(String::as_str) == Some("alias") {
            trace.aliases.insert(invocation.args[4].clone());
        } else {
            trace.deploy_env.push(DeployEnv {
                cwd: invocation.cwd,
                org_id: invocation.org_id,
                project_id: invocation.project_id,
            });
            trace.deploys.push(invocation.args);
        }
    }
    let mut vercel_requests = Vec::new();
    for request in vercel.received_requests().await.unwrap_or_default() {
        vercel_requests.push(VercelRequest {
            line: format!("{} {}", request.method, request.url.path()),
            authorization: authorization(&request),
            query: request
                .url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        });
        let request_path = request.url.path().to_string();
        if request_path == "/v11/now/deployments/get" {
            let host = request
                .url
                .query_pairs()
                .find(|(k, _)| k == "url")
                .map(|(_, v)| v.into_owned());
            trace.lookups.push(host.unwrap_or_default());
        } else if let Some(host) = request_path.strip_prefix("/v13/deployments/") {
            trace.lookups.push(host.to_string());
        } else if request.method.as_str() == "POST" && request_path.ends_with("/aliases") {
            trace
                .aliases
                .insert(body(&request)["alias"].as_str().unwrap().to_string());
        }
    }
    let mut github_order = Vec::new();
    let mut github_auth = Vec::new();
    for request in github.received_requests().await.unwrap_or_default() {
        let key = format!("{} {}", request.method, request.url.path());
        github_auth.push((key.clone(), authorization(&request)));
        github_order.push(key.clone());
        trace.github.entry(key).or_default().push(body(&request));
    }
    trace.outputs = parse_file_commands(&std::fs::read_to_string(&output).unwrap())
        .into_iter()
        .collect();
    trace.exported = parse_file_commands(&std::fs::read_to_string(&env_file).unwrap())
        .into_iter()
        .collect();
    RawRun {
        trace,
        github_order,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        github_auth,
        vercel_requests,
    }
}

/// Approved fixes that apply to every scenario: F8 (status descriptions) and F11 (githubCommitRef).
fn apply_common_fixes(trace: &mut Trace) {
    if let Some(statuses) = trace.github.get_mut(STATUSES) {
        for status in statuses {
            let description = match status["state"].as_str() {
                Some("pending") => "Deploying to Vercel",
                Some("success") => "Deployed to Vercel",
                Some("failure") => "Deployment to Vercel failed",
                other => panic!("unexpected deployment state {other:?}"),
            };
            status["description"] = json!(description);
        }
    }
    for argv in &mut trace.deploys {
        for arg in argv.iter_mut() {
            if let Some(git_ref) = arg.strip_prefix("githubCommitRef=") {
                let name = git_ref
                    .strip_prefix("refs/heads/")
                    .or_else(|| git_ref.strip_prefix("refs/tags/"))
                    .unwrap_or(git_ref);
                *arg = format!("githubCommitRef={name}");
            }
        }
    }
}

fn assert_dependency_order(order: &[String]) {
    let first = |key: &str| order.iter().position(|k| k == key);
    if let Some(created) = first(&format!("POST {REPO}/deployments")) {
        let statuses: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, k)| k.as_str() == STATUSES)
            .map(|(i, _)| i)
            .collect();
        assert!(
            statuses.iter().all(|&i| i > created),
            "status before deployment: {order:?}"
        );
    }
    let deleted = first(&format!("DELETE {REPO}/issues/comments/55"));
    let created = order
        .iter()
        .rposition(|k| k == &format!("POST {REPO}/issues/7/comments"));
    if let (Some(deleted), Some(created)) = (deleted, created) {
        assert!(
            deleted < created,
            "comment created before old one deleted: {order:?}"
        );
    }
}

/// Per-request checks on the Rust run: auth headers and the Vercel team parameter (spec §9.2 allows
/// v1/v2 differences here, so these are asserted directly instead of being part of `Trace`).
fn assert_auth_and_team(scenario: &Scenario, rust: &RawRun) {
    let env = |key: &str| {
        scenario
            .env
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    };
    for (line, auth) in &rust.github_auth {
        assert_eq!(
            auth.as_deref(),
            Some("Bearer gh-secret"),
            "scenario {}: GitHub request `{line}` has wrong authorization",
            scenario.name
        );
    }
    let scope = env("INPUT_VERCEL_SCOPE");
    let expected = if scope.is_empty() {
        ("teamId", env("INPUT_VERCEL_ORG_ID"))
    } else if scope.starts_with("team_") {
        ("teamId", scope)
    } else {
        ("slug", scope)
    };
    for request in &rust.vercel_requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer vercel-secret"),
            "scenario {}: Vercel request `{}` has wrong authorization",
            scenario.name,
            request.line
        );
        let found = request.query.iter().find(|(k, _)| k == expected.0);
        assert_eq!(
            found.map(|(_, v)| v.as_str()),
            Some(expected.1),
            "scenario {}: Vercel request `{}` has wrong `{}` (query {:?})",
            scenario.name,
            request.line,
            expected.0,
            request.query
        );
    }
}

/// Runs both implementations; expects v2 == v1 + common fixes + `adjust` (the scenario's approved deltas).
pub async fn diff(scenario: &Scenario, adjust: impl FnOnce(&mut Trace)) -> RawRun {
    let legacy = run(Impl::Legacy, scenario).await;
    let rust = run(Impl::Rust, scenario).await;
    let mut expected = legacy.trace.clone();
    apply_common_fixes(&mut expected);
    adjust(&mut expected);
    assert_dependency_order(&rust.github_order);
    assert_auth_and_team(scenario, &rust);
    assert_eq!(
        rust.trace, expected,
        "scenario {}\n--- legacy stdout ---\n{}\n--- rust stdout ---\n{}",
        scenario.name, legacy.stdout, rust.stdout
    );
    rust
}
