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
 * Every path is listed, including what git ignores: `target/` is where an
 * output is picked from, and a step can read another's build output. Ignored
 * paths are marked, since `gitignore: true` leaves them out of the inputs.
 *
 * An earlier step can be picked too (`cache.inputs_from`): its outputs become
 * this step's inputs, resolved from its workflow file at every run, so they
 * follow when its outputs change.
 *
 * Saving *sets* the lists: what is ticked when you press Save is the whole of
 * the step's inputs and outputs, so unticking a path removes it from the file
 * and an empty tab clears that list. It writes the workflow's own `cache:`
 * section (see
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

/** The workflow file a cache id (`<member>:<workflow>:<step>`) belongs to. */
function workflowOf(cacheId: string): string {
  return cacheId.slice(0, cacheId.lastIndexOf(":"));
}

/** Whether `path` is, or is under, one of the subtrees git ignores. */
function underIgnored(path: string, ignored: Set<string>): boolean {
  let at = path;
  for (;;) {
    if (ignored.has(at)) return true;
    const cut = at.lastIndexOf("/");
    if (cut < 0) return false;
    at = at.slice(0, cut);
  }
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
  earlier = [],
  open,
  onClose,
}: {
  project: string;
  step: StepView;
  /** The steps this one runs after, any of which it can read the outputs of. */
  earlier?: StepView[];
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
  const declaredFrom = step.deps.declared_inputs_from ?? [];
  const [inputsFrom, setInputsFrom] = useState<string[]>(declaredFrom);
  const ignored = useMemo(() => new Set(data?.ignored ?? []), [data]);

  // A workflow's `cache:` is shared by every step in its file, so a sibling
  // can't be one: it would be told to read itself. One step per cache id.
  const own = step.cache_id ? workflowOf(step.cache_id) : null;
  const sources = useMemo(() => {
    const seen = new Set<string>();
    return earlier.filter((s) => {
      if (!s.cache_id || s.recover || workflowOf(s.cache_id) === own || seen.has(s.cache_id)) return false;
      seen.add(s.cache_id);
      return true;
    });
  }, [earlier, own]);
  // Named in the file but not among this run's earlier steps: kept, and shown.
  const unlisted = inputsFrom.filter((id) => !sources.some((s) => s.cache_id === id));

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
    const isIgnored = underIgnored(path, ignored);
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
          sx={{
            fontFamily: monoFontStack,
            fontSize: 12,
            cursor: "pointer",
            minWidth: 0,
            color: isIgnored ? "text.secondary" : undefined,
          }}
        >
          {label}
          {dir && "/"}
        </Typography>
        {isIgnored && ignored.has(path) && (
          <Typography
            variant="caption"
            color="text.disabled"
            title="git ignores this. Fine for an output; as an input it is skipped when the workflow sets gitignore: true."
            sx={{ fontSize: 10, flexShrink: 0, pl: 0.5 }}
          >
            git-ignored
          </Typography>
        )}
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
    inputsFrom.join("\n") !== declaredFrom.join("\n") ||
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
          everything in it. Saving replaces both lists with exactly what is ticked here.
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
            <Tab
              value="inputs"
              label={`Inputs (${picked.inputs.length + inputsFrom.length})`}
              sx={{ minHeight: 36 }}
            />
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

        {side === "inputs" && (sources.length > 0 || inputsFrom.length > 0) && (
          <Box sx={{ mt: 1, p: 1, border: 1, borderColor: "divider", borderRadius: 1 }}>
            <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.5 }}>
              Read an earlier step's outputs: ticking one makes whatever it writes an input of this
              workflow, and follows its outputs if they change.
            </Typography>
            {sources.map((source) => {
              const id = source.cache_id!;
              const outputs = source.deps?.declared_outputs ?? [];
              const ticked = inputsFrom.includes(id);
              return (
                <Stack key={id} direction="row" alignItems="center" spacing={0.5} sx={{ minWidth: 0 }}>
                  <Checkbox
                    size="small"
                    checked={ticked}
                    disabled={!ticked && outputs.length === 0}
                    onChange={() =>
                      setInputsFrom((all) => (ticked ? all.filter((x) => x !== id) : [...all, id]))
                    }
                    inputProps={{ "aria-label": `read the outputs of ${id}` }}
                    sx={{ p: 0.25 }}
                  />
                  <Typography variant="body2" sx={{ fontFamily: monoFontStack, fontSize: 12, flexShrink: 0 }}>
                    {id}
                  </Typography>
                  <Typography
                    variant="caption"
                    color="text.secondary"
                    noWrap
                    title={outputs.join(", ")}
                    sx={{ fontFamily: monoFontStack, minWidth: 0 }}
                  >
                    {outputs.length > 0 ? `→ ${outputs.join(", ")}` : "declares no outputs"}
                  </Typography>
                </Stack>
              );
            })}
            {unlisted.map((id) => (
              <Chip
                key={id}
                size="small"
                label={id}
                variant="outlined"
                title="Named in the workflow file, but not an earlier step of this run"
                onDelete={() => setInputsFrom((all) => all.filter((x) => x !== id))}
                sx={{ fontFamily: monoFontStack, fontSize: 12, mr: 0.5 }}
              />
            ))}
          </Box>
        )}

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
            A big checkout: not every file is listed. Everything git keeps comes first.
          </Typography>
        )}
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
          Everything is listed, including what git ignores — tick the folder an output lands in,
          such as <code>target/release</code>. An ignored input is skipped when the workflow sets{" "}
          <code>gitignore: true</code>; read another step's build output through its step above.
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
          disabled={!target || save.isPending || !changed}
          onClick={() =>
            target &&
            save.mutate({
              project,
              target,
              inputs: picked.inputs,
              inputs_from: inputsFrom,
              outputs: picked.outputs,
            })
          }
        >
          Save to the workflow file
        </Button>
      </DialogActions>
    </Dialog>
  );
}
