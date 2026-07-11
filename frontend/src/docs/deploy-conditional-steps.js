import {
  mountDoc,
  section,
  code,
  figure,
  cards,
  table,
  note,
} from "./shell.js";

const content = [
  section({
    eyebrow: "What it does",
    title: "Skip steps by condition",
    sub: "Gate any deploy step on the environment. <code>when</code> runs a step only if its condition holds; <code>skip_if</code> skips it when the condition holds. Both take one condition or a list of criteria.",
    html: `
      <div class="split">
        ${code(`[[recipies.web.deploy.steps]]
name    = "notify_slack"
run     = "scripts/notify.sh"
skip_if = "env.IN_CI == true"          # skip in CI

[[recipies.web.deploy.steps]]
name = "prod_release"
run  = "scripts/release.sh"
when = ["env.DEPLOY_ENV == prod",      # run only when
        "REGION == us-east-1"]         # ALL are true`)}
        <div>
          ${cards([
            { icon: "✅", title: "when — run if", body: "Runs the step only when <em>every</em> listed condition is true. Any false one skips it." },
            { icon: "⛔", title: "skip_if — skip if", body: "Skips the step when <em>any</em> listed condition is true. The inverse of <code>when</code>." },
            { icon: "🧮", title: "Multiple criteria", body: "Pass a list to either field to combine conditions — that's the “multiple criteria” case." },
          ])}
        </div>
      </div>`,
  }),

  section({
    eyebrow: "Live view",
    title: "Skipped steps in the deploy GUI",
    sub: "A skipped step is dimmed and labelled <b>skipped</b>, with the reason shown inline — and it counts as satisfied, so its dependents still run.",
    html: `${figure(
      "deploy-gui-conditional.png",
      "<code>notify_slack</code> skipped by <code>skip_if</code> and <code>smoke_test</code> skipped by an unmet <code>when</code>, while the rest of the graph runs to success.",
    )}`,
  }),

  section({
    eyebrow: "Conditions",
    title: "Condition grammar",
    sub: "Each condition is evaluated against the deploy's environment. The variable may carry an optional <code>env.</code> prefix.",
    html: `
      ${table(
        ["Form", "True when", "Example"],
        [
          ["<code>VAR == value</code>", "VAR equals value (string compare; value may be quoted)", "<code>DEPLOY_ENV == prod</code>"],
          ["<code>VAR != value</code>", "VAR does not equal value", "<code>REGION != eu-west-1</code>"],
          ["<code>VAR</code>", "VAR is truthy — set, non-empty, not <code>false</code>/<code>0</code>/<code>no</code>/<code>off</code>", "<code>IN_CI</code>"],
          ["<code>!VAR</code>", "VAR is <em>not</em> truthy", "<code>!DRY_RUN</code>"],
        ],
      )}
      ${note("An unset variable reads as empty. <code>env.IN_CI</code> and <code>IN_CI</code> are equivalent. Blank conditions are rejected up front, so a typo can't silently read as “run”.")}`,
  }),

  section({
    eyebrow: "Semantics",
    title: "How a skip is decided",
    sub: "You can use <code>when</code> and <code>skip_if</code> on the same step. A step is skipped if either rule says so.",
    html: `
      ${table(
        ["Rule", "Skips the step when…"],
        [
          ["<code>skip_if</code>", "<em>any</em> listed condition is true"],
          ["<code>when</code>", "<em>any</em> listed condition is false (all must hold to run)"],
        ],
      )}
      ${cards([
        { icon: "➡️", title: "Dependents still run", body: "A skipped step is treated as satisfied, so steps that <code>need</code> it proceed normally — the graph isn't blocked." },
        { icon: "🌍", title: "Full environment", body: "Conditions see the resolved environment: ambient vars, CI/git <code>CIABATTA_*</code>, <code>-e</code> flags, and anything sourced from an <code>env_file</code>." },
        { icon: "🔁", title: "Same across runners", body: "Skips are shown identically in plain <code>--no-tui</code> output, the TUI, and the <code>--gui</code> view." },
      ])}`,
  }),

  section({
    eyebrow: "Recipe",
    title: "A worked example",
    sub: "Skip developer notifications in CI, and only run the production release for the prod environment.",
    html: `${code(`[recipies.web.deploy]
env_file = ".env.{DEPLOY_ENV}"

[[recipies.web.deploy.steps]]
name = "build"
run  = "scripts/build.sh"

[[recipies.web.deploy.steps]]
name    = "notify_dev"
needs   = ["build"]
skip_if = "env.IN_CI == true"          # noisy in CI — skip there
run     = "scripts/notify.sh"

[[recipies.web.deploy.steps]]
name  = "prod_release"
needs = ["build"]
when  = "DEPLOY_ENV == prod"           # prod runs only
run   = "scripts/release.sh"`)}`,
  }),
].join("");

mountDoc({
  slug: "deploy-conditional-steps.html",
  eyebrow: "Deploy · Configuration",
  title: "Conditional steps",
  lede: "Skip deploy steps based on the environment with <code>when</code> and <code>skip_if</code> — single conditions or multiple criteria — evaluated against every variable the deploy can see.",
  contentHTML: content,
});
