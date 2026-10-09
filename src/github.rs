//! GitHub REST client for the seven calls the action makes (spec §5.4).

use reqwest::header::HeaderMap;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde_json::{Value, json};

use crate::comment::DEPLOYED_MARKER;
use crate::error::Result;
use crate::http::{self, RetryPolicy, encode_segment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentState {
    Pending,
    Success,
    Failure,
}

impl DeploymentState {
    pub fn as_str(self) -> &'static str {
        match self {
            DeploymentState::Pending => "pending",
            DeploymentState::Success => "success",
            DeploymentState::Failure => "failure",
        }
    }

    /// F8: v1 always sent "Starting deployment to Vercel".
    pub fn description(self) -> &'static str {
        match self {
            DeploymentState::Pending => "Deploying to Vercel",
            DeploymentState::Success => "Deployed to Vercel",
            DeploymentState::Failure => "Deployment to Vercel failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub author_name: String,
    /// `None` when the commit author has no GitHub account (F1).
    pub author_login: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedComment {
    pub id: u64,
    pub html_url: String,
}

#[allow(async_fn_in_trait)]
pub trait GitHubApi {
    async fn create_deployment(&self, git_ref: &str, environment: &str) -> Result<Option<u64>>;
    async fn create_deployment_status(
        &self,
        deployment_id: u64,
        state: DeploymentState,
        environment_url: &str,
    ) -> Result<()>;
    async fn get_commit(&self, git_ref: &str) -> Result<CommitInfo>;
    async fn delete_existing_comment(&self, pr_number: &str) -> Result<Option<u64>>;
    async fn create_comment(&self, pr_number: &str, body: &str) -> Result<CreatedComment>;
    async fn add_labels(&self, pr_number: &str, labels: &[String]) -> Result<Vec<String>>;
}

pub struct GitHubClient {
    http: Client,
    policy: RetryPolicy,
    base_url: String,
    token: String,
    owner: String,
    repo: String,
    log_url: String,
}

impl GitHubClient {
    pub fn new(
        http: Client,
        base_url: &str,
        token: &str,
        owner: &str,
        repo: &str,
        log_url: &str,
    ) -> Self {
        Self {
            http,
            policy: RetryPolicy::default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            log_url: log_url.to_string(),
        }
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    fn repo_url(&self, segments: &[&str]) -> String {
        let mut url = format!(
            "{}/repos/{}/{}",
            self.base_url,
            encode_segment(&self.owner),
            encode_segment(&self.repo)
        );
        for segment in segments {
            url.push('/');
            url.push_str(&encode_segment(segment));
        }
        url
    }

    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.http
            .request(method, url)
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn call(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Response> {
        let response = http::send(self.policy, &method, || {
            let request = self.request(method.clone(), url);
            match body {
                Some(body) => request.json(body),
                None => request,
            }
        })
        .await?;
        http::ensure_success(response, &method).await
    }
}

impl GitHubApi for GitHubClient {
    async fn create_deployment(&self, git_ref: &str, environment: &str) -> Result<Option<u64>> {
        let body = json!({
            "ref": git_ref,
            "required_contexts": [],
            "environment": environment,
            "description": "Deploy to Vercel",
            "auto_merge": false,
        });
        let response: Value = self
            .call(Method::POST, &self.repo_url(&["deployments"]), Some(&body))
            .await?
            .json()
            .await?;
        Ok(response.get("id").and_then(Value::as_u64))
    }

    async fn create_deployment_status(
        &self,
        deployment_id: u64,
        state: DeploymentState,
        environment_url: &str,
    ) -> Result<()> {
        let body = json!({
            "state": state.as_str(),
            "log_url": self.log_url,
            "environment_url": environment_url,
            "description": state.description(),
        });
        let url = self.repo_url(&["deployments", &deployment_id.to_string(), "statuses"]);
        self.call(Method::POST, &url, Some(&body)).await?;
        Ok(())
    }

    async fn get_commit(&self, git_ref: &str) -> Result<CommitInfo> {
        let commit: Value = self
            .call(Method::GET, &self.repo_url(&["commits", git_ref]), None)
            .await?
            .json()
            .await?;
        let text = |pointer: &str| {
            commit
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        Ok(CommitInfo {
            author_name: text("/commit/author/name").unwrap_or_default(),
            author_login: text("/author/login"),
            message: text("/commit/message").unwrap_or_default(),
        })
    }

    async fn delete_existing_comment(&self, pr_number: &str) -> Result<Option<u64>> {
        // F2: v1 only read the first 30 comments.
        let mut next = Some(format!(
            "{}?per_page=100",
            self.repo_url(&["issues", pr_number, "comments"])
        ));
        while let Some(url) = next {
            let response = self.call(Method::GET, &url, None).await?;
            next = next_link(response.headers());
            let comments: Vec<Value> = response.json().await?;
            let found = comments
                .iter()
                .find(|c| {
                    c.get("body")
                        .and_then(Value::as_str)
                        .is_some_and(|b| b.contains(DEPLOYED_MARKER))
                })
                .and_then(|c| c.get("id").and_then(Value::as_u64));
            if let Some(id) = found {
                let delete_url = self.repo_url(&["issues", "comments", &id.to_string()]);
                self.call(Method::DELETE, &delete_url, None).await?;
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    async fn create_comment(&self, pr_number: &str, body: &str) -> Result<CreatedComment> {
        let url = self.repo_url(&["issues", pr_number, "comments"]);
        let created: Value = self
            .call(Method::POST, &url, Some(&json!({ "body": body })))
            .await?
            .json()
            .await?;
        Ok(CreatedComment {
            id: created
                .get("id")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            html_url: created
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    async fn add_labels(&self, pr_number: &str, labels: &[String]) -> Result<Vec<String>> {
        let url = self.repo_url(&["issues", pr_number, "labels"]);
        let added: Vec<Value> = self
            .call(Method::POST, &url, Some(&json!({ "labels": labels })))
            .await?
            .json()
            .await?;
        Ok(added
            .iter()
            .filter_map(|label| {
                label
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect())
    }
}

fn next_link(headers: &HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let mut pieces = part.split(';');
        let url = pieces.next()?.trim().strip_prefix('<')?.strip_suffix('>')?;
        pieces
            .any(|p| p.trim() == r#"rel="next""#)
            .then(|| url.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{TEST_RETRY_POLICY, build_client};
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const LOG_URL: &str = "https://github.com/octo/repo/actions/runs/99";

    fn client(server: &MockServer) -> GitHubClient {
        GitHubClient::new(
            build_client("test"),
            &server.uri(),
            "gh-token",
            "octo",
            "repo",
            LOG_URL,
        )
        .with_retry_policy(TEST_RETRY_POLICY)
    }

    #[tokio::test]
    async fn create_deployment_sends_v1_body_and_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/octo/repo/deployments"))
            .and(header("authorization", "Bearer gh-token"))
            .and(header("x-github-api-version", "2022-11-28"))
            .and(body_json(json!({
                "ref": "refs/heads/main", "required_contexts": [], "environment": "Production",
                "description": "Deploy to Vercel", "auto_merge": false
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 42})))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            client(&server)
                .create_deployment("refs/heads/main", "Production")
                .await
                .unwrap(),
            Some(42)
        );
    }

    #[tokio::test]
    async fn create_deployment_without_id_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/octo/repo/deployments"))
            .respond_with(
                ResponseTemplate::new(202).set_body_json(json!({"message": "Auto-merged"})),
            )
            .mount(&server)
            .await;
        assert_eq!(
            client(&server)
                .create_deployment("main", "Preview")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn create_deployment_403_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/octo/repo/deployments"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"message":"Resource not accessible by integration"}"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        let err = client(&server)
            .create_deployment("main", "Preview")
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("403") && message.contains("Resource not accessible by integration"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn deployment_status_uses_state_description() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/repos/octo/repo/deployments/42/statuses"))
            .and(body_json(json!({
                "state": "success", "log_url": LOG_URL,
                "environment_url": "https://pr7.example.com", "description": "Deployed to Vercel"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
            .expect(1)
            .mount(&server)
            .await;
        client(&server)
            .create_deployment_status(42, DeploymentState::Success, "https://pr7.example.com")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn get_commit_encodes_ref_and_tolerates_null_author() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/commits/refs%2Fheads%2Fmain"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "commit": {"author": {"name": "Jane Doe"}, "message": "feat: x\n\nbody"}, "author": null
            })))
            .mount(&server).await;
        let commit = client(&server).get_commit("refs/heads/main").await.unwrap();
        assert_eq!(
            commit,
            CommitInfo {
                author_name: "Jane Doe".into(),
                author_login: None,
                message: "feat: x\n\nbody".into()
            }
        );
    }

    #[tokio::test]
    async fn delete_existing_comment_follows_pagination() {
        let server = MockServer::start().await;
        let next = format!(
            "<{}/repos/octo/repo/issues/7/comments?per_page=100&page=2>; rel=\"next\"",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/issues/7/comments"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 55, "body": "\nThis pull request has been deployed to Vercel.\n"}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/issues/7/comments"))
            .and(query_param("per_page", "100"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"id": 54, "body": "LGTM"}, {"id": 53, "body": null}]))
                    .insert_header("link", next.as_str()),
            )
            .with_priority(2)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/repos/octo/repo/issues/comments/55"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            client(&server).delete_existing_comment("7").await.unwrap(),
            Some(55)
        );
    }

    #[tokio::test]
    async fn delete_existing_comment_without_match_deletes_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/repo/issues/7/comments"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"id": 54, "body": "LGTM"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        assert_eq!(
            client(&server).delete_existing_comment("7").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn create_comment_and_labels() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/repos/octo/repo/issues/7/comments")).and(body_json(json!({"body": "hi"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 99, "html_url": "https://github.com/octo/repo/pull/7#issuecomment-99"})))
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/repos/octo/repo/issues/7/labels"))
            .and(body_json(json!({"labels": ["deployed", "preview"]})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!([{"name": "deployed"}, {"name": "preview"}, {"name": "old"}]),
                ),
            )
            .mount(&server)
            .await;
        let github = client(&server);
        let comment = github.create_comment("7", "hi").await.unwrap();
        assert_eq!(
            comment,
            CreatedComment {
                id: 99,
                html_url: "https://github.com/octo/repo/pull/7#issuecomment-99".into()
            }
        );
        let labels = github
            .add_labels("7", &["deployed".into(), "preview".into()])
            .await
            .unwrap();
        assert_eq!(labels, ["deployed", "preview", "old"]);
    }

    #[test]
    fn next_link_parses_rel_next() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "link",
            r#"<https://x/a?page=2>; rel="next", <https://x/a?page=9>; rel="last""#
                .parse()
                .unwrap(),
        );
        assert_eq!(next_link(&headers).as_deref(), Some("https://x/a?page=2"));
        headers.insert(
            "link",
            r#"<https://x/a?page=1>; rel="prev""#.parse().unwrap(),
        );
        assert_eq!(next_link(&headers), None);
    }
}
