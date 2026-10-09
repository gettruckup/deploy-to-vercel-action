//! Input parsing with the exact semantics of `action-input-parser` 1.2.38 (spec §4.1).

use std::collections::HashMap;

use serde::Serialize;

/// Source of environment variables: `ProcessEnv` in production, `MapEnv` in tests.
pub trait Env {
    fn var(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MapEnv(pub HashMap<String, String>);

impl MapEnv {
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InputError(pub String);

const BOOLEAN_ERROR: &str =
    "boolean input has to be one of `true | True | TRUE | false | False | FALSE`";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayInput {
    Absent,
    Disabled,
    Present(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct Inputs {
    pub github_token: String,
    pub vercel_token: String,
    pub vercel_org_id: String,
    pub vercel_project_id: String,
    pub production: bool,
    pub github_deployment: bool,
    pub create_comment: bool,
    pub delete_existing_comment: bool,
    pub attach_commit_metadata: bool,
    pub deploy_pr_from_fork: bool,
    pub pr_labels: Option<Vec<String>>,
    pub alias_domains: Option<Vec<String>>,
    pub pr_preview_domain: Option<String>,
    pub vercel_scope: Option<String>,
    pub github_repository: String,
    pub github_deployment_env: Option<String>,
    pub trim_commit_message: bool,
    pub working_directory: Option<String>,
    pub build_env: Option<Vec<String>>,
    pub prebuilt: bool,
    pub force: bool,
}

/// Parses every input in v1's order, so the first error reported matches v1.
pub fn parse_inputs(env: &dyn Env, event_is_pr: bool) -> Result<Inputs, InputError> {
    Ok(Inputs {
        github_token: required_string(env, &["GH_PAT", "GITHUB_TOKEN"])?,
        vercel_token: required_string(env, &["VERCEL_TOKEN"])?,
        vercel_org_id: required_string(env, &["VERCEL_ORG_ID"])?,
        vercel_project_id: required_string(env, &["VERCEL_PROJECT_ID"])?,
        production: boolean(env, "PRODUCTION", !event_is_pr)?,
        github_deployment: boolean(env, "GITHUB_DEPLOYMENT", true)?,
        create_comment: boolean(env, "CREATE_COMMENT", true)?,
        delete_existing_comment: boolean(env, "DELETE_EXISTING_COMMENT", true)?,
        attach_commit_metadata: boolean(env, "ATTACH_COMMIT_METADATA", true)?,
        deploy_pr_from_fork: boolean(env, "DEPLOY_PR_FROM_FORK", false)?,
        pr_labels: match array(env, "PR_LABELS", true) {
            ArrayInput::Absent => Some(vec!["deployed".to_string()]),
            ArrayInput::Disabled => None,
            ArrayInput::Present(labels) => Some(labels),
        },
        alias_domains: present(array(env, "ALIAS_DOMAINS", true)),
        pr_preview_domain: string(env, &["PR_PREVIEW_DOMAIN"]),
        vercel_scope: string(env, &["VERCEL_SCOPE"]),
        github_repository: required_string(env, &["GITHUB_REPOSITORY"])?,
        github_deployment_env: string(env, &["GITHUB_DEPLOYMENT_ENV"]),
        trim_commit_message: boolean(env, "TRIM_COMMIT_MESSAGE", false)?,
        working_directory: string(env, &["WORKING_DIRECTORY"]),
        build_env: present(array(env, "BUILD_ENV", false)),
        prebuilt: boolean(env, "PREBUILT", false)?,
        force: boolean(env, "FORCE", false)?,
    })
}

fn present(input: ArrayInput) -> Option<Vec<String>> {
    match input {
        ArrayInput::Present(values) => Some(values),
        ArrayInput::Absent | ArrayInput::Disabled => None,
    }
}

/// `INPUT_<KEY>` if non-empty, else plain env `<KEY>` if non-empty.
fn lookup_one(env: &dyn Env, key: &str) -> Option<String> {
    let input_key = format!("INPUT_{}", key.replace(' ', "_").to_uppercase());
    let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
    non_empty(env.var(&input_key)).or_else(|| non_empty(env.var(key)))
}

fn lookup(env: &dyn Env, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| lookup_one(env, key))
}

pub fn string(env: &dyn Env, keys: &[&str]) -> Option<String> {
    lookup(env, keys).map(|value| js_trim(&value).to_string())
}

pub fn required_string(env: &dyn Env, keys: &[&str]) -> Result<String, InputError> {
    string(env, keys).ok_or_else(|| {
        InputError(format!(
            "Input `{}` is required but was not provided.",
            keys.join(",")
        ))
    })
}

pub fn boolean(env: &dyn Env, key: &str, default: bool) -> Result<bool, InputError> {
    match lookup(env, &[key]).as_deref() {
        None => Ok(default),
        Some("true" | "True" | "TRUE") => Ok(true),
        Some("false" | "False" | "FALSE") => Ok(false),
        Some(_) => Err(InputError(BOOLEAN_ERROR.to_string())),
    }
}

pub fn array(env: &dyn Env, key: &str, disableable: bool) -> ArrayInput {
    match lookup(env, &[key]) {
        None => ArrayInput::Absent,
        Some(value) if disableable && value == "false" => ArrayInput::Disabled,
        Some(value) => ArrayInput::Present(parse_array(&value)),
    }
}

/// Splits on newlines and commas, trims entries, drops empty ones (F10: v1 kept whitespace-only entries as "").
pub fn parse_array(value: &str) -> Vec<String> {
    value
        .split(['\n', ','])
        .map(js_trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// JS `String.prototype.trim` whitespace (WhiteSpace + LineTerminator), which differs from `char::is_whitespace`.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{A}' | '\u{B}' | '\u{C}' | '\u{D}' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

pub fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// JS truthiness for optional strings: `""` counts as unset.
pub fn non_empty_str(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

#[cfg(test)]
#[allow(dead_code)] // first used by the Task 4+ tests
pub(crate) fn test_inputs() -> Inputs {
    Inputs {
        github_token: "gh-token".into(),
        vercel_token: "vercel-token".into(),
        vercel_org_id: "team_org".into(),
        vercel_project_id: "prj_1".into(),
        production: true,
        github_deployment: true,
        create_comment: true,
        delete_existing_comment: true,
        attach_commit_metadata: true,
        deploy_pr_from_fork: false,
        pr_labels: Some(vec!["deployed".into()]),
        alias_domains: None,
        pr_preview_domain: None,
        vercel_scope: None,
        github_repository: "octo/repo".into(),
        github_deployment_env: None,
        trim_commit_message: false,
        working_directory: None,
        build_env: None,
        prebuilt: false,
        force: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct GoldenCase {
        name: String,
        env: HashMap<String, String>,
        ok: Option<serde_json::Value>,
        err: Option<String>,
    }

    /// F10: v1 kept whitespace-only array entries as "", v2 drops them.
    fn apply_f10(expected: &mut serde_json::Value) {
        for key in ["PR_LABELS", "ALIAS_DOMAINS", "BUILD_ENV"] {
            if let Some(serde_json::Value::Array(items)) = expected.get_mut(key) {
                items.retain(|item| item.as_str() != Some(""));
            }
        }
    }

    #[test]
    fn matches_v1_golden_vectors() {
        let cases: Vec<GoldenCase> =
            serde_json::from_str(include_str!("../tests/golden/inputs.json")).unwrap();
        assert!(cases.len() >= 30, "expected the full golden set");
        for case in cases {
            let actual = parse_inputs(&MapEnv(case.env), false);
            match (case.ok, case.err) {
                (Some(mut expected), None) => {
                    apply_f10(&mut expected);
                    let inputs =
                        actual.unwrap_or_else(|e| panic!("{}: unexpected error {e}", case.name));
                    assert_eq!(
                        serde_json::to_value(&inputs).unwrap(),
                        expected,
                        "{}",
                        case.name
                    );
                }
                (None, Some(message)) => {
                    assert_eq!(actual.unwrap_err().0, message, "{}", case.name)
                }
                _ => panic!("{}: malformed golden case", case.name),
            }
        }
    }

    #[test]
    fn production_default_follows_event() {
        let env = MapEnv::from_pairs([
            ("INPUT_GITHUB_TOKEN", "g"),
            ("INPUT_VERCEL_TOKEN", "v"),
            ("INPUT_VERCEL_ORG_ID", "o"),
            ("INPUT_VERCEL_PROJECT_ID", "p"),
            ("GITHUB_REPOSITORY", "octo/repo"),
        ]);
        assert!(parse_inputs(&env, false).unwrap().production);
        assert!(!parse_inputs(&env, true).unwrap().production);
    }

    #[test]
    fn parse_array_drops_whitespace_only_entries() {
        assert_eq!(parse_array("a,\n  \n b ,,c\n"), vec!["a", "b", "c"]);
        assert_eq!(parse_array("\n"), Vec::<String>::new());
    }

    #[test]
    fn js_trim_matches_javascript_whitespace() {
        assert_eq!(js_trim("\u{FEFF}\u{A0} x \u{2028}\t"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x"); // NEL is not JS whitespace
    }

    #[test]
    fn non_empty_str_treats_empty_as_unset() {
        assert_eq!(non_empty_str(&Some(String::new())), None);
        assert_eq!(non_empty_str(&Some("x".into())), Some("x"));
        assert_eq!(non_empty_str(&None), None);
    }
}
