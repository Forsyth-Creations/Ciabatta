//! `kind: assume_role` — a step that takes on a cloud identity for the steps
//! after it.
//!
//! ```yaml
//! - name: aws-dev
//!   kind: assume_role
//!   role: arn:aws:iam::${PADUA_ACCOUNT_ID}:role/OrganizationAccountAccessRole
//! - name: terraform
//!   needs: [build, aws-dev]
//!   run: terraform apply
//! ```
//!
//! The step calls `aws sts assume-role` with whatever credentials the run
//! already has, and every step that names it *directly* in `needs` runs with
//! the short-lived credentials that came back. Nothing else sees them: not the
//! rest of the graph, not the cache (they are never fingerprinted or stored),
//! not the log.
//!
//! When the ambient credentials are already in the role's account — a CI job
//! that assumed a role of its own before calling ciabatta, a local session
//! inside that account — the step passes them through untouched. That is what
//! keeps one workflow usable both ways: run whole from one identity that hops
//! between accounts, or one slice at a time under a role per slice.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::RunStep;
use crate::registry::LogSink;

/// The kinds that select this action. Both spellings, as `release-push` is
/// hyphenated and `assume_role` reads like the API it calls.
pub fn is_kind(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("assume_role") || kind.eq_ignore_ascii_case("assume-role")
}

/// What `aws sts assume-role` accepts for a session's length: 15 minutes to
/// 12 hours. A role assumed *from* an assumed role is held to one hour by AWS
/// whatever is asked for, which is why one hour is also the default.
const MIN_SESSION: Duration = Duration::from_secs(15 * 60);
const MAX_SESSION: Duration = Duration::from_secs(12 * 60 * 60);
const DEFAULT_SESSION: Duration = Duration::from_secs(60 * 60);

/// The variables a step behind an `assume_role` step is handed.
pub const CREDENTIAL_VARS: [&str; 3] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

/// Check an `assume_role` step's own fields, at compile time.
pub fn validate(step: &RunStep, run_name: &str) -> Result<()> {
    let Some(role) = step
        .role
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    else {
        bail!(
            "Step '{}' in run '{}' is `kind: assume_role` but has no `role:`. Name the role to \
             assume, e.g. `role: arn:aws:iam::123456789012:role/Deploy` (${{VAR}} references \
             are filled in from the run's environment).",
            step.name,
            run_name
        );
    };
    if step.run.is_some() || step.script.is_some() {
        bail!(
            "Step '{}' in run '{}' is `kind: assume_role` and also has a `run`/`script`. The step's \
             action is the assume itself; put the command in a step that `needs` this one.",
            step.name,
            run_name
        );
    }
    // A role with no ${VAR} in it can be checked now rather than at run time.
    if !role.contains('$') {
        account_of(role).with_context(|| format!("step '{}' in run '{}'", step.name, run_name))?;
    }
    if let Some(raw) = step.session_duration.as_deref() {
        session_length(raw)
            .with_context(|| format!("step '{}' in run '{}'", step.name, run_name))?;
    }
    Ok(())
}

/// A `role:` on anything but an `assume_role` step would do nothing, and
/// silently doing nothing with a credential setting is how a deploy ends up in
/// the wrong account.
pub fn validate_stray(step: &RunStep, run_name: &str) -> Result<()> {
    if step.is_assume_role() {
        return Ok(());
    }
    if step.role.is_some() || step.session_duration.is_some() {
        bail!(
            "Step '{}' in run '{}' sets `role`/`session_duration`, which only an \
             `kind: assume_role` step reads. Move them to a step of that kind and add it to this \
             step's `needs`.",
            step.name,
            run_name
        );
    }
    Ok(())
}

/// Two identities for one step is a question with no right answer, so it is
/// refused rather than settled by whichever finished last.
pub fn validate_needs(steps: &[RunStep], run_name: &str) -> Result<()> {
    let assumes: std::collections::HashSet<&str> = steps
        .iter()
        .filter(|s| s.is_assume_role())
        .map(|s| s.name.as_str())
        .collect();
    for step in steps {
        let named: Vec<&str> = step
            .needs
            .iter()
            .map(String::as_str)
            .filter(|n| assumes.contains(n))
            .collect();
        if named.len() > 1 {
            bail!(
                "Step '{}' in run '{}' needs more than one `assume_role` step ({}). A step runs as \
                 one identity; split it, or pick the role it actually needs.",
                step.name,
                run_name,
                named.join(", ")
            );
        }
    }
    Ok(())
}

