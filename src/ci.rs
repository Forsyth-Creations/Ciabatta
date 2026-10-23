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
            let value = match env::var(source_key) {
                Ok(value) if !value.is_empty() => value,
                _ => continue,
            };

            if !is_valid_ci_value(system, ciabatta_key, source_key, &value) {
                continue;
            }

            resolved.push(ResolvedVar {
                ciabatta_name: ciabatta_key.clone(),
                source_name: source_key.clone(),
                value: value.clone(),
            });
            vars.insert(ciabatta_key.clone(), value);
            break; // use first match
        }
    }

    (vars, resolved)
}

fn is_valid_ci_value(system: &CiSystem, ciabatta_key: &str, source_key: &str, value: &str) -> bool {
    if ciabatta_key != "CIABATTA_TAG" {
        return true;
    }

    match system {
        CiSystem::Github => match source_key {
            "GITHUB_REF_NAME" => env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag"),
            "GITHUB_REF" => value.starts_with("refs/tags/"),
            _ => true,
        },
        CiSystem::AzureDevOps => match source_key {
            "BUILD_SOURCEBRANCH" => value.starts_with("refs/tags/"),
            "BUILD_SOURCEBRANCHNAME" => env::var("BUILD_SOURCEBRANCH")
                .map(|ref_name| ref_name.starts_with("refs/tags/"))
                .unwrap_or(false),
            _ => true,
        },
        _ => true,
    }
}

#[derive(Debug)]
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
                vec!["GITHUB_HEAD_REF".into(), "GITHUB_REF_NAME".into()],
            ),
            (commit_key, vec!["GITHUB_SHA".into()]),
            (
                tag_key,
                vec!["GITHUB_REF_NAME".into(), "GITHUB_REF".into()],
            ),
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
            (tag_key, vec!["BUILD_SOURCEBRANCH".into(), "BUILD_SOURCEBRANCHNAME".into()]),
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{CiSystem, resolve_ci_vars};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_var(key: &str, value: &str) {
        unsafe { std::env::set_var(key, value) }
    }

    fn restore_var(key: &str, old: Option<String>) {
        unsafe {
            match old {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    fn with_env_vars<T>(vars: &[(&str, &str)], test: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();

        for (key, value) in vars {
            set_var(key, value);
        }

        struct Guard<'a>(Vec<(&'a str, Option<String>)>);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                for (key, old) in &self.0 {
                    restore_var(key, old.clone());
                }
            }
        }

        let _cleanup = Guard(previous);
        test()
    }

    #[test]
    fn github_branch_does_not_set_tag() {
        with_env_vars(
            &[
                ("GITHUB_HEAD_REF", "feature/test"),
                ("GITHUB_REF_NAME", "feature/test"),
                ("GITHUB_REF_TYPE", "branch"),
                ("GITHUB_REF", "refs/heads/feature/test"),
                ("GITHUB_SHA", "abc123"),
                ("GITHUB_RUN_NUMBER", "42"),
            ],
            || {
                let (vars, resolved) = resolve_ci_vars(&CiSystem::Github);
                assert!(resolved.iter().all(|r| r.ciabatta_name != "CIABATTA_TAG"));
                assert!(!vars.contains_key("CIABATTA_TAG"));
            },
        );
    }

    #[test]
    fn github_tag_sets_tag_only_when_ref_type_is_tag() {
        with_env_vars(
            &[
                ("GITHUB_REF_NAME", "v1.2.3"),
                ("GITHUB_REF_TYPE", "tag"),
                ("GITHUB_REF", "refs/tags/v1.2.3"),
                ("GITHUB_SHA", "def456"),
                ("GITHUB_RUN_NUMBER", "99"),
            ],
            || {
                let (vars, resolved) = resolve_ci_vars(&CiSystem::Github);
                assert!(resolved.iter().any(|r| r.ciabatta_name == "CIABATTA_TAG"));
                assert_eq!(vars.get("CIABATTA_TAG"), Some(&"v1.2.3".to_string()));
            },
        );
    }
}
