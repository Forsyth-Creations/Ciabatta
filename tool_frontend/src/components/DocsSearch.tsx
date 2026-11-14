/**
 * Search the docs page: every section, down to its sub-headings, by the text
 * it actually shows.
 *
 * The sections are JSX, not data, so the index is read off the rendered page
 * rather than kept beside it — a section added tomorrow is searchable without
 * anyone remembering to list it, and the command and API tables are found by
 * the same words a reader sees in them.
 *
 * Matches are highlighted where they sit with the CSS Custom Highlight API,
 * which paints ranges without touching the DOM React owns. A browser without
 * it still gets the results and the jump to them; only the paint is missing.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Box,
  InputAdornment,
  List,
  ListItemButton,
  TextField,
  Typography,
} from "@mui/material";
import SearchIcon from "@mui/icons-material/Search";

import { monoFontStack } from "../theme";
import {
  readChunks,
  search,
  terms,
  type Chunk,
  type Hit,
} from "./docsSearchIndex";

/** Ranges covering every occurrence of every term in the given text nodes. */
function rangesFor(nodes: Text[], words: string[]): Range[] {
  const ranges: Range[] = [];
  for (const node of nodes) {
    const lower = node.data.toLowerCase();
    for (const w of words) {
      for (
        let at = lower.indexOf(w);
        at !== -1;
        at = lower.indexOf(w, at + w.length)
      ) {
        const range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + w.length);
        ranges.push(range);
      }
    }
  }
  return ranges;
}

const HIGHLIGHT = "ciabatta-docs-search";
const CURRENT = "ciabatta-docs-search-current";

interface HighlightRegistry {
  set(name: string, highlight: unknown): void;
  delete(name: string): void;
}

function registry(): HighlightRegistry | null {
  const css = (globalThis as { CSS?: { highlights?: HighlightRegistry } }).CSS;
  const Highlight = (globalThis as { Highlight?: unknown }).Highlight;
  return css?.highlights && Highlight ? css.highlights : null;
}

function paint(name: string, ranges: Range[]) {
  const highlights = registry();
  if (!highlights) return;
  if (ranges.length === 0) {
    highlights.delete(name);
    return;
  }
  const Highlight = (
    globalThis as unknown as { Highlight: new (...r: Range[]) => unknown }
  ).Highlight;
  highlights.set(name, new Highlight(...ranges));
}

/**
 * The search box and its results.
 *
 * `root` is the element holding the sections. `offset` is how far below the
 * top of the window a jumped-to match should land, clear of the app bar.
 */
export function DocsSearch({
  root,
  offset,
  autoFocusKey = true,
}: {
  root: React.RefObject<HTMLElement>;
  offset: number;
  /** Whether `/` focuses this box. Only one on the page should. */
  autoFocusKey?: boolean;
}) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [chunks, setChunks] = useState<Chunk[]>([]);
  const input = useRef<HTMLInputElement>(null);

  // Read the page when a search starts, not on every keystroke: the sections
  // don't change while someone types. A fresh search re-reads, so anything
  // that loaded late (download links, the version) is in it.
  const searching = query.trim().length > 0;
  useEffect(() => {
    if (searching && root.current) setChunks(readChunks(root.current));
  }, [searching, root]);

  const hits = useMemo(() => search(chunks, query), [chunks, query]);
  useEffect(() => setSelected(0), [query]);

  // Every match on the page, painted while there's a query.
  useEffect(() => {
    const words = terms(query);
    paint(
      HIGHLIGHT,
      words.length
        ? rangesFor(
            chunks.flatMap((c) => c.nodes),
            words,
          )
        : [],
    );
    return () => paint(HIGHLIGHT, []);
  }, [chunks, query]);
  useEffect(() => () => paint(CURRENT, []), []);

  const open = useCallback(
    (hit: Hit) => {
      const words = terms(query);
      const first = rangesFor(hit.chunk.nodes, words)[0];
      const target = first?.startContainer.parentElement ?? hit.chunk.anchor;
      const top =
        target.getBoundingClientRect().top + window.scrollY - offset - 24;
      window.scrollTo({ top, behavior: "smooth" });
      paint(CURRENT, first ? [first] : []);
      history.replaceState(null, "", `#${hit.chunk.sectionId}`);
    },
    [query, offset],
  );

  // `/` to search, the way most docs sites do — unless already typing.
  useEffect(() => {
    if (!autoFocusKey) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey)
        return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, [contenteditable='true']")) return;
      event.preventDefault();
      input.current?.focus();
      input.current?.select();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [autoFocusKey]);

  return (
    <Box>
      <TextField
        inputRef={input}
        size="small"
        fullWidth
        placeholder="Search the docs"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            setQuery("");
            paint(CURRENT, []);
          } else if (e.key === "ArrowDown") {
            e.preventDefault();
            setSelected((s) => Math.min(s + 1, hits.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setSelected((s) => Math.max(s - 1, 0));
          } else if (e.key === "Enter" && hits[selected]) {
            e.preventDefault();
            open(hits[selected]);
          }
        }}
        slotProps={{
          input: {
            startAdornment: (
              <InputAdornment position="start">
                <SearchIcon fontSize="small" />
              </InputAdornment>
            ),
            endAdornment: !searching && autoFocusKey && (
              <InputAdornment position="end">
                <Box
                  component="kbd"
                  sx={{
                    fontFamily: monoFontStack,
                    fontSize: 11,
                    px: 0.75,
                    border: 1,
                    borderColor: "divider",
                    borderRadius: 0.5,
                    color: "text.secondary",
                  }}
                >
                  /
                </Box>
              </InputAdornment>
            ),
          },
          htmlInput: { "aria-label": "Search the docs" },
        }}
      />

      {searching && (
        <Box sx={{ mt: 1 }}>
          <Typography variant="caption" color="text.secondary" sx={{ px: 0.5 }}>
            {hits.length === 0
              ? "Nothing matches every word."
              : `${hits.length}${hits.length === 30 ? "+" : ""} result${hits.length === 1 ? "" : "s"} · ↑↓ and Enter · Esc clears`}
          </Typography>
          <List dense disablePadding>
            {hits.map((hit, i) => (
              <ListItemButton
                key={`${hit.chunk.sectionId}:${hit.chunk.heading ?? ""}:${i}`}
                selected={i === selected}
                onClick={() => {
                  setSelected(i);
                  open(hit);
                }}
                sx={{ borderRadius: 1, display: "block", py: 0.5 }}
              >
                <Typography variant="body2" sx={{ fontWeight: 600 }} noWrap>
                  {hit.chunk.sectionTitle}
                  {hit.chunk.heading && (
                    <Typography
                      component="span"
                      variant="body2"
                      color="text.secondary"
                    >
                      {" › "}
                      {hit.chunk.heading}
                    </Typography>
                  )}
                </Typography>
                <Typography
                  variant="caption"
                  color="text.secondary"
                  sx={{
                    display: "-webkit-box",
                    WebkitLineClamp: 2,
                    WebkitBoxOrient: "vertical",
                    overflow: "hidden",
                  }}
                >
                  {hit.snippet.before}
                  {hit.snippet.match && (
                    <Box
                      component="mark"
                      sx={{
                        bgcolor: "warning.light",
                        color: "inherit",
                        borderRadius: 0.5,
                      }}
                    >
                      {hit.snippet.match}
                    </Box>
                  )}
                  {hit.snippet.after}
                </Typography>
              </ListItemButton>
            ))}
          </List>
        </Box>
      )}
    </Box>
  );
}
