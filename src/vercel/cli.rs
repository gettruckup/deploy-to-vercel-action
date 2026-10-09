//! The `vercel` CLI: v1-identical argv, URL parsing, and process execution (spec §5.2).

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

use crate::actions_io::Io;
use crate::context::RunContext;
use crate::error::{Error, Result};
use crate::github::CommitInfo;
use crate::inputs::js_trim;

pub struct DeployArgs<'a> {
    pub token: &'a str,
    pub scope: Option<&'a str>,
    pub production: bool,
    pub prebuilt: bool,
    pub force: bool,
    pub meta: Option<&'a [String]>,
    pub build_env: &'a [String],
}

/// Byte-identical to v1's argv order.
pub fn deploy_argv(args: &DeployArgs) -> Vec<String> {
    let mut argv = vec![format!("--token={}", args.token)];
    if let Some(scope) = args.scope.filter(|s| !s.is_empty()) {
        argv.push(format!("--scope={scope}"));
    }
    if args.production {
        argv.push("--prod".into());
    }
    if args.prebuilt {
        argv.push("--prebuilt".into());
    }
    if args.force {
        argv.push("--force".into());
    }
    for item in args.meta.unwrap_or_default() {
        argv.push("--meta".into());
        argv.push(item.clone());
    }
    for item in args.build_env {
        argv.push("--build-env".into());
        argv.push(item.clone());
    }
    for item in args.build_env {
        argv.push("--env".into());
        argv.push(item.clone());
    }
    argv
}

pub fn commit_meta(commit: &CommitInfo, ctx: &RunContext) -> Vec<String> {
    let message = if ctx.trim_commit_message {
        first_line(&commit.message)
    } else {
        commit.message.clone()
    };
    vec![
        format!("githubCommitAuthorName={}", commit.author_name),
        format!(
            "githubCommitAuthorLogin={}",
            commit.author_login.as_deref().unwrap_or_default()
        ),
        format!("githubCommitMessage={message}"),
        format!("githubCommitOrg={}", ctx.user),
        format!("githubCommitRepo={}", ctx.repository),
        format!("githubCommitRef={}", ctx.ref_name()),
        format!("githubCommitSha={}", ctx.sha),
        format!("githubOrg={}", ctx.user),
        format!("githubRepo={}", ctx.repository),
        "githubDeployment=1".to_string(),
    ]
}

/// JS `message.split(/\r?\n/)[0]`.
pub fn first_line(message: &str) -> String {
    match message.find('\n') {
        Some(index) => {
            let line = &message[..index];
            line.strip_suffix('\r').unwrap_or(line).to_string()
        }
        None => message.to_string(),
    }
}

/// v1 rule: the rest of the line after the first `http://` or `https://` in trimmed stdout.
pub fn parse_deployment_host(stdout: &str) -> Result<String> {
    let output = js_trim(stdout);
    let https = output.find("https://").map(|i| i + "https://".len());
    let http = output.find("http://").map(|i| i + "http://".len());
    let start = match (https, http) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let host: String = start
        .map(|i| {
            output[i..]
                .chars()
                .take_while(|c| !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
                .collect()
        })
        .unwrap_or_default();
    if host.is_empty() {
        Err(Error::msg("Could not parse deploymentUrl"))
    } else {
        Ok(host)
    }
}

#[allow(async_fn_in_trait)]
pub trait VercelCli {
    /// Runs `vercel <argv>` and returns its stdout.
    async fn deploy(&self, argv: &[String]) -> Result<String>;
}

pub struct ProcessCli<'a> {
    pub program: String,
    pub cwd: Option<PathBuf>,
    pub envs: Vec<(String, String)>,
    pub io: &'a Io,
}

