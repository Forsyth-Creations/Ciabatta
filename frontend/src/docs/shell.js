// Shared shell for the standalone documentation pages. Each doc page imports
// `mountDoc` and passes its content; this module owns the chrome (topbar, hero,
// cross-links, footer) and the interactions (copy buttons, scroll reveal), so
// every page matches the landing site and stays consistent with the others.
import "../style.css";

const VERSION = __APP_VERSION__;
const REPO = "https://github.com/forsyth-creations/ciabatta";
// "/Ciabatta/" in the built site, "/" in dev — keeps links + image paths right.
const BASE = import.meta.env.BASE_URL;

// The deploy documentation set. Order defines the in-page nav and prev/next.
export const DOC_PAGES = [
  {
    slug: "deploy-env-files.html",
    nav: "Env files",
    title: "Sourcing .env files",
    desc: "Load KEY=VALUE files into a deploy before it runs.",
  },
  {
    slug: "deploy-env-select.html",
    nav: "Env selection",
    title: "Per-environment selection",
    desc: "Pick which env file to source with a {VAR} placeholder.",
  },
  {
    slug: "deploy-conditional-steps.html",
    nav: "Conditional steps",
    title: "Conditional steps",
    desc: "Skip deploy steps with when / skip_if conditions.",
  },
];

export function esc(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

// ── Lightweight TOML highlighter ─────────────────────────────────────────────
// Good enough for the small config snippets in these docs; matches the span
// classes (c/s/k/v) that style.css already colours.
function inlineHashIndex(s) {
  let inS = false;
  let inD = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (c === "'" && !inD) inS = !inS;
    else if (c === '"' && !inS) inD = !inD;
    else if (c === "#" && !inS && !inD) return i;
  }
  return -1;
}

