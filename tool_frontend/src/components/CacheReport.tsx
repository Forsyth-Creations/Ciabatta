/**
 * What the cache made of a step, in three sizes:
 *
 * - **`CacheIcon`** — the glyph on a graph node that was served from the cache,
 *   so a reused step is told apart from one that ran at a glance. Clicking it
 *   opens the details without selecting the node.
 * - **`CacheReportView`** — everything the run recorded: which entry, how old,
 *   from where, what moved and what to change. The popover and the inspector
 *   both render this, so they can't disagree.
 * - **`cacheTone` / `cacheLabel`** — the colour and the few words inspect mode
 *   paints onto every node.
 */

import { useState } from "react";
import {
  Alert,
  Box,
  Chip,
  Divider,
  IconButton,
  Popover,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
import type { Theme } from "@mui/material/styles";
import CachedIcon from "@mui/icons-material/Cached";
import CloudDoneIcon from "@mui/icons-material/CloudDone";
import BuildIcon from "@mui/icons-material/Build";
import BlockIcon from "@mui/icons-material/Block";
import LinkOffIcon from "@mui/icons-material/LinkOff";
import TipsAndUpdatesIcon from "@mui/icons-material/TipsAndUpdates";

import { humanizeBytes, humanizeMs } from "../api/cache";
import type { CacheEntryInfo, CacheReport } from "../api/run";
import { monoFontStack } from "../theme";

/**
 * A hint as the daemon wrote it — prose with `backticked` config names — with
 * the backticked parts set as code, so `cache.no_outputs: true` reads as the
 * thing to type rather than as punctuation.
 */
export function Prose({ text }: { text: string }) {
  return (
    <>
      {text.split(/(`[^`]+`)/g).map((part, index) =>
        part.startsWith("`") && part.endsWith("`") && part.length > 1 ? (
          <Box
            key={index}
            component="code"
            sx={{ fontFamily: monoFontStack, fontSize: "0.95em", px: 0.25 }}
          >
            {part.slice(1, -1)}
          </Box>
        ) : (
          part
        ),
      )}
    </>
  );
}

/** Whether the step was served from the cache rather than run. */
export function wasReused(report: CacheReport | null | undefined): boolean {
  return report?.outcome === "fresh" || report?.outcome === "hit";
}

/**
 * "3 minutes ago", "2 days ago" — how old a timestamp is, the way it's said.
 * `short` gives "3m ago", for a figure that has to fit a stat tile.
 */
export function ago(at: string | null | undefined, short = false): string {
  if (!at) return short ? "never" : "—";
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(at)) / 1000));
  if (Number.isNaN(seconds)) return "—";
  if (seconds < 60) return "just now";
  const unit = (n: number, long: string, abbreviation: string) =>
    short ? `${n}${abbreviation} ago` : `${n} ${long}${n === 1 ? "" : "s"} ago`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return unit(minutes, "minute", "m");
  const hours = Math.round(minutes / 60);
  if (hours < 48) return unit(hours, "hour", "h");
  return unit(Math.round(hours / 24), "day", "d");
}

/** The colour inspect mode paints a step: reused, rebuilt, blocked, or ignored. */
export function cacheTone(report: CacheReport | null | undefined, theme: Theme): string {
  if (!report) return theme.palette.text.disabled;
  if (wasReused(report)) return theme.palette.success.main;
  if (report.blocked_by.length > 0) return theme.palette.error.main;
  if (report.outcome === "rebuild") return theme.palette.warning.main;
  return theme.palette.text.disabled;
}

/** The few words inspect mode puts under a node's name. */
export function cacheLabel(report: CacheReport | null | undefined): string {
  if (!report) return "cache not consulted";
  switch (report.outcome) {
    case "fresh":
      return "reused · already up to date";
    case "hit":
      return report.source === "remote" ? "reused · from remote" : "reused · restored";
    case "uncached":
      return "not cached";
    case "rebuild":
      if (report.blocked_by.length > 0) return `blocked by ${report.blocked_by.join(", ")}`;
      return rebuildWords(report);
  }
}

function rebuildWords(report: CacheReport): string {
  switch (report.reason?.kind) {
    case "never_built":
      return report.previous ? "missed · key changed" : "missed · first build";
    case "inputs_changed":
      return `missed · ${report.reason.total as number} input(s) changed`;
    case "outputs_missing":
      return "missed · outputs evicted";
    case "outputs_modified":
      return "missed · outputs edited";
    case "no_outputs":
      return "missed · no outputs declared";
    case "upstream_reran":
      return "missed · upstream reran";
    case "forced":
      return "ran · --force";
    default:
      return "missed";
  }
}

/**
 * The cache glyph on a node that was reused. Clickable: it opens the report in
 * a popover, and stops the click there so it doesn't also select the node.
 */
export function CacheIcon({ report, size = 15 }: { report: CacheReport; size?: number }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null);
  const Icon = report.source === "remote" ? CloudDoneIcon : CachedIcon;
  const where =
    report.outcome === "fresh"
      ? "outputs were already up to date"
      : report.source === "remote"
        ? "restored from the remote cache"
        : "restored from the local cache";

  return (
    <>
      <Tooltip title={`Used its cached version — ${where}. Click for details.`}>
        <IconButton
          size="small"
          // react-flow drags and pans on mousedown; these opt the button out,
          // so a click on it is a click and not the start of a drag.
          className="nodrag nopan"
          aria-label="Show cache details"
          onClick={(event) => {
            event.stopPropagation();
            setAnchor(event.currentTarget);
          }}
          onMouseDown={(event) => event.stopPropagation()}
          sx={{ p: 0.25, color: "success.main", flexShrink: 0 }}
        >
          <Icon sx={{ fontSize: size }} />
        </IconButton>
      </Tooltip>
      <Popover
        open={anchor !== null}
        anchorEl={anchor}
        onClose={() => setAnchor(null)}
        anchorOrigin={{ vertical: "bottom", horizontal: "left" }}
        // The popover renders outside the node, but React still bubbles its
        // clicks through the node's tree — which would select the node.
        onClick={(event) => event.stopPropagation()}
        slotProps={{ paper: { sx: { p: 2, width: 420, maxWidth: "90vw" } } }}
      >
        <CacheReportView report={report} />
      </Popover>
    </>
  );
}

/** Everything the run recorded about a step's cache decision. */
export function CacheReportView({
  report,
  detailed = false,
}: {
  report: CacheReport;
  /** Inspect mode: show the inputs, upstream fingerprints, and the full list of
   *  changed files rather than a summary. */
  detailed?: boolean;
}) {
  const reused = wasReused(report);
  const OutcomeIcon = reused
    ? report.source === "remote"
      ? CloudDoneIcon
      : CachedIcon
    : report.blocked_by.length > 0
      ? LinkOffIcon
      : report.outcome === "rebuild"
        ? BuildIcon
        : BlockIcon;
  const colour = reused
    ? "success.main"
    : report.blocked_by.length > 0
      ? "error.main"
      : report.outcome === "rebuild"
        ? "warning.main"
        : "text.disabled";

  return (
    <Stack spacing={1.25}>
      <Stack direction="row" spacing={1} alignItems="flex-start">
        <Box sx={{ color: colour, display: "flex", pt: 0.25 }}>
          <OutcomeIcon fontSize="small" />
        </Box>
        <Box sx={{ minWidth: 0 }}>
          <Typography variant="subtitle2">{headline(report)}</Typography>
          <Typography variant="caption" color="text.secondary" sx={{ wordBreak: "break-word" }}>
            <Prose text={report.summary} />
          </Typography>
        </Box>
      </Stack>

      {reused && report.entry && (
        <EntryFacts
          title="The cached version"
          entry={report.entry}
          extra={report.saved_ms > 0 ? `saved about ${humanizeMs(report.saved_ms)}` : undefined}
        />
      )}
      {!reused && report.previous && (
        <EntryFacts title="Compared against the last build" entry={report.previous} />
      )}

      {report.stored && (
        <Fact label="stored">
          {report.stored.skipped
            ? `nothing — ${report.stored.skipped}`
            : `${report.stored.outputs} file(s), ${humanizeBytes(report.stored.size)}${
                report.stored.uploaded === null
                  ? " · locally only"
                  : report.stored.uploaded
                    ? " · and uploaded to the remote cache"
                    : " · the upload to the remote cache failed"
              }`}
        </Fact>
      )}

      {report.diff && (
        <Box>
          <Typography variant="caption" color="text.secondary">
            What moved: {report.diff.summary}
          </Typography>
          <Box component="ul" sx={{ m: 0, pl: 2.5 }}>
            {report.diff.files.slice(0, detailed ? undefined : 6).map((file) => (
              <Typography
                component="li"
                key={file.path}
                variant="caption"
                sx={{ display: "list-item", fontFamily: monoFontStack, wordBreak: "break-all" }}
              >
                {file.path}{" "}
                <Box component="span" sx={{ color: "text.secondary" }}>
                  ({file.kind}
                  {file.kind === "modified" && ` +${file.additions} −${file.deletions}`})
                </Box>
              </Typography>
            ))}
            {report.diff.files_total > (detailed ? report.diff.files.length : 6) && (
              <Typography component="li" variant="caption" color="text.secondary">
                … and {report.diff.files_total - (detailed ? report.diff.files.length : 6)} more
              </Typography>
            )}
            {report.diff.env.map((name) => (
              <Typography component="li" key={`env:${name}`} variant="caption" sx={{ display: "list-item" }}>
                variable <code>{name}</code> changed
              </Typography>
            ))}
            {report.diff.upstream.map((step) => (
              <Typography component="li" key={`up:${step}`} variant="caption" sx={{ display: "list-item" }}>
                <code>{step}</code> produced different outputs
              </Typography>
            ))}
          </Box>
        </Box>
      )}

      {report.hints.length > 0 && (
        <Alert
          severity={reused ? "info" : "warning"}
          icon={<TipsAndUpdatesIcon fontSize="small" />}
          sx={{ py: 0.25, "& .MuiAlert-message": { width: "100%" } }}
        >
          <Stack spacing={0.75}>
            {report.hints.map((hint) => (
              <Typography key={hint} variant="caption" sx={{ display: "block" }}>
                <Prose text={hint} />
              </Typography>
            ))}
          </Stack>
        </Alert>
      )}

      {detailed && (
        <>
          <Divider />
          <Fact label="key">
            <Box component="span" sx={{ fontFamily: monoFontStack, wordBreak: "break-all" }}>
              {report.key ?? "none — not keyed"}
            </Box>
          </Fact>
          <Fact label="inputs">
            {report.input_files} file(s), {humanizeBytes(report.input_bytes)}
          </Fact>
          <Fact label="keys on">{report.env.length > 0 ? report.env.join(", ") : "no variables"}</Fact>
          {report.upstream.length > 0 && (
            <Fact label="upstream">
              <Stack spacing={0.25}>
                {report.upstream.map((up) => (
                  <Box key={up.step} component="span" sx={{ display: "block" }}>
                    <Box component="span" sx={{ fontFamily: monoFontStack }}>
                      {up.step}
                    </Box>{" "}
                    <Box
                      component="span"
                      sx={{ color: up.unaccounted ? "error.main" : "text.secondary" }}
                    >
                      {up.unaccounted
                        ? "— ran without declaring its outputs"
                        : up.fingerprint
                          ? `— outputs ${up.fingerprint}…`
                          : "— contributed nothing to the key"}
                    </Box>
                  </Box>
                ))}
              </Stack>
            </Fact>
          )}
          <Fact label="decided">{new Date(report.decided_at).toLocaleString()}</Fact>
        </>
      )}

      {report.remote && (
        <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
          <Chip
            size="small"
            variant="outlined"
            color={report.remote.connected ? "success" : "warning"}
            label={report.remote.connected ? "remote cache connected" : "remote cache unreachable"}
          />
          {report.remote.read_only && <Chip size="small" variant="outlined" label="read-only" />}
          <Typography variant="caption" color="text.secondary" sx={{ fontFamily: monoFontStack }}>
            {report.remote.url}
          </Typography>
        </Stack>
      )}
    </Stack>
  );
}

function headline(report: CacheReport): string {
  switch (report.outcome) {
    case "fresh":
      return "Used its cached version — already up to date";
    case "hit":
      return report.source === "remote"
        ? "Used its cached version — from the remote cache"
        : "Used its cached version — restored locally";
    case "uncached":
      return "Not cached";
    case "rebuild":
      return report.blocked_by.length > 0
        ? `Couldn't be reused — held back by ${report.blocked_by.join(", ")}`
        : "Ran — no cached version matched";
  }
}

function EntryFacts({
  title,
  entry,
  extra,
}: {
  title: string;
  entry: CacheEntryInfo;
  extra?: string;
}) {
  return (
    <Box>
      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.25 }}>
        {title}
      </Typography>
      <Fact label="built">
        {ago(entry.created_at)}
        <Box component="span" sx={{ color: "text.secondary" }}>
          {" "}
          · {new Date(entry.created_at).toLocaleString()} · took {humanizeMs(entry.duration_ms)}
        </Box>
      </Fact>
      <Fact label="last used">{ago(entry.last_used_at ?? entry.created_at)}</Fact>
      <Fact label="holds">
        {entry.outputs} file(s), {humanizeBytes(entry.size)}
        {extra && (
          <Box component="span" sx={{ color: "text.secondary" }}>
            {" "}
            · {extra}
          </Box>
        )}
      </Fact>
      <Fact label="key">
        <Box component="span" sx={{ fontFamily: monoFontStack }}>
          {entry.key.slice(0, 16)}…
        </Box>
      </Fact>
    </Box>
  );
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <Stack direction="row" spacing={1} alignItems="baseline">
      <Typography
        variant="caption"
        color="text.secondary"
        sx={{ minWidth: 64, flexShrink: 0, textAlign: "right" }}
      >
        {label}
      </Typography>
      <Typography variant="caption" component="div" sx={{ minWidth: 0 }}>
        {children}
      </Typography>
    </Stack>
  );
}
