//! Shared HTTP client, retry policy and path encoding for the GitHub and Vercel REST APIs (spec §7.2).

use std::time::Duration;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};

use crate::error::{Error, Result};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// RFC 3986 unreserved characters stay as-is, like octokit's path parameter encoding.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}

pub fn build_client(user_agent: &str) -> Client {
    // reqwest is built without a bundled provider; ring cross-compiles cleanly to musl.
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .user_agent(user_agent)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("failed to build HTTP client")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_secs(1),
        }
    }
}

#[cfg(test)]
pub(crate) const TEST_RETRY_POLICY: RetryPolicy = RetryPolicy {
    max_retries: 3,
    base_delay: Duration::from_millis(1),
};

pub fn delay_for(policy: RetryPolicy, attempt: u32, retry_after: Option<Duration>) -> Duration {
    match retry_after {
        Some(delay) => delay.min(MAX_RETRY_AFTER),
        None => policy.base_delay * 2u32.pow(attempt),
    }
}

/// Sends the request built by `make`, retrying per the policy. Non-success statuses are returned, not errors.
pub async fn send(
    policy: RetryPolicy,
    method: &Method,
    make: impl Fn() -> RequestBuilder,
) -> Result<Response> {
    let idempotent = [Method::GET, Method::HEAD, Method::DELETE].contains(method);
    let mut attempt = 0;
    loop {
        let result = make().send().await;
        let retry = match &result {
            Ok(response) => {
                should_retry_status(response, idempotent).then(|| retry_after(response))
            }
            Err(err) => should_retry_error(err, idempotent).then_some(None),
        };
        match retry {
            Some(hint) if attempt < policy.max_retries => {
                tokio::time::sleep(delay_for(policy, attempt, hint)).await;
                attempt += 1;
            }
            _ => return Ok(result?),
        }
    }
}

fn should_retry_status(response: &Response, idempotent: bool) -> bool {
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    if idempotent {
        status.is_server_error() || is_secondary_rate_limit(response)
    } else {
        matches!(status.as_u16(), 502..=504)
    }
}

fn is_secondary_rate_limit(response: &Response) -> bool {
    let headers = response.headers();
    response.status() == StatusCode::FORBIDDEN
        && (headers.contains_key("retry-after")
            || headers
                .get("x-ratelimit-remaining")
                .is_some_and(|v| v.as_bytes() == b"0"))
}

fn should_retry_error(err: &reqwest::Error, idempotent: bool) -> bool {
    if idempotent {
        err.is_connect() || err.is_timeout() || err.is_request()
    } else {
        err.is_connect()
    }
}