/// The account id in a role ARN (`arn:<partition>:iam::<account>:role/<name>`).
fn account_of(role: &str) -> Result<&str> {
    let parts: Vec<&str> = role.splitn(6, ':').collect();
    let ok = parts.len() == 6
        && parts[0] == "arn"
        && parts[2] == "iam"
        && parts[4].len() == 12
        && parts[4].bytes().all(|b| b.is_ascii_digit())
        && parts[5].starts_with("role/");
    if !ok {
        bail!(
            "`{role}` is not an IAM role ARN (arn:aws:iam::<12-digit account>:role/<name>). If it \
             is built from ${{VARS}}, one of them is probably unset or wrong."
        );
    }
    Ok(parts[4])
}

fn session_length(raw: &str) -> Result<Duration> {
    let d = super::parse_duration(raw).with_context(|| format!("`session_duration: {raw}`"))?;
    if d < MIN_SESSION || d > MAX_SESSION {
        bail!("`session_duration: {raw}` must be between 15m and 12h");
    }
    Ok(d)
}

/// Fill `${VAR}` and `${VAR:-default}` in a role from the step's environment.
/// An unset variable is an error: a role with a hole in it names some other
/// account, or none.
fn expand(template: &str, env: &HashMap<String, String>) -> Result<String> {
    let re = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}").unwrap();
    let mut missing: Vec<String> = Vec::new();
    let out = re.replace_all(template, |caps: &regex::Captures| {
        let name = &caps[1];
        match (env.get(name).filter(|v| !v.is_empty()), caps.get(2)) {
            (Some(v), _) => v.clone(),
            (None, Some(default)) => default.as_str().to_string(),
            (None, None) => {
                missing.push(name.to_string());
                String::new()
            }
        }
    });
    if !missing.is_empty() {
        bail!(
            "`role: {template}` uses {} but the run's environment doesn't set {}",
            missing.join(", "),
            if missing.len() == 1 { "it" } else { "them" }
        );
    }
    Ok(out.into_owned())
}

/// A session name AWS accepts (`[\w+=,.@-]`, 2–64 chars) that says who asked.
fn session_name(workflow: &str, step: &str) -> String {
    let raw = format!("ciabatta-{workflow}-{step}");
    let mut name: String = raw
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() => c,
            '+' | '=' | ',' | '.' | '@' | '-' | '_' => c,
            _ => '-',
        })
        .collect();
    name.truncate(64);
    name
}

