import { mountDoc, section, code, cards, table, note } from "./shell.js";

const content = [
  section({
    eyebrow: "What it does",
    title: "Choose the env file at run time",
    sub: "Put a <code>{VAR}</code> placeholder in the <code>env_file</code> path and Ciabatta fills it in from the environment before sourcing — so one recipe drives every environment.",
    html: `
      <div class="split">
        ${code(`[recipies.web.deploy]
env_file = ".env.{DEPLOY_ENV}"    # resolved per run

[[recipies.web.deploy.steps]]
name = "release"
run  = "echo shipping to $TARGET"`)}
        <div>
          ${cards([
            { icon: "🔀", title: "One recipe, many envs", body: "<code>.env.{DEPLOY_ENV}</code> becomes <code>.env.dev</code> or <code>.env.prod</code> depending on the run — no duplicated config." },
            { icon: "🔤", title: "Same {VAR} syntax", body: "Uses the exact placeholder syntax as <code>publish_path</code>. Uppercase names, an optional value from CI, git, or <code>-e</code>." },
            { icon: "🧱", title: "Combine with a base", body: "Layer a shared base under the per-env overlay: <code>[ \".env\", \".env.{DEPLOY_ENV}\" ]</code>." },
          ])}
        </div>
      </div>`,
  }),

  section({
    eyebrow: "Try it",
    title: "Flip environments with one flag",
    sub: "Given <code>.env.dev</code> and <code>.env.prod</code> side by side, the selector variable decides which one is sourced.",
    html: `
      <div class="cmdline"><span class="cmdline__sigil">$</span><span class="cmdline__text" id="c1">ciabatta deploy web --gui -e DEPLOY_ENV=dev</span><button class="copy" data-copy-target="c1">copy</button></div>
      ${note("Sources <code>.env.dev</code>. Swap in <code>-e DEPLOY_ENV=prod</code> to source <code>.env.prod</code> instead. <code>DEPLOY_ENV</code> can come from anywhere in the environment — a CI variable, your shell, or the <code>-e</code> flag.")}
      <div class="split" style="margin-top:22px;">
        ${code(`# .env.dev
TARGET=dev.internal
DB_URL=postgres://dev-db/app`)}
        ${code(`# .env.prod
TARGET=app.example.com
DB_URL=postgres://prod-db/app`)}
      </div>`,
  }),

  section({
    eyebrow: "Resolution",
    title: "How the placeholder resolves",
    sub: "The selector is filled in from the fully-resolved environment, before the file is opened.",
    html: `
      ${table(
        ["Step", "What happens"],
        [
          ["1 · Resolve env", "Ambient environment + CI/git <code>CIABATTA_*</code> + <code>-e</code> flags are merged."],
          ["2 · Substitute", "<code>{DEPLOY_ENV}</code> in the path is replaced with that value (e.g. <code>.env.prod</code>)."],
          ["3 · Source", "The resolved file is read and its values layered under anything already set."],
        ],
      )}
      ${note("If the selector variable isn't set, the deploy fails fast rather than sourcing the wrong file:<br><code>Deploy 'web' env_file '.env.{DEPLOY_ENV}': Variable '{DEPLOY_ENV}' not set. Set the variable (e.g. -e DEPLOY_ENV=dev)…</code>")}`,
  }),

  section({
    eyebrow: "Note",
    title: "Syntax details",
    html: `${cards([
      { icon: "{ }", title: "Single braces, uppercase", body: "Use <code>{DEPLOY_ENV}</code> — the same as publish paths — not shell's <code>${DEPLOY_ENV}</code>." },
      { icon: "🧭", title: "Any part of the path", body: "The placeholder can sit anywhere: <code>config/{REGION}/.env</code>, <code>.env.{STAGE}</code>, etc." },
      { icon: "🔗", title: "Pairs with conditional steps", body: "Selecting an env file and skipping steps by condition are complementary — drive both from the same variables." },
    ])}`,
  }),
].join("");

mountDoc({
  slug: "deploy-env-select.html",
  eyebrow: "Deploy · Configuration",
  title: "Per-environment selection",
  lede: "Use a <code>{VAR}</code> placeholder in <code>env_file</code> to pick which <code>.env</code> is sourced at run time — one deploy recipe, every environment.",
  contentHTML: content,
});
