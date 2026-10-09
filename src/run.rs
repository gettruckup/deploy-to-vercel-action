//! Orchestrates one action run (spec §5.1, failure semantics §7.1). All I/O goes through traits.

use futures_util::future::join_all;

use crate::actions_io::Io;
use crate::aliases::{self, TemplateVars};
use crate::comment;
use crate::context::RunContext;
use crate::error::{Error, Result};
use crate::github::{CommitInfo, DeploymentState, GitHubApi};
use crate::inputs::{Inputs, non_empty_str};
use crate::vercel::api::{DeploymentInfo, VercelApi};
use crate::vercel::cli::{self, DeployArgs, VercelCli};

pub struct Deps<'a, G, V, C> {
    pub github: &'a G,
    pub vercel: &'a V,
    pub cli: &'a C,
    pub io: &'a Io,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Deployed,
    ForkRefused,
}

/// Every error that failed the run, in reporting order.
#[derive(Debug)]
pub struct RunFailure {
    pub errors: Vec<Error>,
}

impl From<Error> for RunFailure {
    fn from(err: Error) -> Self {
        Self { errors: vec![err] }
    }
}

struct Deployed {
    urls: Vec<String>,
    info: DeploymentInfo,
}

pub async fn run<G: GitHubApi, V: VercelApi, C: VercelCli>(
    inputs: &Inputs,
    ctx: &RunContext,
    deps: &Deps<'_, G, V, C>,
) -> std::result::Result<Outcome, RunFailure> {
    let io = deps.io;
    if ctx.is_fork && !inputs.deploy_pr_from_fork {
        refuse_fork(ctx, deps.github, io).await?;
        return Ok(Outcome::ForkRefused);
    }

    io.info("Setting environment variables for Vercel CLI");
    io.export_var("VERCEL_ORG_ID", &inputs.vercel_org_id)?;
    io.export_var("VERCEL_PROJECT_ID", &inputs.vercel_project_id)?;

    // Step 3: both branches run to completion; a deployment-creation failure wins and sets no status.
    let (deployment, commit) = tokio::join!(
        create_github_deployment(inputs, ctx, deps.github, io),
        fetch_commit(inputs, ctx, deps.github),
    );
    let deployment_id = deployment?;
    let deployed = match commit {
        Ok(commit) => deploy(inputs, ctx, deps, commit.as_ref()).await,
        Err(err) => Err(err),
    };
    let deployed = match deployed {
        Ok(deployed) => deployed,
        Err(err) => {
            mark_failed(ctx, deps.github, io, deployment_id).await;
            return Err(err.into());
        }
    };

    // F14: outputs are written before post-deploy work, which never flips the deployment to failure.
    write_outputs(inputs, ctx, &deployed, io)?;
    let preview_url = deployed.urls[0].as_str();
    let (status, comment, labels) = tokio::join!(
        mark_succeeded(deps.github, io, deployment_id, preview_url),
        replace_comment(
            inputs,
            ctx,
            deps.github,
            io,
            preview_url,
            &deployed.info.inspector_url
        ),
        add_labels(inputs, ctx, deps.github, io),
    );
    let errors: Vec<Error> = [status, comment, labels]
        .into_iter()
        .filter_map(|r| r.err())
        .collect();
    if !errors.is_empty() {
        return Err(RunFailure { errors });
    }

    io.info("Done");
    Ok(Outcome::Deployed)
}

fn pr_number(ctx: &RunContext) -> &str {
    ctx.pr_number.as_deref().unwrap_or_default()
}

async fn refuse_fork<G: GitHubApi>(ctx: &RunContext, github: &G, io: &Io) -> Result<()> {
    io.warning("PR is from fork and DEPLOY_PR_FROM_FORK is set to false");
    let created = github
        .create_comment(
            pr_number(ctx),
            &comment::fork_refusal(&ctx.actor, &ctx.user),
        )
        .await?;
    io.info(&format!("Comment created: {}", created.html_url));
    io.set_output("DEPLOYMENT_CREATED", "false")?;
    io.set_output("COMMENT_CREATED", "true")?;
    io.info("Done");
    Ok(())
}