impl VercelCli for ProcessCli<'_> {
    async fn deploy(&self, argv: &[String]) -> Result<String> {
        let cwd_label = self
            .cwd
            .as_ref()
            .map_or_else(|| ".".to_string(), |p| p.display().to_string());
        self.io.debug(&format!(
            "EXEC: \"{} {}\" in {cwd_label}",
            self.program,
            argv.join(",")
        ));
        let mut command = Command::new(&self.program);
        command
            .args(argv)
            .envs(self.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.cwd {
            if !dir.is_dir() {
                return Err(Error::msg(format!(
                    "WORKING_DIRECTORY {} does not exist",
                    dir.display()
                )));
            }
            command.current_dir(dir);
        }
        let mut child = command.spawn().map_err(|err| {
            Error::msg(format!(
                "Failed to run `{}`: {err}. Make sure the Vercel CLI is installed (e.g. `npm install -g vercel`).",
                self.program
            ))
        })?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (stdout, stderr) = tokio::join!(pump(stdout, self.io), pump(stderr, self.io));
        let status = child.wait().await?;
        let (stdout, stderr) = (stdout?, stderr?);
        if status.success() {
            Ok(stdout)
        } else if stderr.trim().is_empty() {
            Err(Error::msg(format!("Vercel CLI exited with {status}")))
        } else {
            Err(Error::msg(stderr))
        }
    }
}

