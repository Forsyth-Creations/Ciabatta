/**
 * The docs search's index and ranking, apart from the UI so they can be read
 * (and run) on their own. See `DocsSearch.tsx`.
 */

/** A stretch of a section: everything under one heading, up to the next. */
export interface Chunk {
  sectionId: string;
  sectionTitle: string;
  /** The sub-heading it sits under; null for the part before the first. */
  heading: string | null;
  /** The element to scroll to when nothing more precise is found. */
  anchor: HTMLElement;
  /** Its text nodes, in order, and their text joined. */
  nodes: Text[];
  text: string;
}

export interface Hit {
  chunk: Chunk;
  score: number;
  snippet: { before: string; match: string; after: string };
}

/** The words of a query: lower-cased, at least one character each. */
export function terms(query: string): string[] {
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 0);
}

/**
 * Chunks holding every term, best first. A term in the section's title or the
 * sub-heading counts for more than one in the prose, and an exact phrase for
 * more than its words apart.
 */
export function search(chunks: Chunk[], query: string, limit = 30): Hit[] {
  const words = terms(query);
  if (words.length === 0) return [];
  const phrase = words.join(" ");
  const hits: Hit[] = [];
  for (const chunk of chunks) {
    const title = chunk.sectionTitle.toLowerCase();
    const heading = (chunk.heading ?? "").toLowerCase();
    const body = chunk.text.toLowerCase();
    const all = `${title} ${heading} ${body}`;
    if (!words.every((w) => all.includes(w))) continue;
    let score = 0;
    for (const w of words) {
      if (title.includes(w)) score += 8;
      if (heading.includes(w)) score += 5;
      score += Math.min(occurrences(body, w), 5);
    }
    if (all.includes(phrase)) score += 6;
    hits.push({ chunk, score, snippet: snippet(chunk.text, words) });
  }
  hits.sort((a, b) => b.score - a.score);
  return hits.slice(0, limit);
}

function occurrences(haystack: string, needle: string): number {
  let count = 0;
  for (let at = haystack.indexOf(needle); at !== -1; at = haystack.indexOf(needle, at + 1)) {
    count += 1;
  }
  return count;
}

/** A line of context around the first term found in the prose. */
export function snippet(text: string, words: string[], width = 120): Hit["snippet"] {
  const lower = text.toLowerCase();
  let at = -1;
  let found = "";
  for (const w of words) {
    const i = lower.indexOf(w);
    if (i !== -1 && (at === -1 || i < at)) {
      at = i;
      found = w;
    }
  }
  if (at === -1) {
    // Only the titles matched: show how the section starts.
    return { before: "", match: "", after: clip(text, width) };
  }
  const lead = Math.max(0, at - Math.floor(width / 3));
  const before = (lead > 0 ? "…" : "") + text.slice(lead, at).trimStart();
  const end = at + found.length;
  const after = text.slice(end, end + width - (at - lead));
  return {
    before,
    match: text.slice(at, end),
    after: after + (end + after.length < text.length ? "…" : ""),
  };
}

function clip(text: string, width: number): string {
  return text.length > width ? text.slice(0, width).trimEnd() + "…" : text;
}

/**
 * Read the chunks off the page: each `<section id>` under `root`, split at its
 * `h3` sub-headings. Text inside `<button>`s (copy buttons and the like) and
 * the section's own `h2` title is left out of the prose — the title is
 * matched on its own.
 */
export function readChunks(root: HTMLElement): Chunk[] {
  const chunks: Chunk[] = [];
  for (const section of Array.from(root.querySelectorAll<HTMLElement>("section[id]"))) {
    const sectionTitle = section.querySelector("h2")?.textContent?.trim() ?? section.id;
    let current: Chunk = {
      sectionId: section.id,
      sectionTitle,
      heading: null,
      anchor: section,
      nodes: [],
      text: "",
    };
    const parts: string[] = [];
    const flush = () => {
      current.text = parts.join("").replace(/\s+/g, " ").trim();
      if (current.text || current.heading) chunks.push(current);
      parts.length = 0;
    };
    const walker = document.createTreeWalker(section, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const text = node as Text;
      const parent = text.parentElement;
      if (!parent || parent.closest("h2, button, [aria-hidden='true']")) continue;
      const heading = parent.closest("h3");
      if (heading) {
        // The first text of a new sub-heading starts a chunk.
        if (current.anchor !== heading) {
          flush();
          current = {
            sectionId: section.id,
            sectionTitle,
            heading: heading.textContent?.trim() ?? "",
            anchor: heading as HTMLElement,
            nodes: [],
            text: "",
          };
        }
        continue;
      }
      current.nodes.push(text);
      parts.push(text.data, " ");
    }
    flush();
  }
  return chunks;
}
