/**
 * Every file the cache saw for one step, with its hash — the sanity check for
 * "is the cache looking at what I think it's looking at?".
 *
 * Inspect mode's answer to a miss is usually a diff, but a diff only says what
 * moved since last time. When the question is "why is this file not in my
 * key?" or "is that the build I just made?", you want the whole list: every
 * input the key was computed from and every output that was stored, each with
 * the SHA-256 the cache compares — the same value `sha256sum <path>` prints,
 * so any line can be checked by hand.
 */

import { useMemo, useState } from "react";
import {
  Alert,
  Box,
  IconButton,
  Stack,
  Tab,
  Tabs,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";

import { humanizeBytes, useCacheKeyFiles } from "../api/cache";
import type { FileHash } from "../api/types";
import { monoFontStack } from "../theme";

/** How many rows render before the list asks for a filter instead. */
const MAX_ROWS = 400;

function copy(text: string) {
  try {
    void navigator.clipboard?.writeText(text);
  } catch {
    // No clipboard (an insecure origin): the value is still selectable.
  }
}

/** A hash or key, shortened, with the full value a hover and a click away. */
export function HashText({ value, chars = 12 }: { value: string; chars?: number }) {
  if (!value) {
    return (
      <Typography component="span" variant="caption" color="text.disabled">
        not hashed
      </Typography>
    );
  }
  return (
    <Tooltip title={`${value} — click to copy`}>
      <Box
        component="span"
        onClick={() => copy(value)}
        sx={{ fontFamily: monoFontStack, fontSize: 12, cursor: "copy", color: "text.secondary" }}
      >
        {value.slice(0, chars)}
      </Box>
    </Tooltip>
  );
}

function FileRows({ files, filter }: { files: FileHash[]; filter: string }) {
  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return needle
      ? files.filter((f) => f.path.toLowerCase().includes(needle) || f.sha256.startsWith(needle))
      : files;
  }, [files, filter]);

  if (files.length === 0) {
    return (
      <Typography variant="caption" color="text.secondary">
        None.
      </Typography>
    );
  }

  return (
    <Box sx={{ maxHeight: 300, overflowY: "auto" }}>
      {shown.slice(0, MAX_ROWS).map((file) => (
        <Stack
          key={file.path}
          direction="row"
          spacing={1.5}
          alignItems="baseline"
          sx={{ py: 0.25, borderBottom: 1, borderColor: "divider" }}
        >
          <Typography
            variant="body2"
            title={file.path}
            sx={{
              fontFamily: monoFontStack,
              fontSize: 12,
              flexGrow: 1,
              minWidth: 0,
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
          <Box sx={{ flexShrink: 0 }}>
            <HashText value={file.sha256} />
          </Box>
        </Stack>
      ))}
      {shown.length > MAX_ROWS && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
          …and {shown.length - MAX_ROWS} more. Filter to narrow the list.
        </Typography>
      )}
      {shown.length === 0 && (
        <Typography variant="caption" color="text.secondary">
          Nothing matches “{filter}”.
        </Typography>
      )}
    </Box>
  );
}

/**
 * The inputs and outputs behind a step's cache key. `live` while the step may
 * still be writing, so its outputs fill in as it builds.
 */
export function CacheKeyFilesView({
  project,
  cacheKey,
  live,
}: {
  project: string;
  cacheKey: string;
  live: boolean;
}) {
  const { data, error, isLoading } = useCacheKeyFiles(project, cacheKey, live);
  const [tab, setTab] = useState<"inputs" | "outputs">("inputs");
  const [filter, setFilter] = useState("");

  if (isLoading) {
    return (
      <Typography variant="caption" color="text.secondary">
        Reading the files behind this key…
      </Typography>
    );
  }
  if (error || !data) {
    return (
      <Alert severity="info" sx={{ py: 0.25 }}>
        <Typography variant="caption">
          No file list is recorded for this key on this machine
          {error ? ` (${(error as Error).message})` : ""}. It is written when a run decides
          what to do with the step, so runs from older versions of ciabatta don't have one.
        </Typography>
      </Alert>
    );
  }

  const files = tab === "inputs" ? data.inputs : data.outputs;
  const bytes = files.reduce((total, f) => total + f.size, 0);

  return (
    <Box>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
        <Typography variant="overline" color="text.secondary" sx={{ lineHeight: 1.5 }}>
          Files &amp; hashes
        </Typography>
        <Typography variant="caption" color="text.secondary">
          {data.algorithm.toUpperCase()} — the value <code>{data.algorithm}sum</code> prints
        </Typography>
        <Tooltip title="Copy the whole list as `hash  path` lines, the format sha256sum -c checks">
          <IconButton
            size="small"
            onClick={() =>
              copy(files.map((f) => `${f.sha256}  ${f.path}`).join("\n") + "\n")
            }
          >
            <ContentCopyIcon fontSize="inherit" />
          </IconButton>
        </Tooltip>
      </Stack>
      <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 0.5 }}>
        <Typography variant="caption" color="text.secondary">
          key
        </Typography>
        <HashText value={data.key} chars={24} />
      </Stack>
      {!data.stored && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.5 }}>
          Nothing is stored under this key yet: the inputs are what the run keyed the step on, and
          the outputs are what's on disk right now{live ? " (refreshing while it builds)" : ""}.
        </Typography>
      )}
      <Stack direction="row" spacing={1} alignItems="center">
        <Tabs
          value={tab}
          onChange={(_, value) => setTab(value)}
          sx={{ minHeight: 32, "& .MuiTab-root": { minHeight: 32, py: 0.5 } }}
        >
          <Tab value="inputs" label={`Inputs (${data.inputs.length})`} />
          <Tab value="outputs" label={`Outputs (${data.outputs.length})`} />
        </Tabs>
        <Box sx={{ flexGrow: 1 }} />
        <Typography variant="caption" color="text.secondary">
          {humanizeBytes(bytes)}
        </Typography>
        <TextField
          size="small"
          placeholder="filter path or hash"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
          sx={{ width: 200, "& input": { py: 0.5, fontSize: 12 } }}
        />
      </Stack>
      <FileRows files={files} filter={filter} />
    </Box>
  );
}
