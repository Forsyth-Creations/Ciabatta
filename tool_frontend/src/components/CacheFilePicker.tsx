/**
 * Choose a step's inputs and outputs by browsing the checkout, and save them
 * into its workflow file.
 *
 * Typing globs by hand is where "0 files" comes from: every `cache:` path is
 * relative to the workspace root, so a package's `src/**` has to be written
 * `packages/api/src/**`, and nothing says so until the cache misses. Picking
 * from the tree can't get that wrong — what you tick is the path the daemon
 * listed, from the root.
 *
 * A ticked directory means everything beneath it, which is how the cache reads
 * a directory path. Globs the config already has that aren't a plain path
 * (`src/**\/*.rs`) are kept as written, shown as chips you can remove.
 *
 * Saving writes the workflow's own `cache:` section (see
 * `save_target_files` in the daemon) — the file the step was declared in, so
 * the change is one a reviewer sees in a diff, not a setting hidden in the
 * browser. The run on screen keeps what it ran with; the next run uses it.
 */

import { useMemo, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Checkbox,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  IconButton,
  Stack,
  Tab,
  Tabs,
  TextField,
  Typography,
} from "@mui/material";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import FolderIcon from "@mui/icons-material/Folder";
import InsertDriveFileOutlinedIcon from "@mui/icons-material/InsertDriveFileOutlined";

import { useProjectTree, useSaveTargetFiles } from "../api/cache";
import type { StepView } from "../api/run";
import { monoFontStack } from "../theme";

type Side = "inputs" | "outputs";

/** How many rows a filtered list shows before asking for a narrower filter. */
const FILTER_ROWS = 300;

interface TreeNode {
  name: string;
  path: string;
  dir: boolean;
  children: TreeNode[];
}

/** The listing as a tree, directories first, each level sorted by name. */
function buildTree(dirs: string[], files: string[]): TreeNode[] {
  const root: TreeNode = { name: "", path: "", dir: true, children: [] };
  const index = new Map<string, TreeNode>([["", root]]);
  const place = (path: string, dir: boolean) => {
    if (index.has(path)) return;
    const cut = path.lastIndexOf("/");
    const parentPath = cut < 0 ? "" : path.slice(0, cut);
    if (!index.has(parentPath)) place(parentPath, true);
    const node: TreeNode = { name: path.slice(cut + 1), path, dir, children: [] };
    index.get(parentPath)!.children.push(node);
    index.set(path, node);
  };
  for (const dir of dirs) place(dir, true);
  for (const file of files) place(file, false);
  const sort = (node: TreeNode) => {
    node.children.sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
    node.children.forEach(sort);
  };
  sort(root);
  return root.children;
}

/** A glob, as opposed to a plain path the tree can tick. */
function isPattern(entry: string): boolean {
  return /[*?[\]{}]/.test(entry);
}

