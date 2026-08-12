/**
 * "Your environment moved since last time."
 *
 * A pulled branch that adds, drops, or changes an environment variable is one
 * of the reliably confusing ways for a build to break: nothing in the diff
 * looks related, and the failure surfaces somewhere else entirely, often
 * minutes later. Ciabatta already knows which `.env` files a run sources and
 * snapshots what they defined, so it can simply say so before you start.
 *
 * Shown where it changes a decision — above the run launcher and on the
 * workspace catalogue — rather than globally, and dismissible, because a notice
 * that can't be silenced becomes one nobody reads.
 */

import { useState } from "react";
import { Alert, AlertTitle, Box, Collapse, Link as MuiLink, Typography } from "@mui/material";

import { useEnvDrift, type EnvChange } from "../api/workspace";
import { monoFontStack } from "../theme";

/** One change as a line of text, matching what the CLI prints. */
function describe(change: EnvChange): string {
  switch (change.kind) {
    case "added":
      return `${change.key} is new in ${change.file}`;
    case "removed":
      return `${change.key} is gone from ${change.file}`;
    case "changed":
      return `${change.key} changed in ${change.file}`;
    case "file_added":
      return `${change.file} is new (${change.keys} variable${change.keys === 1 ? "" : "s"})`;
    case "file_removed":
      return `${change.file} is no longer there`;
  }
}

/** The marker before each line: what kind of move it was, at a glance. */
function marker(change: EnvChange): string {
  if (change.kind === "changed") return "~";
  return change.kind === "removed" || change.kind === "file_removed" ? "−" : "+";
}

export function EnvDriftBanner({ project }: { project: string }) {
  const { data } = useEnvDrift(project);
  const [dismissed, setDismissed] = useState(false);

  if (!data?.noteworthy || dismissed) return null;

  const count = data.changes.length;

  return (
    <Collapse in>
      <Alert severity="warning" onClose={() => setDismissed(true)} sx={{ mb: 2, maxWidth: 1100 }}>
        <AlertTitle>
          {count} environment variable{count === 1 ? "" : "s"} changed since ciabatta last ran here
        </AlertTitle>
        <Box sx={{ mb: 1 }}>
          {data.changes.map((change, index) => (
            <Typography
              key={index}
              variant="body2"
              sx={{ fontFamily: monoFontStack, fontSize: 13, lineHeight: 1.7 }}
            >
              {marker(change)} {describe(change)}
            </Typography>
          ))}
        </Box>
        <Typography variant="body2">
          A run started now will use the new values. If you didn&apos;t make these changes, a{" "}
          <MuiLink
            component="span"
            sx={{ fontFamily: monoFontStack, fontSize: "0.9em", cursor: "default" }}
          >
            git diff
          </MuiLink>{" "}
          on {data.files.length === 1 ? "that file" : "those files"} will say why.
        </Typography>
      </Alert>
    </Collapse>
  );
}
