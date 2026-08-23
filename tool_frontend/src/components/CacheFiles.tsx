/**
 * The files a graph reads and writes, shown around the graph itself.
 *
 * A stage graph on its own says what will *run*. It doesn't say what any of it
 * depends on, which is the thing you need when a build you expected to be
 * instant isn't. So the inputs go above the first wave and the outputs below
 * the last one, framing the graph with the two sets of files the caching
 * decision is actually made from.
 */

import { useState } from "react";
import {
  Box,
  Chip,
  Collapse,
  Link,
  Paper,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
import DescriptionIcon from "@mui/icons-material/Description";
import InventoryIcon from "@mui/icons-material/Inventory2";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ReplayIcon from "@mui/icons-material/Replay";
import BuildIcon from "@mui/icons-material/Build";

import { humanizeBytes } from "../api/cache";
import type { CachePlan, Decision, FileHash, PlannedStep } from "../api/types";
import { CacheDiffView } from "./CacheDiff";

/** The distinct input files across every stage, de-duplicated by path. */
export function graphInputs(plan: CachePlan): FileHash[] {
  return dedupe(plan.steps.flatMap((step) => step.inputs));
}

/** The distinct output files across every stage. */
export function graphOutputs(plan: CachePlan): FileHash[] {
  return dedupe(plan.steps.flatMap((step) => step.outputs));
}

function dedupe(files: FileHash[]): FileHash[] {
  const byPath = new Map(files.map((file) => [file.path, file]));
  return [...byPath.values()].sort((a, b) => a.path.localeCompare(b.path));
}

/**
 * A framed list of files, collapsed to a count until you open it.
 *
 * A monorepo graph can read thousands of files; the count and the total size
 * are what you scan, and the list is what you open when the count is wrong.
 */
export function FilePanel({
  title,
  hint,
  files,
  icon,
}: {
  title: string;
  hint: string;
  files: FileHash[];
  icon: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const bytes = files.reduce((total, file) => total + file.size, 0);

  if (files.length === 0) {
    return (
      <Paper variant="outlined" sx={{ p: 1.5 }}>
        <Stack direction="row" spacing={1} alignItems="center">
          <Box sx={{ color: "text.disabled", display: "flex" }}>{icon}</Box>
          <Typography variant="body2" color="text.secondary">
            {title}: none declared
          </Typography>
          <Tooltip title={hint}>
            <Typography variant="caption" color="text.disabled">
              why?
            </Typography>
          </Tooltip>
        </Stack>
      </Paper>
    );
  }

  return (
    <Paper variant="outlined" sx={{ p: 1.5 }}>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
        <Box sx={{ color: "text.secondary", display: "flex" }}>{icon}</Box>
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {title}
        </Typography>
        <Chip size="small" label={`${files.length} file${files.length === 1 ? "" : "s"}`} />
        <Typography variant="caption" color="text.secondary">
          {humanizeBytes(bytes)}
        </Typography>
        <Box sx={{ flexGrow: 1 }} />
        <Link component="button" variant="caption" onClick={() => setOpen(!open)}>
          {open ? "hide" : "show"}
        </Link>
      </Stack>

      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
        {hint}
      </Typography>

      <Collapse in={open}>
        <Box sx={{ mt: 1, maxHeight: 320, overflowY: "auto" }}>
          {files.map((file) => (
            <Stack
              key={file.path}
              direction="row"
              spacing={1}
              justifyContent="space-between"
              sx={{ py: 0.25 }}
            >
              <Typography
                variant="body2"
                sx={{
                  fontFamily: "monospace",
                  fontSize: 12,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {file.path}
              </Typography>
              <Typography variant="caption" color="text.disabled" sx={{ flexShrink: 0 }}>
                {humanizeBytes(file.size)}
              </Typography>
            </Stack>
          ))}
        </Box>
      </Collapse>
    </Paper>
  );
}

/** The graph's inputs, drawn above its first wave. */
export function GraphInputsPanel({ plan }: { plan: CachePlan }) {
  return (
    <FilePanel
      title="Input files — read before wave 1"
      hint="A change to any of these means the stages that read them run again. A build that reads something not listed here will be handed a stale result."
      files={graphInputs(plan)}
      icon={<DescriptionIcon fontSize="small" />}
    />
  );
}

/** The graph's outputs, drawn below its last wave. */
export function GraphOutputsPanel({ plan }: { plan: CachePlan }) {
  return (
    <FilePanel
      title="Output files — what this graph produces"
      hint="Stored when a stage runs and restored when it hits, each verified against its recorded hash before it's written back."
      files={graphOutputs(plan)}
      icon={<InventoryIcon fontSize="small" />}
    />
  );
}

/**
 * One stage's caching verdict, shown on its node in the graph.
 *
 * Compact by default — a chip saying what will happen — and expandable into the
 * full diff when it says "rebuild", because that's the only time anyone wants
 * more than a chip.
 */
export function StageCacheBadge({ step }: { step: PlannedStep }) {
  const [open, setOpen] = useState(false);
  const summary = summarize(step.decision);

  if (!summary) return null;

  return (
    <Box sx={{ mt: 1 }}>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
        <Chip
          size="small"
          icon={summary.icon}
          color={summary.color}
          variant="outlined"
          label={summary.label}
        />
        <Typography variant="caption" color="text.secondary">
          {step.inputs.length} in → {step.outputs.length} out
        </Typography>
        {step.diff && (
          <Link component="button" variant="caption" onClick={() => setOpen(!open)}>
            {open ? "hide what changed" : "what changed?"}
          </Link>
        )}
      </Stack>

      {step.diff && (
        <Collapse in={open}>
          <Box sx={{ mt: 1.5 }}>
            <CacheDiffView diff={step.diff} />
          </Box>
        </Collapse>
      )}
    </Box>
  );
}

function summarize(
  decision: Decision,
): { icon: React.ReactElement; color: "success" | "warning" | "default"; label: string } | null {
  switch (decision.outcome) {
    case "fresh":
      return {
        icon: <CheckCircleIcon />,
        color: "success",
        label: "up to date",
      };
    case "hit":
      return {
        icon: <ReplayIcon />,
        color: "success",
        label: decision.source === "remote" ? "from the remote cache" : "from the cache",
      };
    case "rebuild":
      return { icon: <BuildIcon />, color: "warning", label: "will run" };
    // A stage with caching off gets no badge: a chip on every node saying
    // "not cached" would be noise on the graphs where nothing is cached, which
    // is most of them until somebody opts in.
    case "uncached":
      return null;
  }
}
