/**
 * A step's inputs and outputs as one directory tree, each file marked by what
 * it is to the cache.
 *
 * Two flat lists answer "which files?" but not "where?": a key built from
 * `src/**` that also swept up `src/generated/`, or outputs scattered across
 * three directories nobody expected, only look wrong once they are drawn as
 * the tree they live in. So inputs and outputs share one tree — read files in
 * one colour, written files in another — and a file that is both, which is
 * almost always a build invalidating itself, is marked as its own thing.
 *
 * Directories carry the counts beneath them, so a collapsed tree is still a
 * summary: `packages/api — 214 in · 1 out` says where the weight is before
 * anything is opened. Single-child chains collapse into one row
 * (`src/cache/store`), the way editors draw them, so depth costs no clicks.
 */

import { useMemo, useState } from "react";
import { Box, Chip, Stack, ToggleButton, ToggleButtonGroup, Tooltip, Typography } from "@mui/material";
import type { Theme } from "@mui/material/styles";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import FolderIcon from "@mui/icons-material/Folder";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";
import InsertDriveFileOutlinedIcon from "@mui/icons-material/InsertDriveFileOutlined";

import { humanizeBytes } from "../api/cache";
import type { FileHash } from "../api/types";
import { monoFontStack } from "../theme";

/** What a file is to the cache. */
export type FileRole = "input" | "output" | "both";

/** Past this many visible rows the tree stops drawing and asks for a filter. */
const MAX_ROWS = 1500;

/** Below this many files the whole tree opens on first look. */
const OPEN_ALL_BELOW = 120;

interface TreeFile {
  role: FileRole;
  size: number;
  /** The input's hash, the output's, or both when it is both and they differ. */
  inputHash?: string;
  outputHash?: string;
}

interface TreeNode {
  /** The row's label: one name, or several joined when a chain collapsed. */
  name: string;
  path: string;
  children: Map<string, TreeNode>;
  file?: TreeFile;
  inputs: number;
  outputs: number;
  bytes: number;
}

function emptyNode(name: string, path: string): TreeNode {
  return { name, path, children: new Map(), inputs: 0, outputs: 0, bytes: 0 };
}

/** One tree from both sets, every directory carrying the counts below it. */
export function buildTree(inputs: FileHash[], outputs: FileHash[]): TreeNode {
  const files = new Map<string, TreeFile>();
  for (const f of inputs) {
    files.set(f.path, { role: "input", size: f.size, inputHash: f.sha256 });
  }
  for (const f of outputs) {
    const seen = files.get(f.path);
    files.set(
      f.path,
      seen
        ? { ...seen, role: "both", outputHash: f.sha256, size: Math.max(seen.size, f.size) }
        : { role: "output", size: f.size, outputHash: f.sha256 },
    );
  }

  const root = emptyNode("", "");
  for (const [path, file] of files) {
    const parts = path.split("/").filter(Boolean);
    let at = root;
    const chain: TreeNode[] = [root];
    parts.forEach((part, i) => {
      const sub = parts.slice(0, i + 1).join("/");
      let next = at.children.get(part);
      if (!next) {
        next = emptyNode(part, sub);
        at.children.set(part, next);
      }
      at = next;
      chain.push(at);
    });
    at.file = file;
    for (const node of chain) {
      if (file.role !== "output") node.inputs += 1;
      if (file.role !== "input") node.outputs += 1;
      node.bytes += file.size;
    }
  }
  compact(root);
  return root;
}

/** Fold a directory whose only child is a directory into one row. */
function compact(node: TreeNode) {
  for (const [key, child] of [...node.children]) {
    let merged = child;
    while (!merged.file && merged.children.size === 1) {
      const only = [...merged.children.values()][0];
      if (only.file) break;
      merged = { ...only, name: `${merged.name}/${only.name}` };
    }
    if (merged !== child) {
      node.children.delete(key);
      node.children.set(merged.name, merged);
    }
    compact(merged);
  }
}

/** Directories first, then files, each alphabetically. */
function sorted(node: TreeNode): TreeNode[] {
  return [...node.children.values()].sort((a, b) => {
    const dirA = a.file ? 1 : 0;
    const dirB = b.file ? 1 : 0;
    return dirA - dirB || a.name.localeCompare(b.name);
  });
}

export function roleColour(role: FileRole, theme: Theme): string {
  switch (role) {
    case "input":
      return theme.palette.info.main;
    case "output":
      return theme.palette.success.main;
    case "both":
      return theme.palette.warning.main;
  }
}