async fn create_github_deployment<G: GitHubApi>(
    inputs: &Inputs,
    ctx: &RunContext,
    github: &G,
    io: &Io,
) -> Result<Option<u64>> {
    if !inputs.github_deployment {
        return Ok(None);
    }
    io.info("Creating GitHub deployment");
    let environment = match non_empty_str(&inputs.github_deployment_env) {
        Some(environment) => environment.to_string(),
        None if ctx.production => "Production".to_string(),
        None => "Preview".to_string(),
    };
    let Some(id) = github.create_deployment(&ctx.git_ref, &environment).await? else {
        io.warning("GitHub returned no deployment id; continuing without a GitHub deployment");
        return Ok(None);
    };
    io.info(&format!("Deployment #{id} created"));
    github
        .create_deployment_status(id, DeploymentState::Pending, &ctx.log_url)
        .await?;
    io.info(&format!("Deployment #{id} status changed to \"pending\""));
    Ok(Some(id))
}

async fn fetch_commit<G: GitHubApi>(
    inputs: &Inputs,
    ctx: &RunContext,
    github: &G,
) -> Result<Option<CommitInfo>> {
    if !inputs.attach_commit_metadata {
        return Ok(None);
    }
    github.get_commit(&ctx.git_ref).await.map(Some)
}