/// Run `aws` with the step's environment and give back its stdout. Its output
/// is never sent to the log: for `assume-role` it *is* the credentials.
async fn aws(args: &[&str], env: &HashMap<String, String>) -> Result<String> {
    let output = tokio::process::Command::new("aws")
        .args(args)
        .envs(env)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .context("could not run the `aws` CLI — is it installed and on PATH?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`aws {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// What an `assume_role` step hands the steps that need it. Empty when the
/// ambient credentials were already in the right account.
pub type Provided = HashMap<String, String>;

/// The `assume_role` action.
pub async fn run(
    step: &RunStep,
    workflow: &str,
    env: &HashMap<String, String>,
    dry_run: bool,
    sink: &mut LogSink<'_>,
) -> Result<Provided> {
    let template = step.role.as_deref().unwrap_or_default().trim();
    let role = expand(template, env)?;
    let account = account_of(&role)?.to_string();
    let length = match step.session_duration.as_deref() {
        Some(raw) => session_length(raw)?,
        None => DEFAULT_SESSION,
    };

    if dry_run {
        sink.push(format!("[dry-run] would assume {role}"));
        return Ok(Provided::new());
    }

    let current = aws(
        &[
            "sts",
            "get-caller-identity",
            "--query",
            "Account",
            "--output",
            "text",
        ],
        env,
    )
    .await
    .context("checking which account the current credentials are in")?;
    if current == account {
        sink.push(format!(
            "already in account {account}; the steps that need this one keep the current \
             credentials"
        ));
        return Ok(Provided::new());
    }

    let name = session_name(workflow, &step.name);
    let seconds = length.as_secs().to_string();
    sink.push(format!(
        "assuming {role} from account {current} (session {name})"
    ));
    let out = aws(
        &[
            "sts",
            "assume-role",
            "--role-arn",
            &role,
            "--role-session-name",
            &name,
            "--duration-seconds",
            &seconds,
            "--query",
            "Credentials.[AccessKeyId,SecretAccessKey,SessionToken,Expiration]",
            "--output",
            "text",
        ],
        env,
    )
    .await
    .with_context(|| format!("assuming {role}"))?;

    let fields: Vec<&str> = out.split_whitespace().collect();
    let [key, secret, token, expires] = fields[..] else {
        bail!("`aws sts assume-role` answered in a shape ciabatta doesn't recognise");
    };
    sink.push(format!("assumed {role}; credentials expire {expires}"));

    Ok(CREDENTIAL_VARS
        .iter()
        .zip([key, secret, token])
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

/// The environment `step` runs with: the run's, plus the credentials of the
/// `assume_role` step it needs, if any produced some.
pub fn env_for<'a>(
    step: &RunStep,
    base: &'a HashMap<String, String>,
    provided: &HashMap<String, Provided>,
) -> std::borrow::Cow<'a, HashMap<String, String>> {
    let assumed = step
        .needs
        .iter()
        .filter_map(|need| provided.get(need))
        .find(|vars| !vars.is_empty());
    match assumed {
        None => std::borrow::Cow::Borrowed(base),
        Some(vars) => {
            let mut env = base.clone();
            env.extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));
            std::borrow::Cow::Owned(env)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> RunStep {
        RunStep {
            name: name.into(),
            ..Default::default()
        }
    }

    fn assume(name: &str, role: &str) -> RunStep {
        RunStep {
            name: name.into(),
            kind: Some("assume_role".into()),
            role: Some(role.into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_role_arn_yields_its_account() {
        assert_eq!(
            account_of("arn:aws:iam::449009850031:role/OrganizationAccountAccessRole").unwrap(),
            "449009850031"
        );
        assert!(account_of("arn:aws:iam:::role/X").is_err());
        assert!(account_of("arn:aws:iam::bash:role/X").is_err());
        assert!(account_of("arn:aws:iam::449009850031:user/X").is_err());
    }

    #[test]
    fn a_role_is_filled_in_from_the_environment_and_a_hole_is_refused() {
        let env = HashMap::from([("ACCT".to_string(), "449009850031".to_string())]);
        assert_eq!(
            expand("arn:aws:iam::${ACCT}:role/${ROLE:-Deploy}", &env).unwrap(),
            "arn:aws:iam::449009850031:role/Deploy"
        );
        let err = expand("arn:aws:iam::${NOPE}:role/X", &env).unwrap_err();
        assert!(err.to_string().contains("NOPE"));
    }

    #[test]
    fn session_names_are_ones_aws_accepts() {
        let name = session_name("deploy", "aws images/dev");
        assert_eq!(name, "ciabatta-deploy-aws-images-dev");
        assert!(session_name(&"w".repeat(80), "s").len() <= 64);
    }

    #[test]
    fn an_assume_step_needs_a_role_and_no_command_of_its_own() {
        assert!(validate(&assume("a", "arn:aws:iam::449009850031:role/R"), "w").is_ok());
        assert!(validate(&assume("a", "arn:aws:iam::${ACCT}:role/R"), "w").is_ok());
        assert!(validate(&assume("a", "  "), "w").is_err());
        assert!(validate(&assume("a", "not-an-arn"), "w").is_err());
        let mut with_run = assume("a", "arn:aws:iam::449009850031:role/R");
        with_run.run = Some("echo hi".into());
        assert!(validate(&with_run, "w").is_err());
        let mut long = assume("a", "arn:aws:iam::449009850031:role/R");
        long.session_duration = Some("13h".into());
        assert!(validate(&long, "w").is_err());
    }

    #[test]
    fn a_role_on_any_other_step_is_refused() {
        let mut plain = step("build");
        plain.role = Some("arn:aws:iam::449009850031:role/R".into());
        assert!(validate_stray(&plain, "w").is_err());
        assert!(validate_stray(&step("build"), "w").is_ok());
    }

    #[test]
    fn a_step_may_need_only_one_identity() {
        let mut deploy = step("deploy");
        deploy.needs = vec!["dev".into(), "images".into()];
        let steps = vec![
            assume("dev", "arn:aws:iam::449009850031:role/R"),
            assume("images", "arn:aws:iam::794075228133:role/R"),
            deploy,
        ];
        assert!(validate_needs(&steps, "w").is_err());
    }

    #[test]
    fn only_a_step_that_needs_the_assume_step_gets_its_credentials() {
        let base = HashMap::from([("AWS_ACCESS_KEY_ID".to_string(), "ambient".to_string())]);
        let provided = HashMap::from([
            (
                "dev".to_string(),
                HashMap::from([("AWS_ACCESS_KEY_ID".to_string(), "assumed".to_string())]),
            ),
            // Already in the account: nothing to hand over.
            ("same".to_string(), Provided::new()),
        ]);

        let mut behind = step("terraform");
        behind.needs = vec!["build".into(), "dev".into()];
        assert_eq!(
            env_for(&behind, &base, &provided)["AWS_ACCESS_KEY_ID"],
            "assumed"
        );

        let mut elsewhere = step("lint");
        elsewhere.needs = vec!["build".into()];
        assert!(matches!(
            env_for(&elsewhere, &base, &provided),
            std::borrow::Cow::Borrowed(_)
        ));

        let mut passthrough = step("push");
        passthrough.needs = vec!["same".into()];
        assert_eq!(
            env_for(&passthrough, &base, &provided)["AWS_ACCESS_KEY_ID"],
            "ambient"
        );
    }
}
