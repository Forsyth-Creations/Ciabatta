/**
 * The flowchart builder: design a run DAG and copy out the TOML.
 *
 * Entirely client-side, as it always was — it runs nothing and needs no
 * project, so there's no server state and no route behind it. That's why it
 * sits above `/run/$runId` in the route tree: "builder" must not be read as
 * a run id.
 */

import { useMemo, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Checkbox,
  FormControlLabel,
  Grid2 as Grid,
  IconButton,
  MenuItem,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import ContentCopyIcon from "@mui/icons-material/ContentCopy";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import { useTheme } from "@mui/material/styles";
import type { Theme } from "@mui/material/styles";
import type { Edge, Node } from "@xyflow/react";

import { GraphCanvas } from "../components/GraphCanvas";
import { layeredLayout } from "../components/layout";
import { PageHeader } from "../components/Page";
import { monoFontStack } from "../theme";

interface DraftStep {
  name: string;
  run: string;
  needs: string[];
  onError: string;
  recover: boolean;
}

const STARTER: DraftStep[] = [
  { name: "build", run: "cargo build --release", needs: [], onError: "", recover: false },
  { name: "test", run: "cargo test", needs: ["build"], onError: "notify", recover: false },
  { name: "notify", run: "echo 'run failed' | mail -s ci team@example.com", needs: [], onError: "", recover: true },
  { name: "publish", run: "./scripts/publish.sh", needs: ["test"], onError: "", recover: false },
];

export function RunBuilderPage() {
  const theme = useTheme();
  const [steps, setSteps] = useState<DraftStep[]>(STARTER);
  const [recipeName, setRecipeName] = useState("release");
  const [copied, setCopied] = useState(false);

  const update = (index: number, patch: Partial<DraftStep>) =>
    setSteps((current) => current.map((s, i) => (i === index ? { ...s, ...patch } : s)));

  const addStep = () =>
    setSteps((current) => [
      ...current,
      { name: `step_${current.length + 1}`, run: "", needs: [], onError: "", recover: false },
    ]);

  const removeStep = (index: number) =>
    setSteps((current) => {
      const name = current[index].name;
      // Dropping a step has to drop the references to it too, or the emitted
      // TOML names a step that doesn't exist.
      return current
        .filter((_, i) => i !== index)
        .map((s) => ({
          ...s,
          needs: s.needs.filter((n) => n !== name),
          onError: s.onError === name ? "" : s.onError,
        }));
    });

  const { nodes, edges } = useMemo(() => buildPreview(steps, theme), [steps, theme]);
  const toml = useMemo(() => emitToml(recipeName, steps), [recipeName, steps]);

  return (
    <>
      <PageHeader
        title="Flowchart builder"
        description="Design a run DAG, then copy the TOML into your ciabatta.toml. Nothing here runs — it's an authoring tool."
      />

      <Grid container spacing={3}>
        <Grid size={{ xs: 12, lg: 7 }}>
          <TextField
            size="small"
            label="Recipe name"
            value={recipeName}
            onChange={(e) => setRecipeName(e.target.value)}
            sx={{ mb: 2, width: 240 }}
          />

          <Stack spacing={2}>
            {steps.map((step, index) => (
              <Box key={index} sx={{ p: 2, border: 1, borderColor: "divider", borderRadius: 1 }}>
                <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 1.5 }}>
                  <TextField
                    size="small"
                    label="Step name"
                    value={step.name}
                    onChange={(e) => update(index, { name: e.target.value })}
                    sx={{ width: 180 }}
                  />
                  <Box sx={{ flexGrow: 1 }} />
                  <FormControlLabel
                    control={
                      <Checkbox
                        size="small"
                        checked={step.recover}
                        onChange={(e) => update(index, { recover: e.target.checked })}
                      />
                    }
                    label="Recovery step"
                  />
                  <IconButton size="small" onClick={() => removeStep(index)}>
                    <DeleteOutlineIcon fontSize="small" />
                  </IconButton>
                </Stack>

                <TextField
                  fullWidth
                  size="small"
                  label="Command"
                  value={step.run}
                  onChange={(e) => update(index, { run: e.target.value })}
                  slotProps={{ htmlInput: { style: { fontFamily: monoFontStack } } }}
                  sx={{ mb: 1.5 }}
                />

                <Stack direction="row" spacing={1}>
                  <TextField
                    select
                    size="small"
                    label="Needs"
                    value={step.needs}
                    onChange={(e) =>
                      update(index, {
                        needs:
                          typeof e.target.value === "string"
                            ? e.target.value.split(",").filter(Boolean)
                            : (e.target.value as unknown as string[]),
                      })
                    }
                    slotProps={{ select: { multiple: true } }}
                    sx={{ minWidth: 200, flexGrow: 1 }}
                  >
                    {steps
                      .filter((_, i) => i !== index)
                      .map((other) => (
                        <MenuItem key={other.name} value={other.name}>
                          {other.name}
                        </MenuItem>
                      ))}
                  </TextField>

                  <TextField
                    select
                    size="small"
                    label="On error"
                    value={step.onError}
                    onChange={(e) => update(index, { onError: e.target.value })}
                    sx={{ minWidth: 180 }}
                  >
                    <MenuItem value="">
                      <em>fail the run</em>
                    </MenuItem>
                    {steps
                      .filter((_, i) => i !== index)
                      .map((other) => (
                        <MenuItem key={other.name} value={other.name}>
                          {other.name}
                        </MenuItem>
                      ))}
                  </TextField>
                </Stack>
              </Box>
            ))}
          </Stack>

          <Button startIcon={<AddIcon />} onClick={addStep} sx={{ mt: 2 }}>
            Add step
          </Button>
        </Grid>

        <Grid size={{ xs: 12, lg: 5 }}>
          <Typography variant="h3" sx={{ mb: 1 }}>
            Preview
          </Typography>
          <GraphCanvas nodes={nodes} edges={edges} height={300} />

          <Stack direction="row" alignItems="center" sx={{ mt: 3, mb: 1 }}>
            <Typography variant="h3" sx={{ flexGrow: 1 }}>
              TOML
            </Typography>
            <Button
              size="small"
              startIcon={<ContentCopyIcon />}
              onClick={() => {
                navigator.clipboard.writeText(toml);
                setCopied(true);
              }}
            >
              Copy
            </Button>
          </Stack>
          {copied && (
            <Alert severity="success" sx={{ mb: 1 }} onClose={() => setCopied(false)}>
              Copied — paste it into your .ciabatta/ciabatta.toml.
            </Alert>
          )}
          <Box
            component="pre"
            sx={{
              m: 0,
              p: 1.5,
              maxHeight: 400,
              overflow: "auto",
              border: 1,
              borderColor: "divider",
              borderRadius: 1,
              bgcolor: "background.default",
              fontFamily: monoFontStack,
              fontSize: 12.5,
            }}
          >
            {toml}
          </Box>
        </Grid>
      </Grid>
    </>
  );
}