/** A config entry as the tree names it: no `./`, no trailing slash. */
function plain(entry: string): string {
  return entry.replace(/^\.\//, "").replace(/\/+$/, "");
}

/** The ticked directory `path` falls under, if any. */
function coveredBy(path: string, picked: Set<string>): string | null {
  let at = path;
  while (at.includes("/")) {
    at = at.slice(0, at.lastIndexOf("/"));
    if (picked.has(at)) return at;
  }
  return null;
}

export function CacheFilePicker({
  project,
  step,
  open,
  onClose,
}: {
  project: string;
  step: StepView;
  open: boolean;
  onClose: () => void;
}) {
  const { data, error, isLoading } = useProjectTree(project, open);
  const save = useSaveTargetFiles();
  const [side, setSide] = useState<Side>("inputs");
  const [filter, setFilter] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const declared = {
    inputs: step.deps.declared_inputs ?? step.deps.inputs,
    outputs: step.deps.declared_outputs ?? step.deps.outputs,
  };
  const [picked, setPicked] = useState<Record<Side, string[]>>(() => ({
    inputs: declared.inputs.map((e) => (isPattern(e) ? e : plain(e))),
    outputs: declared.outputs.map((e) => (isPattern(e) ? e : plain(e))),
  }));
  const current = useMemo(() => new Set(picked[side]), [picked, side]);

  const tree = useMemo(() => (data ? buildTree(data.dirs, data.files) : []), [data]);
  const matches = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!data || !needle) return null;
    return [...data.dirs.map((p) => ({ path: p, dir: true })), ...data.files.map((p) => ({ path: p, dir: false }))]
      .filter((entry) => entry.path.toLowerCase().includes(needle))
      .sort((a, b) => a.path.localeCompare(b.path));
  }, [data, filter]);

  const toggle = (path: string) =>
    setPicked((all) => {
      const list = all[side];
      const next = list.includes(path)
        ? list.filter((p) => p !== path)
        : // A directory ticked takes over whatever was ticked beneath it.
          [...list.filter((p) => !p.startsWith(`${path}/`)), path];
      return { ...all, [side]: next };
    });
  const expand = (path: string) =>
    setExpanded((all) => {
      const next = new Set(all);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  const row = (path: string, dir: boolean, depth: number, label: string) => {
    const covered = coveredBy(path, current);
    const ticked = current.has(path);
    return (
      <Stack
        key={path}
        direction="row"
        alignItems="center"
        spacing={0.25}
        sx={{ pl: depth * 2, minWidth: 0, "&:hover": { bgcolor: "action.hover" } }}
      >
        {dir && matches === null ? (
          <IconButton size="small" onClick={() => expand(path)} aria-label={`open ${path}`} sx={{ p: 0.25 }}>
            {expanded.has(path) ? <ExpandMoreIcon fontSize="small" /> : <ChevronRightIcon fontSize="small" />}
          </IconButton>
        ) : (
          <Box sx={{ width: 26, flexShrink: 0 }} />
        )}
        <Checkbox
          size="small"
          checked={ticked || covered !== null}
          disabled={covered !== null}
          onChange={() => toggle(path)}
          inputProps={{ "aria-label": `use ${path} as ${side === "inputs" ? "an input" : "an output"}` }}
          sx={{ p: 0.25 }}
        />
        {dir ? (
          <FolderIcon sx={{ fontSize: 16, color: "text.secondary" }} />
        ) : (
          <InsertDriveFileOutlinedIcon sx={{ fontSize: 16, color: "text.disabled" }} />
        )}
        <Typography
          variant="body2"
          noWrap
          title={path}
          onClick={() => (dir && matches === null ? expand(path) : toggle(path))}
          sx={{ fontFamily: monoFontStack, fontSize: 12, cursor: "pointer", minWidth: 0 }}
        >
          {label}
          {dir && "/"}
        </Typography>
      </Stack>
    );
  };

  const rows = (nodes: TreeNode[], depth: number): React.ReactNode[] =>
    nodes.flatMap((node) => [
      row(node.path, node.dir, depth, node.name),
      ...(node.dir && expanded.has(node.path) ? rows(node.children, depth + 1) : []),
    ]);

  const target = step.cache_id ?? null;
  const changed =
    picked.inputs.join("\n") !== declared.inputs.map((e) => (isPattern(e) ? e : plain(e))).join("\n") ||
    picked.outputs.join("\n") !== declared.outputs.map((e) => (isPattern(e) ? e : plain(e))).join("\n");

  return (
    <Dialog open={open} onClose={onClose} fullWidth maxWidth="md">
      <DialogTitle sx={{ pb: 0.5 }}>
        Choose the files{" "}
        <Box component="span" sx={{ fontFamily: monoFontStack }}>
          {step.name}
        </Box>{" "}
        reads and writes
      </DialogTitle>
      <DialogContent>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 1 }}>
          Tick what the step reads as its inputs and what it writes as its outputs. Paths are from
          the workspace root, which is what every cache path is relative to; a ticked folder means
          everything in it.
        </Typography>
        {(step.deps.cargo?.length ?? 0) > 0 && (
          <Alert severity="info" sx={{ mb: 1, py: 0.25 }}>
            <Typography variant="caption">
              This target also uses <code>cache.cargo</code>, which adds its crates' files by
              itself. Only what you tick here is written; the Cargo closure stays resolved.
            </Typography>
          </Alert>
        )}
        <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
          <Tabs value={side} onChange={(_, value) => setSide(value)} sx={{ minHeight: 36 }}>
            <Tab value="inputs" label={`Inputs (${picked.inputs.length})`} sx={{ minHeight: 36 }} />
            <Tab value="outputs" label={`Outputs (${picked.outputs.length})`} sx={{ minHeight: 36 }} />
          </Tabs>
          <Box sx={{ flexGrow: 1 }} />
          <TextField
            size="small"
            placeholder="filter paths"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            sx={{ width: 240, "& input": { py: 0.5, fontSize: 13 } }}
          />
        </Stack>

        {picked[side].length > 0 && (
          <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap sx={{ my: 1 }}>
            {picked[side].map((entry) => (
              <Chip
                key={entry}
                size="small"
                label={entry}
                variant={isPattern(entry) ? "outlined" : "filled"}
                title={isPattern(entry) ? "A glob, kept as written" : undefined}
                onDelete={() =>
                  setPicked((all) => ({ ...all, [side]: all[side].filter((p) => p !== entry) }))
                }
                sx={{ fontFamily: monoFontStack, fontSize: 12 }}
              />
            ))}
          </Stack>
        )}

        <Box
          sx={{
            height: 380,
            overflowY: "auto",
            border: 1,
            borderColor: "divider",
            borderRadius: 1,
            py: 0.5,
          }}
        >
          {isLoading && (
            <Typography variant="caption" color="text.secondary" sx={{ p: 1, display: "block" }}>
              Listing the checkout…
            </Typography>
          )}
          {error && (
            <Alert severity="error" sx={{ m: 1 }}>
              {(error as Error).message}
            </Alert>
          )}
          {matches === null ? rows(tree, 0) : matches.slice(0, FILTER_ROWS).map((m) => row(m.path, m.dir, 0, m.path))}
          {matches !== null && matches.length > FILTER_ROWS && (
            <Typography variant="caption" color="text.secondary" sx={{ p: 1, display: "block" }}>
              …and {matches.length - FILTER_ROWS} more. Narrow the filter.
            </Typography>
          )}
          {matches !== null && matches.length === 0 && (
            <Typography variant="caption" color="text.secondary" sx={{ p: 1, display: "block" }}>
              Nothing matches “{filter}”.
            </Typography>
          )}
        </Box>
        {data?.truncated && (
          <Typography variant="caption" color="text.secondary">
            A big checkout: not every file is listed, but every folder is.
          </Typography>
        )}
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
          Files git ignores aren't listed — they are what a build makes. Tick the folder an output
          lands in.
        </Typography>

        {save.isSuccess && (
          <Alert severity="success" sx={{ mt: 1.5 }}>
            Saved to <code>{save.data.path}</code>. The next run of this workflow uses them.
          </Alert>
        )}
        {save.error && (
          <Alert severity="error" sx={{ mt: 1.5 }}>
            {save.error.message}
          </Alert>
        )}
        {!target && (
          <Alert severity="warning" sx={{ mt: 1.5 }}>
            This step wasn't declared in a sub-workspace's workflow file, so there is nowhere to save
            to. Add these paths to its <code>cache:</code> by hand.
          </Alert>
        )}
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>{save.isSuccess ? "Done" : "Cancel"}</Button>
        <Button
          variant="contained"
          disabled={!target || save.isPending || !changed || (picked.inputs.length === 0 && picked.outputs.length === 0)}
          onClick={() =>
            target &&
            save.mutate({ project, target, inputs: picked.inputs, outputs: picked.outputs })
          }
        >
          Save to the workflow file
        </Button>
      </DialogActions>
    </Dialog>
  );
}