async fn deploy<G, V: VercelApi, C: VercelCli>(
    inputs: &Inputs,
    ctx: &RunContext,
    deps: &Deps<'_, G, V, C>,
    commit: Option<&CommitInfo>,
) -> Result<Deployed> {
    let io = deps.io;
    io.info("Creating deployment with Vercel CLI");
    let meta = commit.map(|commit| cli::commit_meta(commit, ctx));
    let argv = cli::deploy_argv(&DeployArgs {
        token: &inputs.vercel_token,
        scope: non_empty_str(&inputs.vercel_scope),
        production: ctx.production,
        prebuilt: inputs.prebuilt,
        force: inputs.force,
        meta: meta.as_deref(),
        build_env: inputs.build_env.as_deref().unwrap_or_default(),
    });
    io.info("Starting deploy with Vercel CLI");
    let stdout = deps.cli.deploy(&argv).await?;
    let host = cli::parse_deployment_host(&stdout)?;
    io.info("Successfully deployed to Vercel!");

    let info = deps.vercel.get_deployment(&host).await?;
    let planned = planned_aliases(inputs, ctx, io);
    let hosts: Vec<String> = planned
        .iter()
        .map(|alias| aliases::remove_schema(alias))
        .collect();
    join_all(
        hosts
            .iter()
            .map(|alias| deps.vercel.assign_alias(&info.id, alias)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<()>>>()?;

    let mut urls: Vec<String> = planned
        .iter()
        .map(|alias| aliases::add_schema(alias))
        .collect();
    urls.push(aliases::add_schema(&host));
    io.info(&format!(
        "Deployment \"{}\" available at: {}",
        info.id,
        urls.join(", ")
    ));
    Ok(Deployed { urls, info })
}

fn planned_aliases(inputs: &Inputs, ctx: &RunContext, io: &Io) -> Vec<String> {
    let vars = TemplateVars {
        user: &ctx.user,
        repository: &ctx.repository,
        branch: &ctx.branch,
        sha: &ctx.sha,
        pr_number: ctx.pr_number.as_deref(),
    };
    if ctx.is_pr {
        let Some(template) = non_empty_str(&inputs.pr_preview_domain) else {
            return Vec::new();
        };
        io.info("Assigning custom preview domain to PR");
        let preview = aliases::preview_alias(template, &vars);
        if let Some(prefix) = &preview.truncated_from {
            io.warning(&format!(
                "The alias {prefix} exceeds 60 chars in length, truncating using vercel's rules. See https://vercel.com/docs/concepts/deployments/automatic-urls#automatic-branch-urls"
            ));
            io.info(&format!("Updated domain alias: {}", preview.alias));
        }
        vec![preview.alias]
    } else {
        let Some(domains) = &inputs.alias_domains else {
            return Vec::new();
        };
        io.info("Assigning custom domains to Vercel deployment");
        domains
            .iter()
            .map(|template| aliases::domain_alias(template, &vars))
            .collect()
    }
}

fn write_outputs(inputs: &Inputs, ctx: &RunContext, deployed: &Deployed, io: &Io) -> Result<()> {
    let bool_str = |value: bool| if value { "true" } else { "false" };
    io.set_output("PREVIEW_URL", &deployed.urls[0])?;
    io.set_output("DEPLOYMENT_URLS", &serde_json::to_string(&deployed.urls)?)?;
    io.set_output(
        "DEPLOYMENT_UNIQUE_URL",
        deployed.urls.last().map(String::as_str).unwrap_or_default(),
    )?;
    io.set_output("DEPLOYMENT_ID", &deployed.info.id)?;
    io.set_output("DEPLOYMENT_INSPECTOR_URL", &deployed.info.inspector_url)?;
    io.set_output("DEPLOYMENT_CREATED", "true")?;
    io.set_output(
        "COMMENT_CREATED",
        bool_str(ctx.is_pr && inputs.create_comment),
    )?;
    Ok(())
}

/// F6: a failing failure-status call is a warning; the original error is what gets reported.
async fn mark_failed<G: GitHubApi>(
    ctx: &RunContext,
    github: &G,
    io: &Io,
    deployment_id: Option<u64>,
) {
    let Some(id) = deployment_id else { return };
    if let Err(err) = github
        .create_deployment_status(id, DeploymentState::Failure, &ctx.log_url)
        .await
    {
        io.warning(&format!(
            "Could not set GitHub deployment status to \"failure\": {err}"
        ));
    }
}

async fn mark_succeeded<G: GitHubApi>(
    github: &G,
    io: &Io,
    deployment_id: Option<u64>,
    preview_url: &str,
) -> Result<()> {
    let Some(id) = deployment_id else {
        return Ok(());
    };
    io.info("Changing GitHub deployment status to \"success\"");
    github
        .create_deployment_status(id, DeploymentState::Success, preview_url)
        .await
}

async fn replace_comment<G: GitHubApi>(
    inputs: &Inputs,
    ctx: &RunContext,
    github: &G,
    io: &Io,
    preview_url: &str,
    inspector_url: &str,
) -> Result<()> {
    if !ctx.is_pr {
        return Ok(());
    }
    if inputs.delete_existing_comment {
        io.info("Checking for existing comment on PR");
        if let Some(id) = github.delete_existing_comment(pr_number(ctx)).await? {
            io.info(&format!("Deleted existing comment #{id}"));
        }
    }
    if inputs.create_comment {
        io.info("Creating new comment on PR");
        let body = comment::deployed(&ctx.sha, preview_url, inspector_url, &ctx.log_url);
        let created = github.create_comment(pr_number(ctx), &body).await?;
        io.info(&format!("Comment created: {}", created.html_url));
    }
    Ok(())
}

async fn add_labels<G: GitHubApi>(
    inputs: &Inputs,
    ctx: &RunContext,
    github: &G,
    io: &Io,
) -> Result<()> {
    let Some(labels) = inputs
        .pr_labels
        .as_ref()
        .filter(|labels| ctx.is_pr && !labels.is_empty())
    else {
        return Ok(());
    };
    io.info("Adding label(s) to PR");
    let added = github.add_labels(pr_number(ctx), labels).await?;
    io.info(&format!("Label(s) \"{}\" added", added.join(", ")));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions_io::{SharedBuf, parse_file_commands};
    use crate::context::{test_pr_context, test_push_context};
    use crate::github::{CreatedComment, DeploymentState};
    use crate::inputs::test_inputs;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::path::PathBuf;

    #[derive(Default)]
    struct FakeGitHub {
        calls: RefCell<Vec<String>>,
        bodies: RefCell<Vec<String>>,
        fail: HashSet<&'static str>,
        no_deployment_id: bool,
        existing_comment: Option<u64>,
        anonymous_author: bool,
    }

    impl FakeGitHub {
        fn failing(ops: &[&'static str]) -> Self {
            Self {
                fail: ops.iter().copied().collect(),
                ..Self::default()
            }
        }

        fn record(&self, op: &'static str, detail: String) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("{op} {detail}").trim_end().to_string());
            if self.fail.contains(op) {
                Err(Error::msg(format!("{op} failed")))
            } else {
                Ok(())
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl GitHubApi for FakeGitHub {
        async fn create_deployment(&self, git_ref: &str, environment: &str) -> Result<Option<u64>> {
            self.record("create_deployment", format!("{git_ref} {environment}"))?;
            Ok((!self.no_deployment_id).then_some(42))
        }

        async fn create_deployment_status(
            &self,
            id: u64,
            state: DeploymentState,
            url: &str,
        ) -> Result<()> {
            let op = match state {
                DeploymentState::Pending => "status_pending",
                DeploymentState::Success => "status_success",
                DeploymentState::Failure => "status_failure",
            };
            self.record(op, format!("{id} {url}"))
        }

        async fn get_commit(&self, git_ref: &str) -> Result<CommitInfo> {
            self.record("get_commit", git_ref.to_string())?;
            Ok(CommitInfo {
                author_name: "Jane Doe".into(),
                author_login: (!self.anonymous_author).then(|| "jane".to_string()),
                message: "feat: add thing\n\nbody".into(),
            })
        }

        async fn delete_existing_comment(&self, pr: &str) -> Result<Option<u64>> {
            self.record("delete_comment", pr.to_string())?;
            Ok(self.existing_comment)
        }

        async fn create_comment(&self, pr: &str, body: &str) -> Result<CreatedComment> {
            self.record("create_comment", pr.to_string())?;
            self.bodies.borrow_mut().push(body.to_string());
            Ok(CreatedComment {
                id: 99,
                html_url: "https://github.com/octo/repo/pull/7#issuecomment-99".into(),
            })
        }

        async fn add_labels(&self, pr: &str, labels: &[String]) -> Result<Vec<String>> {
            self.record("add_labels", format!("{pr} {}", labels.join(",")))?;
            Ok(labels.to_vec())
        }
    }

    #[derive(Default)]
    struct FakeVercel {
        calls: RefCell<Vec<String>>,
        fail_aliases: HashSet<String>,
    }

    impl VercelApi for FakeVercel {
        async fn get_deployment(&self, host: &str) -> Result<DeploymentInfo> {
            self.calls
                .borrow_mut()
                .push(format!("get_deployment {host}"));
            Ok(DeploymentInfo {
                id: "dpl_1".into(),
                inspector_url: "https://vercel.com/octo/repo/dpl1".into(),
            })
        }

        async fn assign_alias(&self, id: &str, alias: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("assign_alias {id} {alias}"));
            if self.fail_aliases.contains(alias) {
                Err(Error::msg(format!("alias {alias} failed")))
            } else {
                Ok(())
            }
        }
    }

    impl FakeVercel {
        fn aliases(&self) -> Vec<String> {
            let mut aliases: Vec<String> = self
                .calls
                .borrow()
                .iter()
                .filter(|c| c.starts_with("assign_alias"))
                .cloned()
                .collect();
            aliases.sort();
            aliases
        }
    }

    struct FakeCli {
        argv: RefCell<Option<Vec<String>>>,
        result: std::result::Result<String, String>,
    }

    impl FakeCli {
        fn ok() -> Self {
            Self {
                argv: RefCell::new(None),
                result: Ok("https://proj-abc.vercel.app\n".into()),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                argv: RefCell::new(None),
                result: Err(message.into()),
            }
        }

        fn argv(&self) -> Option<Vec<String>> {
            self.argv.borrow().clone()
        }
    }

    impl VercelCli for FakeCli {
        async fn deploy(&self, argv: &[String]) -> Result<String> {
            *self.argv.borrow_mut() = Some(argv.to_vec());
            self.result.clone().map_err(Error::msg)
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        output: PathBuf,
        env: PathBuf,
        log: SharedBuf,
        io: Io,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (output, env) = (dir.path().join("output"), dir.path().join("env"));
            let log = SharedBuf::default();
            let io = Io::new(
                Box::new(log.clone()),
                Some(output.clone()),
                Some(env.clone()),
            );
            Self {
                _dir: dir,
                output,
                env,
                log,
                io,
            }
        }

        fn outputs(&self) -> Vec<(String, String)> {
            std::fs::read_to_string(&self.output)
                .map(|c| parse_file_commands(&c))
                .unwrap_or_default()
        }

        fn output(&self, name: &str) -> Option<String> {
            self.outputs()
                .into_iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v)
        }

        fn exported(&self) -> Vec<(String, String)> {
            std::fs::read_to_string(&self.env)
                .map(|c| parse_file_commands(&c))
                .unwrap_or_default()
        }

        fn log(&self) -> String {
            self.log.contents()
        }
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn assert_in_order(calls: &[String], expected: &[&str]) {
        let mut from = 0;
        for needle in expected {
            let found = calls[from..]
                .iter()
                .position(|c| c == needle)
                .unwrap_or_else(|| panic!("{needle:?} not found in order in {calls:?}"));
            from += found + 1;
        }
    }

    async fn execute(
        inputs: &Inputs,
        ctx: &RunContext,
        gh: &FakeGitHub,
        vercel: &FakeVercel,
        cli: &FakeCli,
        h: &Harness,
    ) -> std::result::Result<Outcome, RunFailure> {
        run(
            inputs,
            ctx,
            &Deps {
                github: gh,
                vercel,
                cli,
                io: &h.io,
            },
        )
        .await
    }

    fn messages(failure: RunFailure) -> Vec<String> {
        failure.errors.iter().map(ToString::to_string).collect()
    }

    const LOG: &str = "https://github.com/octo/repo/actions/runs/99";

    #[tokio::test]
    async fn push_deploys_with_aliases_and_writes_outputs() {
        let mut inputs = test_inputs();
        inputs.alias_domains = Some(vec![
            "{BRANCH}.example.com".into(),
            "App.Example.com".into(),
        ]);
        inputs.build_env = Some(vec!["A=1".into()]);
        inputs.vercel_scope = Some("team-slug".into());
        let (gh, vercel, cli, h) = (
            FakeGitHub::default(),
            FakeVercel::default(),
            FakeCli::ok(),
            Harness::new(),
        );

        assert_eq!(
            execute(&inputs, &test_push_context(), &gh, &vercel, &cli, &h)
                .await
                .unwrap(),
            Outcome::Deployed
        );

        let calls = gh.calls();
        assert_in_order(
            &calls,
            &[
                "create_deployment refs/heads/main Production",
                &format!("status_pending 42 {LOG}"),
                "status_success 42 https://main.example.com",
            ],
        );
        assert!(calls.contains(&"get_commit refs/heads/main".to_string()));
        assert_eq!(calls.len(), 4);
        assert_eq!(
            cli.argv().unwrap(),
            [
                "--token=vercel-token",
                "--scope=team-slug",
                "--prod",
                "--meta",
                "githubCommitAuthorName=Jane Doe",
                "--meta",
                "githubCommitAuthorLogin=jane",
                "--meta",
                "githubCommitMessage=feat: add thing\n\nbody",
                "--meta",
                "githubCommitOrg=octo",
                "--meta",
                "githubCommitRepo=repo",
                "--meta",
                "githubCommitRef=main",
                "--meta",
                "githubCommitSha=0123456789abcdef",
                "--meta",
                "githubOrg=octo",
                "--meta",
                "githubRepo=repo",
                "--meta",
                "githubDeployment=1",
                "--build-env",
                "A=1",
                "--env",
                "A=1",
            ]
        );
        assert_eq!(
            vercel.calls.borrow()[0],
            "get_deployment proj-abc.vercel.app"
        );
        assert_eq!(
            vercel.aliases(),
            [
                "assign_alias dpl_1 app.example.com",
                "assign_alias dpl_1 main.example.com"
            ]
        );
        assert_eq!(
            h.outputs(),
            pairs(&[
                ("PREVIEW_URL", "https://main.example.com"),
                (
                    "DEPLOYMENT_URLS",
                    r#"["https://main.example.com","https://app.example.com","https://proj-abc.vercel.app"]"#
                ),
                ("DEPLOYMENT_UNIQUE_URL", "https://proj-abc.vercel.app"),
                ("DEPLOYMENT_ID", "dpl_1"),
                (
                    "DEPLOYMENT_INSPECTOR_URL",
                    "https://vercel.com/octo/repo/dpl1"
                ),
                ("DEPLOYMENT_CREATED", "true"),
                ("COMMENT_CREATED", "false"),
            ])
        );
        assert_eq!(
            h.exported(),
            pairs(&[
                ("VERCEL_ORG_ID", "team_org"),
                ("VERCEL_PROJECT_ID", "prj_1")
            ])
        );
        assert!(h.log().ends_with("Done\n"));
    }

    #[tokio::test]
    async fn pr_deploy_assigns_preview_domain_comments_and_labels() {
        let mut inputs = test_inputs();
        inputs.production = false;
        inputs.pr_preview_domain = Some("pr{PR}.app.example.com".into());
        inputs.alias_domains = Some(vec!["ignored.example.com".into()]);
        let ctx = test_pr_context();
        let gh = FakeGitHub {
            existing_comment: Some(11),
            ..FakeGitHub::default()
        };
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());

        execute(&inputs, &ctx, &gh, &vercel, &cli, &h)
            .await
            .unwrap();

        let calls = gh.calls();
        assert_in_order(
            &calls,
            &[
                "create_deployment feature/x Preview",
                &format!("status_pending 42 {LOG}"),
            ],
        );
        assert_in_order(&calls, &["delete_comment 7", "create_comment 7"]);
        assert!(calls.contains(&"status_success 42 https://pr7.app.example.com".to_string()));
        assert!(calls.contains(&"add_labels 7 deployed".to_string()));
        assert_eq!(vercel.aliases(), ["assign_alias dpl_1 pr7.app.example.com"]);
        assert!(!cli.argv().unwrap().contains(&"--prod".to_string()));
        assert_eq!(
            gh.bodies.borrow()[0],
            comment::deployed(
                &ctx.sha,
                "https://pr7.app.example.com",
                "https://vercel.com/octo/repo/dpl1",
                LOG
            )
        );
        assert_eq!(
            h.output("PREVIEW_URL").as_deref(),
            Some("https://pr7.app.example.com")
        );
        assert_eq!(h.output("COMMENT_CREATED").as_deref(), Some("true"));
        assert!(h.log().contains("Deleted existing comment #11"));
    }

    #[tokio::test]
    async fn disabled_or_empty_labels_skip_label_call() {
        for labels in [None, Some(vec![])] {
            let mut inputs = test_inputs();
            inputs.pr_labels = labels;
            let (gh, vercel, cli, h) = (
                FakeGitHub::default(),
                FakeVercel::default(),
                FakeCli::ok(),
                Harness::new(),
            );
            execute(&inputs, &test_pr_context(), &gh, &vercel, &cli, &h)
                .await
                .unwrap();
            assert!(!gh.calls().iter().any(|c| c.starts_with("add_labels")));
        }
    }

    #[tokio::test]
    async fn fork_pr_is_refused_with_comment() {
        let mut ctx = test_pr_context();
        ctx.is_fork = true;
        let (gh, vercel, cli, h) = (
            FakeGitHub::default(),
            FakeVercel::default(),
            FakeCli::ok(),
            Harness::new(),
        );

        assert_eq!(
            execute(&test_inputs(), &ctx, &gh, &vercel, &cli, &h)
                .await
                .unwrap(),
            Outcome::ForkRefused
        );

        assert_eq!(gh.calls(), ["create_comment 7"]);
        assert_eq!(
            gh.bodies.borrow()[0],
            comment::fork_refusal("contributor", "octo")
        );
        assert_eq!(
            h.outputs(),
            pairs(&[("DEPLOYMENT_CREATED", "false"), ("COMMENT_CREATED", "true")])
        );
        assert_eq!(cli.argv(), None);
        assert!(h.exported().is_empty());
        assert!(
            h.log()
                .contains("::warning::PR is from fork and DEPLOY_PR_FROM_FORK is set to false")
        );
    }

    #[tokio::test]
    async fn fork_pr_deploys_when_allowed() {
        let mut inputs = test_inputs();
        inputs.deploy_pr_from_fork = true;
        let mut ctx = test_pr_context();
        ctx.is_fork = true;
        let (gh, vercel, cli, h) = (
            FakeGitHub::default(),
            FakeVercel::default(),
            FakeCli::ok(),
            Harness::new(),
        );
        assert_eq!(
            execute(&inputs, &ctx, &gh, &vercel, &cli, &h)
                .await
                .unwrap(),
            Outcome::Deployed
        );
        assert!(cli.argv().is_some());
    }

    #[tokio::test]
    async fn without_github_deployment_or_commit_metadata() {
        let mut inputs = test_inputs();
        inputs.github_deployment = false;
        inputs.attach_commit_metadata = false;
        let (gh, vercel, cli, h) = (
            FakeGitHub::default(),
            FakeVercel::default(),
            FakeCli::ok(),
            Harness::new(),
        );
        execute(&inputs, &test_push_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap();
        assert!(gh.calls().is_empty());
        assert_eq!(cli.argv().unwrap(), ["--token=vercel-token", "--prod"]);
    }

    #[tokio::test]
    async fn missing_deployment_id_continues_without_statuses() {
        let gh = FakeGitHub {
            no_deployment_id: true,
            ..FakeGitHub::default()
        };
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());
        assert_eq!(
            execute(&test_inputs(), &test_push_context(), &gh, &vercel, &cli, &h)
                .await
                .unwrap(),
            Outcome::Deployed
        );
        assert!(!gh.calls().iter().any(|c| c.starts_with("status_")));
        assert!(h.log().contains(
            "::warning::GitHub returned no deployment id; continuing without a GitHub deployment"
        ));
    }

    #[tokio::test]
    async fn cli_failure_marks_deployment_failed() {
        let (gh, vercel, h) = (FakeGitHub::default(), FakeVercel::default(), Harness::new());
        let cli = FakeCli::failing("Error: build failed");
        let failure = execute(&test_inputs(), &test_push_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(messages(failure), ["Error: build failed"]);
        assert!(gh.calls().contains(&format!("status_failure 42 {LOG}")));
        assert!(h.outputs().is_empty());
    }

    #[tokio::test]
    async fn commit_failure_marks_failed_and_skips_deploy() {
        let gh = FakeGitHub::failing(&["get_commit"]);
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());
        let failure = execute(&test_inputs(), &test_push_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(messages(failure), ["get_commit failed"]);
        assert!(gh.calls().contains(&format!("status_failure 42 {LOG}")));
        assert_eq!(cli.argv(), None);
    }

    #[tokio::test]
    async fn create_deployment_failure_sets_no_status() {
        let gh = FakeGitHub::failing(&["create_deployment"]);
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());
        let failure = execute(&test_inputs(), &test_push_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(messages(failure), ["create_deployment failed"]);
        assert!(!gh.calls().iter().any(|c| c.starts_with("status_")));
        assert_eq!(cli.argv(), None);
    }

    #[tokio::test]
    async fn alias_failures_report_first_in_input_order() {
        let mut inputs = test_inputs();
        inputs.alias_domains = Some(vec!["a.example.com".into(), "b.example.com".into()]);
        let vercel = FakeVercel {
            fail_aliases: ["a.example.com".to_string(), "b.example.com".to_string()].into(),
            ..FakeVercel::default()
        };
        let (gh, cli, h) = (FakeGitHub::default(), FakeCli::ok(), Harness::new());
        let failure = execute(&inputs, &test_push_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(messages(failure), ["alias a.example.com failed"]);
        assert_eq!(
            vercel.aliases().len(),
            2,
            "all alias requests run to completion"
        );
        assert!(gh.calls().contains(&format!("status_failure 42 {LOG}")));
        assert!(h.outputs().is_empty());
    }

    #[tokio::test]
    async fn failure_status_error_keeps_original_error() {
        let gh = FakeGitHub::failing(&["status_failure"]);
        let (vercel, h) = (FakeVercel::default(), Harness::new());
        let cli = FakeCli::failing("Error: build failed");
        let failure = execute(&test_inputs(), &test_push_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(messages(failure), ["Error: build failed"]);
        assert!(h.log().contains("::warning::Could not set GitHub deployment status to \"failure\": status_failure failed"));
    }

    #[tokio::test]
    async fn post_deploy_failure_keeps_success_status_and_outputs() {
        let gh = FakeGitHub::failing(&["create_comment"]);
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());
        let failure = execute(&test_inputs(), &test_pr_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(messages(failure), ["create_comment failed"]);
        let calls = gh.calls();
        assert!(calls.iter().any(|c| c.starts_with("status_success")));
        assert!(!calls.iter().any(|c| c.starts_with("status_failure")));
        assert!(calls.iter().any(|c| c.starts_with("add_labels")));
        assert_eq!(h.output("DEPLOYMENT_CREATED").as_deref(), Some("true"));
    }

    #[tokio::test]
    async fn every_post_deploy_failure_is_reported() {
        let gh = FakeGitHub::failing(&["status_success", "create_comment", "add_labels"]);
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());
        let failure = execute(&test_inputs(), &test_pr_context(), &gh, &vercel, &cli, &h)
            .await
            .unwrap_err();
        assert_eq!(
            messages(failure),
            [
                "status_success failed",
                "create_comment failed",
                "add_labels failed"
            ]
        );
    }

    #[tokio::test]
    async fn long_preview_alias_is_truncated_with_warning() {
        let mut inputs = test_inputs();
        inputs.pr_preview_domain = Some("{BRANCH}.vercel.app".into());
        let mut ctx = test_pr_context();
        ctx.branch = format!("feature/{}", "a".repeat(60));
        let (gh, vercel, cli, h) = (
            FakeGitHub::default(),
            FakeVercel::default(),
            FakeCli::ok(),
            Harness::new(),
        );
        execute(&inputs, &ctx, &gh, &vercel, &cli, &h)
            .await
            .unwrap();
        let vars = aliases::TemplateVars {
            user: "octo",
            repository: "repo",
            branch: &ctx.branch,
            sha: &ctx.sha,
            pr_number: Some("7"),
        };
        let expected = aliases::preview_alias("{BRANCH}.vercel.app", &vars);
        assert_eq!(
            vercel.aliases(),
            [format!("assign_alias dpl_1 {}", expected.alias)]
        );
        assert!(h.log().contains(&format!(
            "::warning::The alias {} exceeds 60 chars in length",
            expected.truncated_from.unwrap()
        )));
        assert!(
            h.log()
                .contains(&format!("Updated domain alias: {}", expected.alias))
        );
    }

    #[tokio::test]
    async fn trimmed_message_and_anonymous_author_in_meta() {
        let mut ctx = test_push_context();
        ctx.trim_commit_message = true;
        let gh = FakeGitHub {
            anonymous_author: true,
            ..FakeGitHub::default()
        };
        let (vercel, cli, h) = (FakeVercel::default(), FakeCli::ok(), Harness::new());
        execute(&test_inputs(), &ctx, &gh, &vercel, &cli, &h)
            .await
            .unwrap();
        let argv = cli.argv().unwrap();
        assert!(argv.contains(&"githubCommitMessage=feat: add thing".to_string()));
        assert!(argv.contains(&"githubCommitAuthorLogin=".to_string()));
    }
}
