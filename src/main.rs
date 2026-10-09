//! Entry point: parse inputs, resolve the run context, then orchestrate the deployment.

use std::path::PathBuf;
use std::process::ExitCode;

use deploy_to_vercel::actions_io::Io;
use deploy_to_vercel::context::{self, RunContext};
use deploy_to_vercel::github::GitHubClient;
use deploy_to_vercel::http;
use deploy_to_vercel::inputs::{self, Env, Inputs, ProcessEnv, non_empty_str};
use deploy_to_vercel::run::{self, Deps};
use deploy_to_vercel::vercel::api::{VercelClient, team_param};
use deploy_to_vercel::vercel::cli::ProcessCli;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    // F13: a deployed repo's .env must not inject inputs in CI; it is only a local-run convenience.
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
        let _ = dotenvy::from_path(".env");
    }
    let io = Io::from_process_env();
    let env = ProcessEnv;

    let inputs = match inputs::parse_inputs(&env, context::event_is_pr(&env)) {
        Ok(inputs) => inputs,
        Err(err) => {
            io.error(&err.to_string());
            return ExitCode::FAILURE;
        }
    };
    io.mask(&inputs.github_token);
    io.mask(&inputs.vercel_token);
    io.info(&format!("deploy-to-vercel-action v{VERSION}"));

    let payload = context::load_event_payload(&env);
    let ctx = match context::resolve(&env, &inputs, payload.as_ref()) {
        Ok(ctx) => ctx,
        Err(err) => {
            io.error(&err.to_string());
            return ExitCode::FAILURE;
        }
    };
    io.debug(&context::debug_dump(&inputs, &ctx));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start tokio runtime");
    runtime.block_on(execute(&env, &inputs, &ctx, &io))
}

async fn execute(env: &ProcessEnv, inputs: &Inputs, ctx: &RunContext, io: &Io) -> ExitCode {
    let base_url = |key: &str, default: &str| {
        env.var(key)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let http = http::build_client(&format!("deploy-to-vercel-action/{VERSION}"));
    let github = GitHubClient::new(
        http.clone(),
        &base_url("GITHUB_API_URL", "https://api.github.com"),
        &inputs.github_token,
        &ctx.user,
        &ctx.repository,
        &ctx.log_url,
    );
    let vercel = VercelClient::new(
        http,
        &base_url("VERCEL_API_URL", "https://api.vercel.com"),
        &inputs.vercel_token,
        team_param(&inputs.vercel_org_id),
    );
    let cli = ProcessCli {
        program: "vercel".to_string(),
        cwd: non_empty_str(&inputs.working_directory).map(PathBuf::from),
        envs: vec![
            ("VERCEL_ORG_ID".to_string(), inputs.vercel_org_id.clone()),
            (
                "VERCEL_PROJECT_ID".to_string(),
                inputs.vercel_project_id.clone(),
            ),
        ],
        io,
    };
    let deps = Deps {
        github: &github,
        vercel: &vercel,
        cli: &cli,
        io,
    };
    match run::run(inputs, ctx, &deps).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(failure) => {
            for err in &failure.errors {
                io.error(&err.to_string());
            }
            ExitCode::FAILURE
        }
    }
}
