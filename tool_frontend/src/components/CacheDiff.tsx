/**
 * Why a stage didn't hit the cache, shown the way a pull request shows a change.
 *
 * "Cache miss" is not something you can act on. What you can act on is *these
 * three lines of this file moved*, and until a cache can show that, people
 * don't trust it — and a build cache nobody trusts gets switched off, which is
 * worse than not having one.
 *
 * A stage has three dependencies, so this renders three sections: the input
 * files (with hunks), the environment variables it declared, and the outputs of
 * the stages it needs. Any of the three changing is a rebuild, and each is
 * shown in the same place so nobody has to guess which one it was.
 */

import { useState } from "react";
import {
  Accordion,
  AccordionDetails,
  AccordionSummary,
  Box,
  Chip,
  Stack,
  Typography,
} from "@mui/material";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";

import type { CacheDiff, ChangeKind, FileDiff, Hunk } from "../api/types";

/** Colour per kind of change, matching the convention every diff view uses. */
const KIND_COLOR: Record<ChangeKind, "success" | "error" | "warning"> = {
  added: "success",
  removed: "error",
  modified: "warning",
};

const KIND_LABEL: Record<ChangeKind, string> = {
  added: "added",
  removed: "deleted",
  modified: "modified",
};

export function CacheDiffView({ diff }: { diff: CacheDiff }) {
  const nothing =
    diff.files.length === 0 && diff.env.length === 0 && diff.upstream.length === 0;

  if (nothing) {
    return (
      <Typography variant="body2" color="text.secondary">
        Nothing changed since the last run — this stage is being rebuilt for another
        reason (most likely its outputs are missing).
      </Typography>
    );
  }

  return (
    <Stack spacing={2}>
      {diff.previous_at && (
        <Typography variant="caption" color="text.secondary">
          Compared against the run of {new Date(diff.previous_at).toLocaleString()}
        </Typography>
      )}

      {diff.upstream.length > 0 && (
        <Section
          title="Upstream stages"
          subtitle="A stage this one depends on produced something different, so this has to run again even though its own files didn't move."
        >
          <Stack spacing={0.5}>
            {diff.upstream.map((entry) => (
              <Stack key={entry.step} direction="row" spacing={1} alignItems="center">
                <Chip size="small" color={KIND_COLOR[entry.kind]} label={KIND_LABEL[entry.kind]} />
                <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
                  {entry.step}
                </Typography>
              </Stack>
            ))}
          </Stack>
        </Section>
      )}

      {diff.env.length > 0 && (
        <Section
          title="Environment"
          subtitle="Variables this stage declared its result depends on."
        >
          <Stack spacing={0.5}>
            {diff.env.map((entry) => (
              <Stack key={entry.name} direction="row" spacing={1} alignItems="center">
                <Chip size="small" color={KIND_COLOR[entry.kind]} label={KIND_LABEL[entry.kind]} />
                <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
                  {entry.name}
                </Typography>
                {entry.kind === "modified" && (
                  <Typography variant="body2" color="text.secondary" sx={{ fontFamily: "monospace" }}>
                    {entry.before} → {entry.after}
                  </Typography>
                )}
                {entry.kind === "added" && (
                  <Typography variant="body2" color="text.secondary" sx={{ fontFamily: "monospace" }}>
                    = {entry.after}
                  </Typography>
                )}
              </Stack>
            ))}
          </Stack>
        </Section>
      )}

      {diff.files.length > 0 && (
        <Section title="Input files" subtitle={fileSummary(diff.files)}>
          <Stack spacing={1}>
            {diff.files.map((file) => (
              <FileDiffView key={file.path} file={file} />
            ))}
          </Stack>
        </Section>
      )}
    </Stack>
  );
}

function fileSummary(files: FileDiff[]): string {
  const additions = files.reduce((total, file) => total + file.additions, 0);
  const deletions = files.reduce((total, file) => total + file.deletions, 0);
  return `${files.length} file${files.length === 1 ? "" : "s"} changed, +${additions} −${deletions}`;
}

function Section({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <Box>
      <Typography variant="subtitle2">{title}</Typography>
      <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 1 }}>
        {subtitle}
      </Typography>
      {children}
    </Box>
  );
}

