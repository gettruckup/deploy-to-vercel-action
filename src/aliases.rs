//! Alias templating, URL-safe placeholders and Vercel's 60-character preview truncation (v1 rules, spec §5.5).

use sha2::{Digest, Sha256};

pub const PREVIEW_DOMAIN_SUFFIX: &str = ".vercel.app";

pub struct TemplateVars<'a> {
    pub user: &'a str,
    pub repository: &'a str,
    pub branch: &'a str,
    pub sha: &'a str,
    pub pr_number: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewAlias {
    pub alias: String,
    /// The original prefix when the alias was truncated (used for the v1 warning text).
    pub truncated_from: Option<String>,
}

/// Replaces characters outside `[A-Za-z0-9_~]` with `-`, once per UTF-16 unit like the v1 JS regex.
pub fn url_safe(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '~' {
            out.push(c);
        } else {
            out.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    out
}

pub fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// F3: every occurrence is replaced (v1 replaced only the first).
fn render(template: &str, vars: &TemplateVars, with_pr: bool) -> String {
    let mut rendered = template
        .replace("{USER}", &url_safe(vars.user))
        .replace("{REPO}", &url_safe(vars.repository))
        .replace("{BRANCH}", &url_safe(vars.branch));
    if with_pr {
        rendered = rendered.replace("{PR}", vars.pr_number.unwrap_or_default());
    }
    rendered
        .replace("{SHA}", &short_sha(vars.sha))
        .to_lowercase()
}

pub fn preview_alias(template: &str, vars: &TemplateVars) -> PreviewAlias {
    let alias = render(template, vars, true);
    if !alias.ends_with(PREVIEW_DOMAIN_SUFFIX) {
        return PreviewAlias {
            alias,
            truncated_from: None,
        };
    }
    let prefix = &alias[..alias.find(PREVIEW_DOMAIN_SUFFIX).unwrap_or(0)];
    if utf16_len(prefix) < 60 {
        return PreviewAlias {
            alias,
            truncated_from: None,
        };
    }
    let unique_suffix = &sha256_hex(&format!("git-{}-{}", vars.branch, vars.repository))[..6];
    PreviewAlias {
        alias: format!(
            "{}-{unique_suffix}{PREVIEW_DOMAIN_SUFFIX}",
            utf16_prefix(prefix, 55)
        ),
        truncated_from: Some(prefix.to_string()),
    }
}

/// `ALIAS_DOMAINS` entries: same placeholders except `{PR}`, no truncation (v1 behavior).
pub fn domain_alias(template: &str, vars: &TemplateVars) -> String {
    render(template, vars, false)
}

pub fn add_schema(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

pub fn remove_schema(url: &str) -> String {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
        .to_string()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn utf16_prefix(value: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, c) in value.char_indices() {
        if units + c.len_utf16() > max_units {
            return &value[..index];
        }
        units += c.len_utf16();
    }
    value
}

fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Golden {
        #[serde(rename = "urlSafe")]
        url_safe: Vec<UrlSafeCase>,
        schema: Vec<SchemaCase>,
        preview: Vec<AliasCase>,
        domain: Vec<AliasCase>,
    }

    #[derive(Deserialize)]
    struct UrlSafeCase {
        input: String,
        output: String,
    }

    #[derive(Deserialize)]
    struct SchemaCase {
        input: String,
        add: String,
        remove: String,
    }

    #[derive(Deserialize)]
    struct AliasCase {
        template: String,
        #[serde(rename = "USER")]
        user: String,
        #[serde(rename = "REPOSITORY")]
        repository: String,
        #[serde(rename = "BRANCH")]
        branch: String,
        #[serde(rename = "PR_NUMBER")]
        pr_number: String,
        #[serde(rename = "SHA")]
        sha: String,
        alias: String,
        #[serde(rename = "truncatedFrom", default)]
        truncated_from: Option<String>,
    }

    impl AliasCase {
        fn vars(&self) -> TemplateVars<'_> {
            TemplateVars {
                user: &self.user,
                repository: &self.repository,
                branch: &self.branch,
                sha: &self.sha,
                pr_number: Some(&self.pr_number),
            }
        }
    }

    fn golden() -> Golden {
        serde_json::from_str(include_str!("../tests/golden/aliases.json")).unwrap()
    }

    #[test]
    fn url_safe_matches_v1() {
        for case in golden().url_safe {
            assert_eq!(url_safe(&case.input), case.output, "{:?}", case.input);
        }
    }

    #[test]
    fn schema_helpers_match_v1() {
        for case in golden().schema {
            assert_eq!(add_schema(&case.input), case.add, "{:?}", case.input);
            assert_eq!(remove_schema(&case.input), case.remove, "{:?}", case.input);
        }
    }

    #[test]
    fn preview_aliases_match_v1() {
        for case in golden().preview {
            let actual = preview_alias(&case.template, &case.vars());
            assert_eq!(actual.alias, case.alias, "{}", case.template);
            assert_eq!(
                actual.truncated_from, case.truncated_from,
                "{}",
                case.template
            );
        }
    }

    #[test]
    fn domain_aliases_match_v1() {
        for case in golden().domain {
            assert_eq!(
                domain_alias(&case.template, &case.vars()),
                case.alias,
                "{}",
                case.template
            );
        }
    }

    #[test]
    fn every_placeholder_occurrence_is_replaced() {
        let vars = TemplateVars {
            user: "octo",
            repository: "repo",
            branch: "main",
            sha: "abcdef0123",
            pr_number: Some("7"),
        };
        assert_eq!(
            domain_alias("{BRANCH}-{BRANCH}.{USER}.{USER}.example.com", &vars),
            "main-main.octo.octo.example.com"
        );
        assert_eq!(
            preview_alias("pr{PR}-{PR}-{SHA}{SHA}.vercel.app", &vars).alias,
            "pr7-7-abcdef0abcdef0.vercel.app"
        );
    }

    #[test]
    fn missing_pr_number_renders_empty() {
        let vars = TemplateVars {
            user: "octo",
            repository: "repo",
            branch: "main",
            sha: "abc",
            pr_number: None,
        };
        assert_eq!(
            preview_alias("pr{PR}.example.com", &vars).alias,
            "pr.example.com"
        );
    }
}
