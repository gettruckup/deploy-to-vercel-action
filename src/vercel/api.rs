//! Vercel REST client: deployment lookup and alias assignment (spec §5.3).

use reqwest::{Client, Method, Response, StatusCode};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::http::{self, RetryPolicy, encode_segment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentInfo {
    pub id: String,
    pub inspector_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamParam {
    Personal,
    TeamId(String),
}

impl TeamParam {
    fn query(&self) -> String {
        match self {
            TeamParam::Personal => String::new(),
            TeamParam::TeamId(id) => format!("?teamId={}", encode_segment(id)),
        }
    }
}

/// The project owner decides the REST team context: a `team_` org id, otherwise the personal account.
/// `VERCEL_SCOPE` stays CLI-only: it may name a personal account, which Vercel's `slug` param rejects.
pub fn team_param(org_id: &str) -> TeamParam {
    if org_id.starts_with("team_") {
        TeamParam::TeamId(org_id.to_string())
    } else {
        TeamParam::Personal
    }
}

#[allow(async_fn_in_trait)]
pub trait VercelApi {
    async fn get_deployment(&self, host: &str) -> Result<DeploymentInfo>;
    async fn assign_alias(&self, deployment_id: &str, alias: &str) -> Result<()>;
}

pub struct VercelClient {
    http: Client,
    policy: RetryPolicy,
    base_url: String,
    token: String,
    team: TeamParam,
}

impl VercelClient {
    pub fn new(http: Client, base_url: &str, token: &str, team: TeamParam) -> Self {
        Self {
            http,
            policy: RetryPolicy::default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            team,
        }
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}{}", self.base_url, self.team.query())
    }

    async fn send(&self, method: Method, url: &str, body: Option<&Value>) -> Result<Response> {
        http::send(self.policy, &method, || {
            let request = self
                .http
                .request(method.clone(), url)
                .bearer_auth(&self.token);
            match body {
                Some(body) => request.json(body),
                None => request,
            }
        })
        .await
    }

    async fn alias_points_to(&self, alias: &str, deployment_id: &str) -> Result<bool> {
        let url = self.url(&format!("/v4/aliases/{}", encode_segment(alias)));
        let response = self.send(Method::GET, &url, None).await?;
        if !response.status().is_success() {
            return Ok(false);
        }
        let found: Value = response.json().await?;
        Ok(found.get("deploymentId").and_then(Value::as_str) == Some(deployment_id))
    }
}

impl VercelApi for VercelClient {
    async fn get_deployment(&self, host: &str) -> Result<DeploymentInfo> {
        let url = self.url(&format!("/v13/deployments/{}", encode_segment(host)));
        let response =
            http::ensure_success(self.send(Method::GET, &url, None).await?, &Method::GET).await?;
        let deployment: Value = response.json().await?;
        let id = deployment
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::msg(format!("Vercel returned no deployment id for {host}")))?;
        Ok(DeploymentInfo {
            id: id.to_string(),
            inspector_url: deployment
                .get("inspectorUrl")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    async fn assign_alias(&self, deployment_id: &str, alias: &str) -> Result<()> {
        let url = self.url(&format!(
            "/v2/deployments/{}/aliases",
            encode_segment(deployment_id)
        ));
        let response = self
            .send(Method::POST, &url, Some(&json!({ "alias": alias })))
            .await?;
        if response.status() != StatusCode::CONFLICT {
            http::ensure_success(response, &Method::POST).await?;
            return Ok(());
        }
        let conflict = http::status_error(response, &Method::POST).await;
        if self.alias_points_to(alias, deployment_id).await? {
            Ok(())
        } else {
            Err(conflict)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{TEST_RETRY_POLICY, build_client};
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer, team: TeamParam) -> VercelClient {
        VercelClient::new(build_client("test"), &server.uri(), "vercel-token", team)
            .with_retry_policy(TEST_RETRY_POLICY)
    }

    #[test]
    fn team_param_comes_from_org_id_only() {
        assert_eq!(team_param("team_abc"), TeamParam::TeamId("team_abc".into()));
        assert_eq!(team_param("QmUserId"), TeamParam::Personal);
        assert_eq!(team_param(""), TeamParam::Personal);
    }

    #[tokio::test]
    async fn personal_account_requests_carry_no_team_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v13/deployments/proj-abc.vercel.app"))
            .and(header("authorization", "Bearer vercel-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({"id": "dpl_1", "inspectorUrl": "https://vercel.com/i/1"}),
                ),
            )
            .mount(&server)
            .await;
        let info = client(&server, team_param("QmUserId"))
            .get_deployment("proj-abc.vercel.app")
            .await
            .unwrap();
        assert_eq!(
            info,
            DeploymentInfo {
                id: "dpl_1".into(),
                inspector_url: "https://vercel.com/i/1".into()
            }
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[0].url.query(), None);
    }

    #[tokio::test]
    async fn null_inspector_url_becomes_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v13/deployments/h.vercel.app"))
            .and(query_param("teamId", "team_1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "dpl_2", "inspectorUrl": null})),
            )
            .mount(&server)
            .await;
        let info = client(&server, TeamParam::TeamId("team_1".into()))
            .get_deployment("h.vercel.app")
            .await
            .unwrap();
        assert_eq!(info.inspector_url, "");
    }

    #[tokio::test]
    async fn assign_alias_posts_alias() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/deployments/dpl_1/aliases"))
            .and(body_json(json!({"alias": "a.example.com"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"uid": "x", "alias": "a.example.com", "created": "2026-10-09T00:00:00Z"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        client(&server, TeamParam::Personal)
            .assign_alias("dpl_1", "a.example.com")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn conflict_on_same_deployment_is_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/deployments/dpl_1/aliases"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_string(r#"{"error":{"message":"already assigned"}}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/aliases/a.example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"deploymentId": "dpl_1"})),
            )
            .mount(&server)
            .await;
        client(&server, TeamParam::Personal)
            .assign_alias("dpl_1", "a.example.com")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn conflict_on_other_deployment_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/deployments/dpl_1/aliases"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_string(r#"{"error":{"message":"domain not allowed"}}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/aliases/a.example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"deploymentId": "dpl_other"})),
            )
            .mount(&server)
            .await;
        let err = client(&server, TeamParam::Personal)
            .assign_alias("dpl_1", "a.example.com")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("409") && err.to_string().contains("domain not allowed"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn alias_404_surfaces_vercel_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v2/deployments/dpl_1/aliases"))
            .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":{"code":"not_found","message":"The domain used for the alias was not found"}}"#))
            .expect(1).mount(&server).await;
        let err = client(&server, TeamParam::Personal)
            .assign_alias("dpl_1", "nope.example.com")
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("The domain used for the alias was not found"),
            "{err}"
        );
    }
}
