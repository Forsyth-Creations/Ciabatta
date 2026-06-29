use std::collections::HashMap;
use std::env;

/// Supported CI/CD systems.
#[derive(Debug, Clone, PartialEq)]
pub enum CiSystem {
    Gitlab,
    Github,
    Jenkins,
    CircleCi,
    Travis,
    AzureDevOps,
    Bitbucket,
    Unknown(String),
}

impl From<&str> for CiSystem {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "gitlab" => CiSystem::Gitlab,
            "github" => CiSystem::Github,
            "jenkins" => CiSystem::Jenkins,
            "circleci" | "circle" => CiSystem::CircleCi,
            "travis" => CiSystem::Travis,
            "azure" | "azuredevops" | "azure_devops" => CiSystem::AzureDevOps,
            "bitbucket" => CiSystem::Bitbucket,
            other => CiSystem::Unknown(other.to_string()),
        }
    }
}

impl std::fmt::Display for CiSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CiSystem::Gitlab => write!(f, "GitLab CI"),
            CiSystem::Github => write!(f, "GitHub Actions"),
            CiSystem::Jenkins => write!(f, "Jenkins"),
            CiSystem::CircleCi => write!(f, "CircleCI"),
            CiSystem::Travis => write!(f, "Travis CI"),
            CiSystem::AzureDevOps => write!(f, "Azure DevOps"),
            CiSystem::Bitbucket => write!(f, "Bitbucket Pipelines"),
            CiSystem::Unknown(name) => write!(f, "Unknown CI ({})", name),
        }
    }
}

/// Resolve CIABATTA_* variables from the current CI environment.
/// Returns the variables that were found, and a list of what was resolved.
pub fn resolve_ci_vars(system: &CiSystem) -> (HashMap<String, String>, Vec<ResolvedVar>) {
    let mut vars = HashMap::new();
    let mut resolved = Vec::new();

    let mappings = ci_mappings(system);
    for (ciabatta_key, source_keys) in &mappings {
        for source_key in source_keys {
            if let Ok(value) = env::var(source_key)
                && !value.is_empty()
            {
                resolved.push(ResolvedVar {
                    ciabatta_name: ciabatta_key.clone(),
                    source_name: source_key.clone(),
                    value: value.clone(),
                });
                vars.insert(ciabatta_key.clone(), value);
                break; // use first match
            }
        }
    }

    (vars, resolved)
}

pub struct ResolvedVar {
    pub ciabatta_name: String,
    pub source_name: String,
    pub value: String,
}

/// Per-system environment variable mappings.
/// Each entry: (CIABATTA_VAR_NAME, [candidate source env vars in priority order])
fn ci_mappings(system: &CiSystem) -> Vec<(String, Vec<String>)> {
    let branch_key = "CIABATTA_BRANCH".to_string();
    let commit_key = "CIABATTA_COMMIT".to_string();
    let tag_key = "CIABATTA_TAG".to_string();
    let build_key = "CIABATTA_BUILD_NUMBER".to_string();

    match system {
        CiSystem::Gitlab => vec![
            (
                branch_key,
                vec!["CI_COMMIT_BRANCH".into(), "CI_COMMIT_REF_NAME".into()],
            ),
            (commit_key, vec!["CI_COMMIT_SHA".into()]),
            (tag_key, vec!["CI_COMMIT_TAG".into()]),
            (
                build_key,
                vec!["CI_PIPELINE_IID".into(), "CI_JOB_ID".into()],
            ),
        ],
        CiSystem::Github => vec![
            (
                branch_key,
                vec!["GITHUB_REF_NAME".into(), "GITHUB_HEAD_REF".into()],
            ),
            (commit_key, vec!["GITHUB_SHA".into()]),
            (tag_key, vec!["GITHUB_REF_NAME".into()]), // only meaningful when triggered by tag
            (build_key, vec!["GITHUB_RUN_NUMBER".into()]),
        ],
        CiSystem::Jenkins => vec![
            (branch_key, vec!["GIT_BRANCH".into(), "BRANCH_NAME".into()]),
            (commit_key, vec!["GIT_COMMIT".into()]),
            (tag_key, vec!["TAG_NAME".into(), "GIT_TAG_NAME".into()]),
            (build_key, vec!["BUILD_NUMBER".into()]),
        ],
        CiSystem::CircleCi => vec![
            (branch_key, vec!["CIRCLE_BRANCH".into()]),
            (commit_key, vec!["CIRCLE_SHA1".into()]),
            (tag_key, vec!["CIRCLE_TAG".into()]),
            (build_key, vec!["CIRCLE_BUILD_NUM".into()]),
        ],
        CiSystem::Travis => vec![
            (branch_key, vec!["TRAVIS_BRANCH".into()]),
            (commit_key, vec!["TRAVIS_COMMIT".into()]),
            (tag_key, vec!["TRAVIS_TAG".into()]),
            (build_key, vec!["TRAVIS_BUILD_NUMBER".into()]),
        ],
        CiSystem::AzureDevOps => vec![
            (
                branch_key,
                vec!["BUILD_SOURCEBRANCH".into(), "BUILD_SOURCEBRANCHNAME".into()],
            ),
            (commit_key, vec!["BUILD_SOURCEVERSION".into()]),
            (tag_key, vec!["BUILD_SOURCEBRANCH".into()]), // refs/tags/...
            (
                build_key,
                vec!["BUILD_BUILDNUMBER".into(), "BUILD_BUILDID".into()],
            ),
        ],
        CiSystem::Bitbucket => vec![
            (branch_key, vec!["BITBUCKET_BRANCH".into()]),
            (commit_key, vec!["BITBUCKET_COMMIT".into()]),
            (tag_key, vec!["BITBUCKET_TAG".into()]),
            (build_key, vec!["BITBUCKET_BUILD_NUMBER".into()]),
        ],
        CiSystem::Unknown(_) => vec![
            // Fall back to common generic env vars
            (
                branch_key,
                vec!["CI_BRANCH".into(), "BRANCH".into(), "GIT_BRANCH".into()],
            ),
            (
                commit_key,
                vec!["CI_COMMIT".into(), "GIT_COMMIT".into(), "COMMIT".into()],
            ),
            (
                tag_key,
                vec!["CI_TAG".into(), "GIT_TAG".into(), "TAG".into()],
            ),
            (
                build_key,
                vec!["CI_BUILD_NUMBER".into(), "BUILD_NUMBER".into()],
            ),
        ],
    }
}