/// Streams lines to `::debug::` (v1 visibility) and returns everything read, decoded lossily.
async fn pump<R: AsyncRead + Unpin>(reader: R, io: &Io) -> std::io::Result<String> {
    let mut reader = BufReader::new(reader);
    let mut all = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        io.debug(String::from_utf8_lossy(&line).trim_end_matches(['\n', '\r']));
        all.extend_from_slice(&line);
    }
    Ok(String::from_utf8_lossy(&all).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::test_push_context;

    #[derive(serde::Deserialize)]
    struct StdoutCase {
        stdout: String,
        host: Option<String>,
    }

    #[test]
    fn deploy_argv_matches_v1_order() {
        let meta = vec![
            "githubCommitAuthorName=Jane".to_string(),
            "githubDeployment=1".to_string(),
        ];
        let build_env = vec!["A=1".to_string(), "B=2".to_string()];
        let argv = deploy_argv(&DeployArgs {
            token: "t",
            scope: Some("team"),
            production: true,
            prebuilt: true,
            force: true,
            meta: Some(&meta),
            build_env: &build_env,
        });
        assert_eq!(
            argv,
            [
                "--token=t",
                "--scope=team",
                "--prod",
                "--prebuilt",
                "--force",
                "--meta",
                "githubCommitAuthorName=Jane",
                "--meta",
                "githubDeployment=1",
                "--build-env",
                "A=1",
                "--build-env",
                "B=2",
                "--env",
                "A=1",
                "--env",
                "B=2",
            ]
        );
    }

    #[test]
    fn deploy_argv_minimal() {
        let argv = deploy_argv(&DeployArgs {
            token: "t",
            scope: Some(""),
            production: false,
            prebuilt: false,
            force: false,
            meta: None,
            build_env: &[],
        });
        assert_eq!(argv, ["--token=t"]);
    }

    #[test]
    fn commit_meta_uses_ref_name_trims_message_and_tolerates_missing_login() {
        let mut ctx = test_push_context();
        ctx.trim_commit_message = true;
        let commit = CommitInfo {
            author_name: "Jane Doe".into(),
            author_login: None,
            message: "feat: subject\r\n\r\nbody".into(),
        };
        assert_eq!(
            commit_meta(&commit, &ctx),
            [
                "githubCommitAuthorName=Jane Doe",
                "githubCommitAuthorLogin=",
                "githubCommitMessage=feat: subject",
                "githubCommitOrg=octo",
                "githubCommitRepo=repo",
                "githubCommitRef=main",
                "githubCommitSha=0123456789abcdef",
                "githubOrg=octo",
                "githubRepo=repo",
                "githubDeployment=1",
            ]
        );
    }

    #[test]
    fn first_line_matches_js_split() {
        assert_eq!(first_line("a\r\nb"), "a");
        assert_eq!(first_line("a\nb"), "a");
        assert_eq!(first_line("a\r"), "a\r");
        assert_eq!(first_line("a"), "a");
    }

    #[test]
    fn parse_deployment_host_matches_v1_regex() {
        let cases: Vec<StdoutCase> =
            serde_json::from_str(include_str!("../../tests/golden/cli-stdout.json")).unwrap();
        for case in cases {
            match case.host {
                Some(host) => assert_eq!(
                    parse_deployment_host(&case.stdout).unwrap(),
                    host,
                    "{:?}",
                    case.stdout
                ),
                None => assert_eq!(
                    parse_deployment_host(&case.stdout).unwrap_err().to_string(),
                    "Could not parse deploymentUrl",
                    "{:?}",
                    case.stdout
                ),
            }
        }
    }

    #[cfg(unix)]
    mod process {
        use super::super::*;
        use crate::actions_io::SharedBuf;

        fn io() -> (Io, SharedBuf) {
            let buf = SharedBuf::default();
            (Io::new(Box::new(buf.clone()), None, None), buf)
        }

        /// Runs scripts through `/bin/sh <script>` so tests never exec a file they just wrote (avoids ETXTBSY).
        fn sh<'a>(io: &'a Io, cwd: Option<PathBuf>) -> ProcessCli<'a> {
            ProcessCli {
                program: "/bin/sh".into(),
                cwd,
                envs: vec![("VERCEL_ORG_ID".into(), "team_org".into())],
                io,
            }
        }

        fn script(dir: &std::path::Path, body: &str) -> String {
            let path = dir.join("fake-vercel.sh");
            std::fs::write(&path, body).unwrap();
            path.display().to_string()
        }

        #[tokio::test]
        async fn returns_stdout_and_streams_output_to_debug() {
            let dir = tempfile::tempdir().unwrap();
            let s = script(
                dir.path(),
                "echo \"args=$*\"\necho \"org=$VERCEL_ORG_ID\"\necho building >&2\necho https://proj-abc.vercel.app\n",
            );
            let (io, log) = io();
            let out = sh(&io, None).deploy(&[s, "--prod".into()]).await.unwrap();
            assert_eq!(
                out,
                "args=--prod\norg=team_org\nhttps://proj-abc.vercel.app\n"
            );
            assert!(log.contents().contains("::debug::building"));
        }

        #[tokio::test]
        async fn failure_reports_stderr() {
            let dir = tempfile::tempdir().unwrap();
            let s = script(dir.path(), "echo 'Error: boom' >&2\nexit 1\n");
            let (io, _) = io();
            assert_eq!(
                sh(&io, None).deploy(&[s]).await.unwrap_err().to_string(),
                "Error: boom\n"
            );
        }

        #[tokio::test]
        async fn failure_without_stderr_reports_exit_status() {
            let dir = tempfile::tempdir().unwrap();
            let s = script(dir.path(), "exit 3\n");
            let (io, _) = io();
            let message = sh(&io, None).deploy(&[s]).await.unwrap_err().to_string();
            assert!(message.starts_with("Vercel CLI exited with"), "{message}");
        }

        #[tokio::test]
        async fn missing_program_reports_install_hint() {
            let (io, _) = io();
            let cli = ProcessCli {
                program: "/nonexistent/vercel".into(),
                cwd: None,
                envs: vec![],
                io: &io,
            };
            let message = cli.deploy(&[]).await.unwrap_err().to_string();
            assert!(
                message.starts_with("Failed to run `/nonexistent/vercel`")
                    && message.contains("npm install -g vercel"),
                "{message}"
            );
        }

        #[tokio::test]
        async fn runs_in_working_directory() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("sub")).unwrap();
            let s = script(dir.path(), "pwd\n");
            let (io, _) = io();
            let out = sh(&io, Some(dir.path().join("sub")))
                .deploy(&[s])
                .await
                .unwrap();
            assert!(out.trim_end().ends_with("/sub"), "{out}");
        }

        #[tokio::test]
        async fn missing_working_directory_is_reported() {
            let (io, _) = io();
            let err = sh(&io, Some(PathBuf::from("/nonexistent/app")))
                .deploy(&[])
                .await
                .unwrap_err();
            assert_eq!(
                err.to_string(),
                "WORKING_DIRECTORY /nonexistent/app does not exist"
            );
        }

        #[tokio::test]
        async fn non_utf8_output_does_not_panic() {
            let dir = tempfile::tempdir().unwrap();
            let s = script(
                dir.path(),
                "printf '\\377\\n'\nprintf '\\376\\n' >&2\necho https://x.vercel.app\n",
            );
            let (io, _) = io();
            let out = sh(&io, None).deploy(&[s]).await.unwrap();
            assert!(out.ends_with("https://x.vercel.app\n"), "{out:?}");
        }
    }
}