const ROLE_LABEL: Record<FileRole, string> = {
  input: "input",
  output: "output",
  both: "input & output",
};

const ROLE_HINT: Record<FileRole, string> = {
  input: "Read by the build: part of the cache key, so a change here means a rebuild.",
  output: "Written by the build: stored on a hit's behalf, and verified before one is granted.",
  both:
    "Both read and written. A build that rewrites one of its own inputs changes its key every time it runs — exclude it from inputs, or stop writing it.",
};

type Show = "all" | "inputs" | "outputs";

/**
 * The tree, with a legend that doubles as a filter by role and an optional
 * text filter over paths. `filter` comes from the caller so the tree can sit
 * beside the flat list it replaces and share its search box.
 */
export function FileTree({
  inputs,
  outputs,
  filter = "",
  maxHeight = 420,
  renderHash,
}: {
  inputs: FileHash[];
  outputs: FileHash[];
  filter?: string;
  maxHeight?: number | string;
  /** Draw a hash; omitted, files show size alone. */
  renderHash?: (hash: string) => React.ReactNode;
}) {
  const [show, setShow] = useState<Show>("all");
  const needle = filter.trim().toLowerCase();

  const tree = useMemo(() => {
    const keep = (f: FileHash) =>
      !needle || f.path.toLowerCase().includes(needle) || f.sha256.startsWith(needle);
    return buildTree(
      show === "outputs" ? [] : inputs.filter(keep),
      show === "inputs" ? [] : outputs.filter(keep),
    );
  }, [inputs, outputs, needle, show]);

  const total = tree.inputs + tree.outputs;
  // Open by default when it is small, or when a filter has already narrowed it
  // to what somebody is looking for; otherwise the top two levels.
  const openAll = total <= OPEN_ALL_BELOW || needle.length > 0;
  const [toggled, setToggled] = useState<Map<string, boolean>>(new Map());
  const isOpen = (node: TreeNode, depth: number) =>
    toggled.get(node.path) ?? (openAll || depth < 2);
  const toggle = (node: TreeNode, depth: number) =>
    setToggled((prev) => new Map(prev).set(node.path, !isOpen(node, depth)));

  // Flatten what is visible, so the cap counts rows actually drawn.
  const rows: { node: TreeNode; depth: number }[] = [];
  const walk = (node: TreeNode, depth: number) => {
    for (const child of sorted(node)) {
      if (rows.length > MAX_ROWS) return;
      rows.push({ node: child, depth });
      if (!child.file && isOpen(child, depth)) walk(child, depth + 1);
    }
  };
  walk(tree, 0);

  const both = useMemo(() => {
    const read = new Set(inputs.map((f) => f.path));
    return outputs.filter((f) => read.has(f.path)).length;
  }, [inputs, outputs]);

  return (
    <Box>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap sx={{ mb: 0.75 }}>
        <ToggleButtonGroup
          size="small"
          exclusive
          value={show}
          onChange={(_, value: Show | null) => value && setShow(value)}
          sx={{ "& .MuiToggleButton-root": { py: 0.25, px: 1, fontSize: 12, textTransform: "none" } }}
        >
          <ToggleButton value="all">all</ToggleButton>
          <ToggleButton value="inputs">
            <Swatch role="input" /> inputs ({inputs.length})
          </ToggleButton>
          <ToggleButton value="outputs">
            <Swatch role="output" /> outputs ({outputs.length})
          </ToggleButton>
        </ToggleButtonGroup>
        {both > 0 && (
          <Tooltip title={ROLE_HINT.both}>
            <Chip
              size="small"
              variant="outlined"
              color="warning"
              label={`${both} file(s) both read and written`}
            />
          </Tooltip>
        )}
      </Stack>

      {total === 0 ? (
        <Typography variant="caption" color="text.secondary">
          {needle ? `Nothing matches “${filter}”.` : "No files."}
        </Typography>
      ) : (
        <Box
          role="tree"
          sx={{
            maxHeight,
            overflowY: "auto",
            border: 1,
            borderColor: "divider",
            borderRadius: 1,
            py: 0.5,
          }}
        >
          {rows.slice(0, MAX_ROWS).map(({ node, depth }) =>
            node.file ? (
              <FileRow key={node.path} node={node} depth={depth} renderHash={renderHash} />
            ) : (
              <DirRow
                key={node.path}
                node={node}
                depth={depth}
                open={isOpen(node, depth)}
                onToggle={() => toggle(node, depth)}
              />
            ),
          )}
          {rows.length > MAX_ROWS && (
            <Typography variant="caption" color="text.secondary" sx={{ display: "block", px: 1, py: 0.5 }}>
              …the tree is larger than this. Filter, or collapse a directory, to see the rest.
            </Typography>
          )}
        </Box>
      )}
    </Box>
  );
}

