/**
 * Environment variables, drawn as what they are: dependencies of the steps that
 * read them.
 *
 * A graph explains why a step runs when it does. It does not explain why the
 * same graph built something different on your machine than it did in CI — that
 * answer is almost always a variable. So the values travel with the graph:
 * every view that draws steps can draw the variables feeding them, from the
 * same resolved report the terminal prints.
 *
 * Values whose names look like secrets arrive already masked from the daemon;
 * nothing here can un-mask them.
 */

import {
  Alert,
  AlertTitle,
  Box,
  Chip,
  Stack,
  Tooltip,
  Typography,
} from "@mui/material";
import KeyIcon from "@mui/icons-material/VpnKey";

import type { EnvReport, EnvVar } from "../api/types";
import { monoFontStack } from "../theme";

/** The value as text, including the two cases where there isn't one. */
export function envValueText(variable: EnvVar): string {
  if (variable.value !== null) return variable.value;
  return variable.varies ? "set per step" : "unset";
}

/** Where the value came from, in the words the config uses. */
export function originText(variable: EnvVar): string {
  switch (variable.origin) {
    case "environment":
      return "from the environment ciabatta was started with";
    case "env_file":
      return `from ${variable.file ?? "an env file"}`;
    case "config":
      return "from an [env] table in the config";
    default:
      return "nothing sets this — steps will read an empty string";
  }
}

/**
 * One variable as `KEY = value`.
 *
 * Monospace and full-width-tolerant, because these get compared against what a
 * shell prints and a truncated value is worse than a wrapped one.
 */
export function EnvVarChip({ variable }: { variable: EnvVar }) {
  const unset = variable.origin === "unset";
  return (
    <Tooltip
      title={
        <>
          {originText(variable)}
          {variable.required && " · REQUIRED_ENV"}
          {variable.secret && " · value hidden because the name looks like a secret"}
          {variable.steps.length > 0 && ` · used by ${variable.steps.join(", ")}`}
        </>
      }
    >
      <Chip
        size="small"
        variant="outlined"
        color={unset ? "error" : variable.required ? "warning" : "default"}
        icon={variable.secret ? <KeyIcon /> : undefined}
        label={
          <span style={{ fontFamily: monoFontStack }}>
            {variable.key}
            <Box component="span" sx={{ opacity: 0.6 }}>
              {" = "}
            </Box>
            <Box component="span" sx={{ fontStyle: variable.value === null ? "italic" : "normal" }}>
              {envValueText(variable)}
            </Box>
          </span>
        }
      />
    </Tooltip>
  );
}

/** A step's own `[env]` table, layered over the run's. */
export function StepEnvChips({ env }: { env: Record<string, string> }) {
  const keys = Object.keys(env);
  if (keys.length === 0) return null;
  return (
    <>
      {keys.map((key) => (
        <Tooltip key={key} title="Set by this step, on top of the run's environment">
          <Chip
            size="small"
            variant="outlined"
            color="info"
            label={
              <span style={{ fontFamily: monoFontStack }}>
                {key} = {env[key]}
              </span>
            }
          />
        </Tooltip>
      ))}
    </>
  );
}

/**
 * The whole environment a graph depends on: what is set, where it came from,
 * and what is still missing.
 *
 * Required variables lead, then the ones steps actually read; a variable a
 * sourced `.env` defines but nobody reads is listed last, because "this file
 * sets things nothing uses" is worth knowing but isn't the question.
 */
export function EnvPanel({ report, title = "Environment" }: { report: EnvReport; title?: string }) {
  if (report.vars.length === 0 && report.files.length === 0) return null;

  const used = report.vars.filter((v) => v.steps.length > 0 || v.required);
  const unused = report.vars.filter((v) => v.steps.length === 0 && !v.required);

  return (
    <Box>
      <Stack direction="row" spacing={1} alignItems="baseline" flexWrap="wrap" useFlexGap>
        <Typography variant="overline" color="text.secondary">
          {title} — {report.vars.length} variable(s) this graph depends on
        </Typography>
        {report.files.length > 0 && (
          <Typography variant="caption" color="text.secondary" sx={{ fontFamily: monoFontStack }}>
            sourcing {report.files.join(", ")}
          </Typography>
        )}
      </Stack>

      {report.missing.length > 0 && (
        <Alert severity="warning" sx={{ my: 1 }}>
          <AlertTitle>
            {report.missing.length} required variable(s) have no value here
          </AlertTitle>
          <Typography variant="body2" sx={{ fontFamily: monoFontStack }}>
            {report.missing.join(", ")}
          </Typography>
          <Typography variant="caption" color="text.secondary">
            Starting the run from this page will ask you for them.
          </Typography>
        </Alert>
      )}

      <Stack direction="row" spacing={0.75} sx={{ mt: 0.5 }} flexWrap="wrap" useFlexGap>
        {used.map((variable) => (
          <EnvVarChip key={variable.key} variable={variable} />
        ))}
      </Stack>

      {unused.length > 0 && (
        <Typography
          variant="caption"
          color="text.secondary"
          display="block"
          sx={{ mt: 0.75, fontFamily: monoFontStack }}
        >
          also available, unread by any step: {unused.map((v) => v.key).join(", ")}
        </Typography>
      )}
    </Box>
  );
}