fn retry_after(response: &Response) -> Option<Duration> {
    let value = response.headers().get("retry-after")?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

pub async fn status_error(response: Response, method: &Method) -> Error {
    let status = response.status().as_u16();
    let url = response.url().to_string();
    let body = response.text().await.unwrap_or_default();
    Error::Status {
        method: method.to_string(),
        url,
        status,
        body,
    }
}

pub async fn ensure_success(response: Response, method: &Method) -> Result<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(status_error(response, method).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn hits(server: &MockServer) -> usize {
        server.received_requests().await.unwrap().len()
    }

    async fn server_with(
        verb: &str,
        first: ResponseTemplate,
        times: u64,
        then: ResponseTemplate,
    ) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method(verb))
            .and(path("/x"))
            .respond_with(first)
            .up_to_n_times(times)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method(verb))
            .and(path("/x"))
            .respond_with(then)
            .with_priority(2)
            .mount(&server)
            .await;
        server
    }

    async fn call(server: &MockServer, verb: Method) -> Result<Response> {
        let client = build_client("test");
        let url = format!("{}/x", server.uri());
        send(TEST_RETRY_POLICY, &verb, || {
            client.request(verb.clone(), &url)
        })
        .await
    }

    #[tokio::test]
    async fn get_retries_server_errors_then_succeeds() {
        let server = server_with(
            "GET",
            ResponseTemplate::new(500),
            2,
            ResponseTemplate::new(200),
        )
        .await;
        assert_eq!(call(&server, Method::GET).await.unwrap().status(), 200);
        assert_eq!(hits(&server).await, 3);
    }

    #[tokio::test]
    async fn get_gives_up_after_three_retries() {
        let server = server_with(
            "GET",
            ResponseTemplate::new(500),
            100,
            ResponseTemplate::new(200),
        )
        .await;
        assert_eq!(call(&server, Method::GET).await.unwrap().status(), 500);
        assert_eq!(hits(&server).await, 4);
    }

    #[tokio::test]
    async fn post_does_not_retry_500() {
        let server = server_with(
            "POST",
            ResponseTemplate::new(500),
            1,
            ResponseTemplate::new(201),
        )
        .await;
        assert_eq!(call(&server, Method::POST).await.unwrap().status(), 500);
        assert_eq!(hits(&server).await, 1);
    }

    #[tokio::test]
    async fn post_retries_503() {
        let server = server_with(
            "POST",
            ResponseTemplate::new(503),
            1,
            ResponseTemplate::new(201),
        )
        .await;
        assert_eq!(call(&server, Method::POST).await.unwrap().status(), 201);
        assert_eq!(hits(&server).await, 2);
    }

    #[tokio::test]
    async fn post_retries_429_with_retry_after() {
        let limited = ResponseTemplate::new(429).insert_header("retry-after", "0");
        let server = server_with("POST", limited, 1, ResponseTemplate::new(201)).await;
        assert_eq!(call(&server, Method::POST).await.unwrap().status(), 201);
        assert_eq!(hits(&server).await, 2);
    }

    #[tokio::test]
    async fn get_retries_secondary_rate_limit_but_post_does_not_retry_plain_403() {
        let limited = ResponseTemplate::new(403).insert_header("retry-after", "0");
        let server = server_with("GET", limited, 1, ResponseTemplate::new(200)).await;
        assert_eq!(call(&server, Method::GET).await.unwrap().status(), 200);
        assert_eq!(hits(&server).await, 2);

        let server = server_with(
            "POST",
            ResponseTemplate::new(403),
            1,
            ResponseTemplate::new(201),
        )
        .await;
        assert_eq!(call(&server, Method::POST).await.unwrap().status(), 403);
        assert_eq!(hits(&server).await, 1);
    }

    #[tokio::test]
    async fn ensure_success_reports_status_and_body() {
        let server = server_with(
            "GET",
            ResponseTemplate::new(404).set_body_string("nope"),
            100,
            ResponseTemplate::new(200),
        )
        .await;
        let response = call(&server, Method::GET).await.unwrap();
        match ensure_success(response, &Method::GET).await {
            Err(Error::Status {
                method,
                status,
                body,
                url,
            }) => {
                assert_eq!(
                    (method.as_str(), status, body.as_str()),
                    ("GET", 404, "nope")
                );
                assert!(url.ends_with("/x"));
            }
            other => panic!("expected status error, got {other:?}"),
        }
    }

    #[test]
    fn backoff_doubles_and_retry_after_is_capped() {
        let policy = RetryPolicy::default();
        assert_eq!(delay_for(policy, 0, None), Duration::from_secs(1));
        assert_eq!(delay_for(policy, 2, None), Duration::from_secs(4));
        assert_eq!(
            delay_for(policy, 0, Some(Duration::from_secs(600))),
            MAX_RETRY_AFTER
        );
    }

    #[test]
    fn encode_segment_matches_octokit() {
        assert_eq!(encode_segment("refs/heads/main"), "refs%2Fheads%2Fmain");
        assert_eq!(encode_segment("a-b.c_d~e"), "a-b.c_d~e");
        assert_eq!(encode_segment("ñ x"), "%C3%B1%20x");
    }

    #[tokio::test]
    #[ignore = "needs network; run in CI with --ignored tls_smoke"]
    async fn tls_smoke() {
        let response = build_client("deploy-to-vercel-action/test")
            .get("https://api.github.com/zen")
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success() || response.status() == StatusCode::FORBIDDEN);
    }
}