function highlightValue(v) {
  return v
    .replace(/("[^"]*"|'[^']*')/g, '<span class="v">$1</span>')
    .replace(/(^|[\s,[])(-?\d+(?:\.\d+)?|true|false)\b/g, '$1<span class="v">$2</span>');
}

export function toml(src) {
  const lines = esc(src.replace(/^\n/, "").replace(/\s+$/, "")).split("\n");
  return lines
    .map((raw) => {
      if (!raw.trim()) return "";
      if (/^\s*#/.test(raw)) return `<span class="c">${raw}</span>`;
      let code = raw;
      let comment = "";
      const h = inlineHashIndex(raw);
      if (h !== -1) {
        code = raw.slice(0, h);
        comment = raw.slice(h);
      }
      let html;
      const sec = code.match(/^(\s*)(\[\[?[^\]]+\]?\])(\s*)$/);
      const kv = code.match(/^(\s*)([A-Za-z0-9_.]+)(\s*=\s*)(.*)$/);
      if (sec) {
        html = `${sec[1]}<span class="s">${sec[2]}</span>${sec[3]}`;
      } else if (kv) {
        html = `${kv[1]}<span class="k">${kv[2]}</span>${kv[3]}${highlightValue(kv[4])}`;
      } else {
        html = highlightValue(code);
      }
      if (comment) html += `<span class="c">${comment}</span>`;
      return html;
    })
    .join("\n");
}

// ── Content helpers (returned as HTML strings, composed by each page) ─────────
export function code(src) {
  return `<pre class="code">${toml(src)}</pre>`;
}

export function figure(file, caption) {
  return `
    <figure class="shot">
      <img src="${BASE}${file}" alt="${esc(caption)}" loading="lazy" />
      <figcaption>${caption}</figcaption>
    </figure>`;
}

export function section({ eyebrow, title, sub, html }) {
  return `
    <section class="section reveal"${title ? ` id="${slugify(title)}"` : ""}>
      <div class="section__head">
        ${eyebrow ? `<div class="eyebrow">${eyebrow}</div>` : ""}
        ${title ? `<h2>${title}</h2>` : ""}
        ${sub ? `<p class="section__sub">${sub}</p>` : ""}
      </div>
      ${html}
    </section>`;
}

export function cards(items) {
  return `<div class="grid" style="grid-template-columns:1fr;">${items
    .map(
      (c) =>
        `<div class="fcard"><div class="fcard__icon">${c.icon}</div><h3>${c.title}</h3><p>${c.body}</p></div>`,
    )
    .join("")}</div>`;
}

export function table(headers, rows) {
  return `<div class="tablecard"><table>
    <thead><tr>${headers.map((h) => `<th>${h}</th>`).join("")}</tr></thead>
    <tbody>${rows
      .map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join("")}</tr>`)
      .join("")}</tbody>
  </table></div>`;
}

export function note(html) {
  return `<p class="cmdline__note">${html}</p>`;
}

function slugify(s) {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/(^-|-$)/g, "");
}

// ── The page assembler ───────────────────────────────────────────────────────
export function mountDoc({ slug, eyebrow, title, lede, contentHTML }) {
  const idx = DOC_PAGES.findIndex((p) => p.slug === slug);
  const prev = idx > 0 ? DOC_PAGES[idx - 1] : null;
  const next = idx >= 0 && idx < DOC_PAGES.length - 1 ? DOC_PAGES[idx + 1] : null;

  const nav = DOC_PAGES.map(
    (p) =>
      `<a class="topbar__link${p.slug === slug ? " is-active" : ""}" href="${BASE}${p.slug}">${p.nav}</a>`,
  ).join("");

  const app = document.getElementById("app");
  app.innerHTML = `
    <header class="topbar">
      <div class="topbar__inner">
        <a class="brand" href="${BASE}"><span class="brand__loaf">🍞</span> ciabatta <span class="brand__ver">v${VERSION}</span></a>
        <span class="topbar__spacer"></span>
        ${nav}
        <a class="topbar__cta" href="${REPO}">GitHub ↗</a>
      </div>
    </header>

    <div class="wrap">
      <nav class="crumbs"><a href="${BASE}">Home</a> <span>/</span> <a href="${BASE}#deploy">Deploy</a> <span>/</span> <b>${title}</b></nav>

      <section class="dochero reveal in">
        <div class="eyebrow">${eyebrow}</div>
        <h1>${title}</h1>
        <p class="dochero__lede">${lede}</p>
      </section>

      ${contentHTML}

      <nav class="docnav reveal">
        ${prev ? `<a class="docnav__link docnav__prev" href="${BASE}${prev.slug}"><span>← Previous</span><b>${prev.title}</b></a>` : "<span></span>"}
        ${next ? `<a class="docnav__link docnav__next" href="${BASE}${next.slug}"><span>Next →</span><b>${next.title}</b></a>` : "<span></span>"}
      </nav>
    </div>

    <footer class="footer">
      <div class="footer__loaf">🍞</div>
      <div class="footer__links">
        <a href="${BASE}">Home</a>
        <a href="${REPO}">GitHub</a>
        <a href="${REPO}/blob/main/README.md">Docs</a>
      </div>
      <p class="footer__fine">Ciabatta v${VERSION} · MIT License · Artifact publishing made easy</p>
    </footer>
  `;

  wireCopyButtons();
  wireReveal();
}

function wireCopyButtons() {
  document.querySelectorAll("[data-copy-target]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const target = document.getElementById(btn.dataset.copyTarget);
      if (!target) return;
      try {
        await navigator.clipboard.writeText(target.textContent.trim());
        const prev = btn.textContent;
        btn.textContent = "copied ✓";
        btn.classList.add("copied");
        setTimeout(() => {
          btn.textContent = prev;
          btn.classList.remove("copied");
        }, 1400);
      } catch {
        btn.textContent = "select & copy";
      }
    });
  });
}

function wireReveal() {
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const els = [...document.querySelectorAll(".reveal")];
  if (reduce || !("IntersectionObserver" in window)) {
    els.forEach((el) => el.classList.add("in"));
    return;
  }
  const io = new IntersectionObserver(
    (entries) => {
      entries.forEach((e) => {
        if (e.isIntersecting) {
          e.target.classList.add("in");
          io.unobserve(e.target);
        }
      });
    },
    { rootMargin: "0px 0px -10% 0px" },
  );
  els.forEach((el) => io.observe(el));
}