function Swatch({ role }: { role: FileRole }) {
  return (
    <Box
      component="span"
      sx={(theme) => ({
        display: "inline-block",
        width: 8,
        height: 8,
        borderRadius: "50%",
        mr: 0.75,
        bgcolor: roleColour(role, theme),
      })}
    />
  );
}

const INDENT = 14;

function DirRow({
  node,
  depth,
  open,
  onToggle,
}: {
  node: TreeNode;
  depth: number;
  open: boolean;
  onToggle: () => void;
}) {
  const Chevron = open ? ExpandMoreIcon : ChevronRightIcon;
  const Folder = open ? FolderOpenIcon : FolderIcon;
  return (
    <Stack
      role="treeitem"
      aria-expanded={open}
      direction="row"
      alignItems="center"
      spacing={0.5}
      onClick={onToggle}
      sx={{
        pl: `${depth * INDENT + 4}px`,
        pr: 1,
        py: 0.125,
        cursor: "pointer",
        "&:hover": { bgcolor: "action.hover" },
      }}
    >
      <Chevron sx={{ fontSize: 16, color: "text.secondary" }} />
      <Folder sx={{ fontSize: 15, color: "text.secondary" }} />
      <Typography
        variant="body2"
        title={node.path}
        sx={{ fontFamily: monoFontStack, fontSize: 12, flexGrow: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}
      >
        {node.name}/
      </Typography>
      {node.inputs > 0 && <Count role="input" n={node.inputs} label="in" />}
      {node.outputs > 0 && <Count role="output" n={node.outputs} label="out" />}
      <Typography variant="caption" color="text.disabled" sx={{ flexShrink: 0, minWidth: 56, textAlign: "right" }}>
        {humanizeBytes(node.bytes)}
      </Typography>
    </Stack>
  );
}

function Count({ role, n, label }: { role: FileRole; n: number; label: string }) {
  return (
    <Typography
      variant="caption"
      sx={(theme) => ({ flexShrink: 0, color: roleColour(role, theme), fontVariantNumeric: "tabular-nums" })}
    >
      {n} {label}
    </Typography>
  );
}

function FileRow({
  node,
  depth,
  renderHash,
}: {
  node: TreeNode;
  depth: number;
  renderHash?: (hash: string) => React.ReactNode;
}) {
  const file = node.file!;
  const hash = file.outputHash ?? file.inputHash ?? "";
  return (
    <Tooltip title={`${ROLE_LABEL[file.role]} — ${ROLE_HINT[file.role]}`} placement="left" enterDelay={600}>
      <Stack
        role="treeitem"
        direction="row"
        alignItems="center"
        spacing={0.5}
        sx={(theme) => ({
          pl: `${depth * INDENT + 4 + 16}px`,
          pr: 1,
          py: 0.125,
          // The role is carried twice — a coloured edge and the icon tint —
          // so it survives a glance and a colour-blind reader alike, with the
          // tooltip saying it in words.
          boxShadow: `inset 3px 0 0 ${roleColour(file.role, theme)}`,
          bgcolor:
            file.role === "both"
              ? `color-mix(in srgb, ${theme.palette.warning.main} 12%, transparent)`
              : "transparent",
          "&:hover": { bgcolor: "action.hover" },
        })}
      >
        <InsertDriveFileOutlinedIcon
          sx={(theme) => ({ fontSize: 15, color: roleColour(file.role, theme) })}
        />
        <Typography
          variant="body2"
          title={node.path}
          sx={{ fontFamily: monoFontStack, fontSize: 12, flexGrow: 1, minWidth: 0, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}
        >
          {node.name}
        </Typography>
        <Typography
          variant="caption"
          sx={(theme) => ({ flexShrink: 0, color: roleColour(file.role, theme), fontSize: 11 })}
        >
          {ROLE_LABEL[file.role]}
        </Typography>
        <Typography variant="caption" color="text.disabled" sx={{ flexShrink: 0, minWidth: 56, textAlign: "right" }}>
          {humanizeBytes(file.size)}
        </Typography>
        {renderHash && <Box sx={{ flexShrink: 0 }}>{renderHash(hash)}</Box>}
      </Stack>
    </Tooltip>
  );
}
