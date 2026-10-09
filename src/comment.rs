//! PR comment bodies, byte-identical to v1's dedented output (spec §5.6).

use crate::aliases::short_sha;

/// v1 finds its previous comment by this text; it must never change.
pub const DEPLOYED_MARKER: &str = "This pull request has been deployed to Vercel.";

pub fn fork_refusal(actor: &str, user: &str) -> String {
    format!(
        "\nRefusing to deploy this Pull Request to Vercel because it originates from @{actor}'s fork.\n\n**@{user}** To allow this behaviour set `DEPLOY_PR_FROM_FORK` to true ([more info](https://github.com/BetaHuhn/deploy-to-vercel-action#deploying-a-pr-made-from-a-fork-or-dependabot)).\n"
    )
}

pub fn deployed(sha: &str, preview_url: &str, inspector_url: &str, log_url: &str) -> String {
    let sha = short_sha(sha);
    format!(
        "\n{DEPLOYED_MARKER}\n\n<table>\n<tr>\n<td><strong>Latest commit:</strong></td>\n<td><code>{sha}</code></td>\n</tr>\n<tr>\n<td><strong>✅ Preview:</strong></td>\n<td><a href='{preview_url}'>{preview_url}</a></td>\n</tr>\n<tr>\n<td><strong>🔍 Inspect:</strong></td>\n<td><a href='{inspector_url}'>{inspector_url}</a></td>\n</tr>\n</table>\n\n[View Workflow Logs]({log_url})\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Golden {
        fork: Vec<ForkCase>,
        deployed: Vec<DeployedCase>,
    }

    #[derive(Deserialize)]
    struct ForkCase {
        actor: String,
        user: String,
        body: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DeployedCase {
        sha: String,
        preview_url: String,
        inspector_url: String,
        log_url: String,
        body: String,
    }

    fn golden() -> Golden {
        serde_json::from_str(include_str!("../tests/golden/comments.json")).unwrap()
    }

    #[test]
    fn fork_refusal_is_byte_identical_to_v1() {
        for case in golden().fork {
            assert_eq!(fork_refusal(&case.actor, &case.user), case.body);
        }
    }

    #[test]
    fn deployed_comment_is_byte_identical_to_v1() {
        for case in golden().deployed {
            assert_eq!(
                deployed(
                    &case.sha,
                    &case.preview_url,
                    &case.inspector_url,
                    &case.log_url
                ),
                case.body
            );
        }
    }

    #[test]
    fn deployed_comment_contains_marker() {
        assert!(deployed("abc", "u", "i", "l").contains(DEPLOYED_MARKER));
    }
}
