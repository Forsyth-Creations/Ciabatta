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
    title: "Load a .env file before the deploy runs",
    sub: "Point a deploy recipe at one or more <code>.env</code> files with <code>env_file</code>. Every <code>KEY=VALUE</code> line is sourced into the environment before any phase or step executes, so your scripts see them without hand-exporting anything.",
    html: `
      <div class="split">
        ${code(`# .ciabatta/ciabatta.toml
[recipies.web.deploy]
env_file = ".env"                 # one file…
# env_file = [".env", ".env.deploy"]   # …or a list

[[recipies.web.deploy.steps]]
name = "migrate_db"
run  = "echo migrating $DB_URL"   # DB_URL comes from .env`)}
        <div>
          ${cards([
            { icon: "📄", title: "One file or many", body: "<code>env_file</code> takes a single path or a list. Files are sourced left to right; a later file overrides an earlier one." },
            { icon: "👁", title: "Seen everywhere", body: "Sourced values reach every phase (<code>login → pre → deploy → post</code>) and every step's <code>script</code> / <code>run</code>." },
            { icon: "🧩", title: "Also on the flowchart", body: "Set <code>env_file</code> on the flowchart entry too; recipe-level files layer on top of it." },
          ])}
        </div>
      </div>`,
  }),

  section({
    eyebrow: "Live view",
    title: "See it in the deploy GUI",
    sub: "Run <code>ciabatta deploy web --gui</code> and the sourced file is announced in the log stream, with its values visibly flowing into your step output.",
    html: `${figure(
      "deploy-gui-conditional.png",
      "The deploy GUI: <code>Sourcing env file: .env.dev</code> in the log panel, and <code>DB_URL</code> resolved into the migrate step's output.",
    )}`,
  }),

  section({
    eyebrow: "Precedence",
    title: "What wins when values collide",
    sub: "A <code>.env</code> file only fills in what isn't already set, so it never clobbers a value you passed deliberately.",
    html: `
      ${table(
        ["Source", "Beats a .env value?", "Notes"],
        [
          ["<code>-e KEY=VALUE</code> flag", "<span class='yes'>✓ wins</span>", "Explicit CLI overrides always take priority."],
          ["CI / git-resolved <code>CIABATTA_*</code>", "<span class='yes'>✓ wins</span>", "Anything already resolved for the run is kept."],
          ["Ambient environment", "<span class='yes'>✓ wins</span>", "A non-empty exported var is left untouched."],
          ["Later <code>env_file</code> in the list", "<span class='yes'>✓</span> over earlier", "Files apply in order; the last one wins among files."],
          ["Earlier <code>env_file</code> in the list", "—", "Provides a value only if nothing above set it."],
        ],
      )}
      ${note(
        "In short: a <code>.env</code> supplies <em>defaults</em>. Explicit <code>-e</code> flags, CI, and the ambient environment always win over it.",
      )}`,
  }),

  section({
    eyebrow: "Works with REQUIRED_ENV",
    title: "A sourced value can satisfy a requirement",
    sub: "Because files are sourced <em>before</em> the <code>REQUIRED_ENV</code> gate is checked, a variable provided by a <code>.env</code> counts as present.",
    html: `
      ${code(`[recipies.web.deploy]
env_file     = ".env"
REQUIRED_ENV = ["DB_URL", "DEPLOY_TOKEN"]   # both may come from .env`)}
      ${note(
        "If a required variable is still empty or unset after sourcing, the deploy aborts before running a single step and names what's missing. A referenced <code>env_file</code> that doesn't exist is a hard error too — no silent skip.",
      )}`,
  }),

  section({
    eyebrow: "Format",
    title: "What the file may contain",
    sub: "The parser handles the common <code>.env</code> shape.",
    html: `
      <div class="split">
        ${code(`# comments and blank lines are ignored
export DB_URL=postgres://db:5432/app   # 'export' is optional
API_SECRET="quoted value"              # quotes are stripped
REGION='us-east-1'
FEATURE_FLAG=`)}
        <div>
          ${cards([
            { icon: "#️⃣", title: "Comments & blanks", body: "Lines starting with <code>#</code> and empty lines are skipped." },
            { icon: "📦", title: "export prefix", body: "A leading <code>export </code> is accepted and ignored, so a shell-style file works as-is." },
            { icon: "❝", title: "Quotes", body: "Single or double quotes around a value are stripped; the rest is taken verbatim (trimmed)." },
          ])}
        </div>
      </div>`,
  }),
].join("");

mountDoc({
  slug: "deploy-env-files.html",
  eyebrow: "Deploy · Configuration",
  title: "Sourcing .env files",
  lede: "Load <code>KEY=VALUE</code> files into a deploy before it runs — with clear precedence, so a file provides defaults without ever overriding what you set on purpose.",
  contentHTML: content,
});