function buildPreview(steps: DraftStep[], theme: Theme) {
  const ids = steps.map((s) => s.name);
  const orderEdges = steps.flatMap((step) =>
    step.needs.map((need) => ({ source: need, target: step.name })),
  );

  const positioned = layeredLayout(ids, orderEdges, (id) => ({ label: id }));

  const byName = new Map(steps.map((s) => [s.name, s]));
  const nodes: Node[] = positioned.map((node) => ({
    ...node,
    style: {
      background: theme.palette.background.paper,
      color: theme.palette.text.primary,
      border: `2px ${byName.get(node.id)?.recover ? "dashed" : "solid"} ${
        byName.get(node.id)?.recover ? theme.palette.warning.main : theme.palette.divider
      }`,
      borderRadius: 8,
      fontSize: 12,
      padding: "6px 12px",
    },
  }));

  const edges: Edge[] = [
    ...orderEdges.map((e, i) => ({
      id: `needs-${i}`,
      source: e.source,
      target: e.target,
      style: { stroke: theme.palette.divider },
    })),
    ...steps
      .filter((s) => s.onError)
      .map((s, i) => ({
        id: `error-${i}`,
        source: s.name,
        target: s.onError,
        label: "on_error",
        style: { stroke: theme.palette.error.main, strokeDasharray: "5 4" },
      })),
  ];

  return { nodes, edges };
}

/** Render the draft as a `[recipies.<name>.run]` TOML block. */
function emitToml(recipeName: string, steps: DraftStep[]): string {
  const name = recipeName.trim() || "release";
  const lines: string[] = [`[recipies.${name}.run]`, ""];

  for (const step of steps) {
    if (!step.name.trim()) continue;
    lines.push(`[[recipies.${name}.run.steps]]`);
    lines.push(`name = ${quote(step.name)}`);
    if (step.run.trim()) lines.push(`run = ${quote(step.run)}`);
    if (step.needs.length > 0) {
      lines.push(`needs = [${step.needs.map(quote).join(", ")}]`);
    }
    if (step.onError) lines.push(`on_error = ${quote(step.onError)}`);
    if (step.recover) lines.push("recover = true");
    lines.push("");
  }

  return lines.join("\n");
}

/** TOML basic string, escaping what the format requires. */
function quote(value: string): string {
  const escaped = value
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .replace(/\t/g, "\\t");
  return `"${escaped}"`;
}