/**
 * One file, collapsed to its header until you open it.
 *
 * A rebuild can touch a lot of files, and a page that dumps every hunk of every
 * one is a page nobody scrolls to the bottom of. The header alone answers "what
 * moved"; opening it answers "what exactly".
 */
function FileDiffView({ file }: { file: FileDiff }) {
  // Open a single changed file by default: with one file, the header is
  // already the answer to "what moved" and the hunks are the actual question.
  const [open, setOpen] = useState(false);

  return (
    <Accordion
      expanded={open}
      onChange={(_, next) => setOpen(next)}
      disableGutters
      sx={{ "&:before": { display: "none" }, border: 1, borderColor: "divider" }}
    >
      <AccordionSummary expandIcon={<ExpandMoreIcon />}>
        <Stack direction="row" spacing={1} alignItems="center" sx={{ minWidth: 0, flexGrow: 1 }}>
          <Chip size="small" color={KIND_COLOR[file.kind]} label={KIND_LABEL[file.kind]} />
          <Typography
            variant="body2"
            sx={{
              fontFamily: "monospace",
              flexGrow: 1,
              minWidth: 0,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {file.path}
          </Typography>
          <Typography variant="caption" color="success.main">
            +{file.additions}
          </Typography>
          <Typography variant="caption" color="error.main">
            −{file.deletions}
          </Typography>
        </Stack>
      </AccordionSummary>

      <AccordionDetails sx={{ p: 0 }}>
        {file.note ? (
          <Typography variant="body2" color="text.secondary" sx={{ p: 2 }}>
            {file.note}
          </Typography>
        ) : (
          file.hunks.map((hunk, index) => <HunkView key={index} hunk={hunk} />)
        )}
      </AccordionDetails>
    </Accordion>
  );
}

function HunkView({ hunk }: { hunk: Hunk }) {
  return (
    <Box sx={{ overflowX: "auto" }}>
      <Box
        sx={{
          px: 2,
          py: 0.5,
          bgcolor: "action.hover",
          color: "text.secondary",
          fontFamily: "monospace",
          fontSize: 12,
        }}
      >
        @@ -{hunk.old_start},{hunk.old_lines} +{hunk.new_start},{hunk.new_lines} @@
      </Box>

      <Box component="table" sx={{ borderCollapse: "collapse", width: "100%", fontSize: 12 }}>
        <tbody>
          {hunk.lines.map((line, index) => {
            const added = line.op === "added";
            const removed = line.op === "removed";
            return (
              <Box
                component="tr"
                key={index}
                sx={{
                  // Tinted rather than solid, so the row stays readable in both
                  // the light and dark themes without a second palette.
                  bgcolor: added
                    ? "rgba(46, 160, 67, 0.15)"
                    : removed
                      ? "rgba(248, 81, 73, 0.15)"
                      : "transparent",
                }}
              >
                <LineNumber>{line.op === "added" ? "" : line.old}</LineNumber>
                <LineNumber>{line.op === "removed" ? "" : line.new}</LineNumber>
                <Box
                  component="td"
                  sx={{
                    px: 1,
                    // Takes every pixel the two gutters don't, which is what
                    // makes them shrink to their contents.
                    width: "100%",
                    fontFamily: "monospace",
                    whiteSpace: "pre",
                    color: added ? "success.main" : removed ? "error.main" : "text.primary",
                  }}
                >
                  {added ? "+" : removed ? "−" : " "}
                  {line.text}
                </Box>
              </Box>
            );
          })}
        </tbody>
      </Box>
    </Box>
  );
}

function LineNumber({ children }: { children: React.ReactNode }) {
  return (
    <Box
      component="td"
      sx={{
        px: 1,
        // `width: 1` would be MUI shorthand for 100% and push the code off the
        // right of the row; "1%" is the table idiom for "as narrow as fits".
        width: "1%",
        whiteSpace: "nowrap",
        textAlign: "right",
        color: "text.disabled",
        fontFamily: "monospace",
        userSelect: "none",
        borderRight: 1,
        borderColor: "divider",
      }}
    >
      {children}
    </Box>
  );
}
