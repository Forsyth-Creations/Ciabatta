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

import { useMemo, useState } from "react";
import {
  Alert,
  AlertTitle,
  Box,
  Chip,
  InputAdornment,
  Stack,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import SearchIcon from "@mui/icons-material/Search";
import KeyIcon from "@mui/icons-material/VpnKey";
import FingerprintIcon from "@mui/icons-material/Fingerprint";

import type { EnvReport, EnvVar } from "../api/types";
import { monoFontStack } from "../theme";

/** The value as text, including the two cases where there isn't one. */
export function envValueText(variable: EnvVar): string {
  if (variable.value !== null) return variable.value;
  if (variable.varies) return "set per step";
  return variable.optional ? "unset · optional" : "unset";
}

/**
 * Whether a variable being unset is a problem worth colouring red.
 *
 * Not for an optional one — a registry credential a push does without, an AWS
 * key when a profile or role supplies the access — where red would send
 * somebody hunting for a fault that isn't there.
 */
export function unsetProblem(variable: EnvVar): boolean {
  return variable.origin === "unset" && !variable.optional;
}

/** Where the value came from, in the words the config uses — and what it beat. */
export function originText(variable: EnvVar): string {
  const beat = variable.overrides?.length ? `, overriding ${variable.overrides.join(", ")}` : "";
  switch (variable.origin) {
    case "environment":
      return `from the terminal ciabatta was started in${beat}`;
    case "env_file":
      return `from ${variable.file ?? "an env file"}${beat}`;
    case "default":
      return `from the defaults file ${variable.file ?? ""}${beat}`;
    case "config":
      return "set by value in a workspace's ciabatta.yaml (workspace.env — deprecated, move it to the .env)";
    case "argument":
      return "a custom argument, given after `...` for this run only";
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
  const unset = unsetProblem(variable);
  return (
    <Tooltip
      title={
        <>
          {variable.purpose && `${variable.purpose} · `}
          {variable.reason ?? originText(variable)}
          {variable.optional && " · optional"}
          {variable.required && " · required_env"}
          {variable.in_cache_key && " · part of the cache key: changing it rebuilds"}
          {variable.secret && " · value hidden because the name looks like a secret"}
          {variable.steps.length > 0 && ` · used by ${variable.steps.join(", ")}`}
        </>
      }
    >
      <Chip
        size="small"
        variant="outlined"
        color={
          unset
            ? "error"
            : variable.required || variable.origin === "argument"
              ? "warning"
              : "default"
        }
        icon={variable.secret ? <KeyIcon /> : undefined}
        label={
          <span style={{ fontFamily: monoFontStack }}>
            {variable.in_cache_key && (
              <FingerprintIcon
                aria-label="keys the cache"
                sx={{ fontSize: 13, opacity: 0.7, mr: 0.5, verticalAlign: "-2px" }}
              />
            )}
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
 * The run's custom arguments — what was typed after `...` — set apart and in
 * the warning colour: they exist only in the command that started this run,
 * so the next run won't have them unless they're typed again. Says how to
 * keep them instead.
 */
export function CustomArguments({ report }: { report: EnvReport }) {
  const args = report.vars.filter((v) => v.origin === "argument");
  if (args.length === 0) return null;
  return (
    <Alert severity="warning" icon={false} sx={{ my: 1, py: 0.5 }}>
      <Typography variant="caption" sx={{ display: "block", fontWeight: 600, mb: 0.5 }}>
        ⚑ {args.length} custom argument{args.length === 1 ? "" : "s"}, for this run only
      </Typography>
      <Stack direction="row" spacing={0.75} flexWrap="wrap" useFlexGap>
        {args.map((variable) => (
          <EnvVarChip key={variable.key} variable={variable} />
        ))}
      </Stack>
      <Typography variant="caption" sx={{ display: "block", mt: 0.75 }}>
        If this run needs these every time, keep them in an env profile: put them in{" "}
        <code>{report.profile ? `.env.${report.profile}` : ".env.<name>"}</code> next to your{" "}
        <code>.env</code> and run with <code>--env-profile {report.profile ?? "<name>"}</code>.
      </Typography>
    </Alert>
  );
}

/** One labelled row of variables — nothing at all when there are none. */
function EnvGroup({ label, hint, vars }: { label: string; hint: string; vars: EnvVar[] }) {
  if (vars.length === 0) return null;
  return (
    <Stack direction="row" spacing={0.75} sx={{ mt: 0.75 }} alignItems="center" flexWrap="wrap" useFlexGap>
      <Tooltip title={hint}>
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ fontFamily: monoFontStack, minWidth: 96 }}
        >
          {label}
        </Typography>
      </Tooltip>
      {vars.map((variable) => (
        <EnvVarChip key={variable.key} variable={variable} />
      ))}
    </Stack>
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
/**
 * A report's variables in the groups a workflow file declares them in:
 * `required_env`, then `env`, then the deprecated `workspace.env`, then what
 * steps read without declaring, and last what's set but read by nothing.
 * Custom arguments are left out: they have a block of their own.
 */
function groupEnv(report: EnvReport) {
  const vars = report.vars.filter((v) => v.origin !== "argument");
  return {
    required: vars.filter((v) => v.required),
    listed: vars.filter((v) => !v.required && v.declared),
    declared: vars.filter((v) => !v.required && !v.declared && v.origin === "config"),
    read: vars.filter(
      (v) => !v.required && !v.declared && v.origin !== "config" && v.steps.length > 0,
    ),
    unused: vars.filter(
      (v) => v.steps.length === 0 && !v.required && !v.declared && v.origin !== "config",
    ),
    keyed: vars.some((v) => v.in_cache_key),
  };
}

export function EnvPanel({ report, title = "Environment" }: { report: EnvReport; title?: string }) {
  if (report.vars.length === 0 && report.files.length === 0) return null;

  const { required, listed, declared, read, unused, keyed } = groupEnv(report);

  return (
    <Box>
      <Stack direction="row" spacing={1} alignItems="baseline" flexWrap="wrap" useFlexGap>
        <Typography variant="overline" color="text.secondary">
          {title} — {report.vars.length} variable(s) this graph depends on
        </Typography>
        {(report.profile || report.profile_reason) && (
          <Tooltip title={report.profile_reason ?? ""}>
            <Chip
              size="small"
              color={report.profile ? "secondary" : "default"}
              variant="outlined"
              label={`profile: ${report.profile ?? "none"}`}
            />
          </Tooltip>
        )}
        {report.profile_reason && (
          <Typography variant="caption" color="text.secondary">
            {report.profile_reason}
          </Typography>
        )}
        {report.files.length > 0 && (
          <Typography variant="caption" color="text.secondary" sx={{ fontFamily: monoFontStack }}>
            sourcing {report.files.join(", ")}
          </Typography>
        )}
      </Stack>

      <CustomArguments report={report} />

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

      <EnvGroup
        label="required_env"
        hint="must be set, or the run won't start"
        vars={required}
      />
      <EnvGroup
        label="env"
        hint="optional — the workflow uses them, the .env (and its profile) sets them"
        vars={listed}
      />
      <EnvGroup
        label="workspace.env"
        hint="set by value in a workspace's ciabatta.yaml — deprecated: move these values to the .env"
        vars={declared}
      />
      <EnvGroup label="read by steps" hint="from the environment or a .env file" vars={read} />
      {keyed && (
        <Typography variant="caption" color="text.secondary" display="block" sx={{ mt: 0.5 }}>
          <FingerprintIcon sx={{ fontSize: 13, verticalAlign: "-2px", mr: 0.5 }} />
          part of the cache key — every <code>required_env</code> and <code>env</code> variable
          is, so changing one rebuilds the steps that see it.
        </Typography>
      )}

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

/**
 * The environment as a list: one row per variable, grouped as the workflow
 * file declares them, with a search across names, values and the steps that
 * read them. For a box with room to scroll, where the chips of `EnvPanel`
 * become a wall to read across.
 */
export function EnvList({ report }: { report: EnvReport }) {
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();
  const groups = useMemo(() => {
    const { required, listed, declared, read, unused } = groupEnv(report);
    return [
      { label: "required_env", hint: "must be set, or the run won't start", vars: required },
      { label: "env", hint: "optional — the workflow uses them, the .env sets them", vars: listed },
      {
        label: "workspace.env",
        hint: "set by value in a workspace's ciabatta.yaml — deprecated: move these to the .env",
        vars: declared,
      },
      { label: "read by steps", hint: "from the environment or a .env file", vars: read },
      { label: "unread", hint: "set, but no step reads it", vars: unused },
    ];
  }, [report]);

  const matches = (variable: EnvVar) =>
    !needle ||
    variable.key.toLowerCase().includes(needle) ||
    (variable.value ?? "").toLowerCase().includes(needle) ||
    variable.steps.some((step) => step.toLowerCase().includes(needle));
  const shown = groups
    .map((group) => ({ ...group, vars: group.vars.filter(matches) }))
    .filter((group) => group.vars.length > 0);

  if (report.vars.length === 0 && report.files.length === 0) {
    return (
      <Typography variant="caption" color="text.secondary">
        This workflow reads no environment variables.
      </Typography>
    );
  }

  return (
    <Box>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap sx={{ mb: 1 }}>
        <TextField
          size="small"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={`Search ${report.vars.length} variables`}
          sx={{ flex: "0 1 260px", minWidth: 160, "& .MuiInputBase-root": { height: 30, fontSize: 13 } }}
          slotProps={{
            htmlInput: { "aria-label": "Search the environment" },
            input: {
              startAdornment: (
                <InputAdornment position="start">
                  <SearchIcon sx={{ fontSize: 16 }} />
                </InputAdornment>
              ),
            },
          }}
        />
        {(report.profile || report.profile_reason) && (
          <Tooltip title={report.profile_reason ?? ""}>
            <Chip
              size="small"
              color={report.profile ? "secondary" : "default"}
              variant="outlined"
              label={`profile: ${report.profile ?? "none"}`}
            />
          </Tooltip>
        )}
        {report.files.length > 0 && (
          <Typography variant="caption" color="text.secondary" sx={{ fontFamily: monoFontStack }}>
            sourcing {report.files.join(", ")}
          </Typography>
        )}
      </Stack>

      <CustomArguments report={report} />

      {report.missing.length > 0 && (
        <Alert severity="warning" sx={{ mb: 1, py: 0 }}>
          No value for required {report.missing.join(", ")}
        </Alert>
      )}

      {shown.length === 0 && (
        <Typography variant="caption" color="text.secondary">
          No variable matches “{query.trim()}”.
        </Typography>
      )}

      {shown.map((group) => (
        <Box key={group.label} sx={{ mb: 1 }}>
          <Tooltip title={group.hint} placement="top-start">
            <Typography
              variant="overline"
              color="text.secondary"
              // As written in the workflow file, not shouted: these are keys.
              sx={{ display: "block", lineHeight: 2, fontFamily: monoFontStack, textTransform: "none" }}
            >
              {group.label} · {group.vars.length}
            </Typography>
          </Tooltip>
          {group.vars.map((variable) => (
            <EnvRow key={variable.key} variable={variable} />
          ))}
        </Box>
      ))}
    </Box>
  );
}

/** One variable in `EnvList`: name, value, and what it's for on hover. */
function EnvRow({ variable }: { variable: EnvVar }) {
  const unset = unsetProblem(variable);
  return (
    // Below the row and after a moment, so running the pointer down the list
    // doesn't flash a tooltip over the search box at every row.
    <Tooltip
      placement="bottom-start"
      enterDelay={400}
      enterNextDelay={400}
      title={
        <>
          {variable.purpose && `${variable.purpose} · `}
          {variable.reason ?? originText(variable)}
          {variable.in_cache_key && " · part of the cache key: changing it rebuilds"}
          {variable.secret && " · value hidden because the name looks like a secret"}
          {variable.steps.length > 0 && ` · used by ${variable.steps.join(", ")}`}
        </>
      }
    >
      <Box
        sx={{
          display: "grid",
          gridTemplateColumns: "minmax(120px, 240px) minmax(0, 1fr) auto",
          columnGap: 2,
          alignItems: "center",
          px: 1,
          py: 0.25,
          borderRadius: 0.5,
          fontFamily: monoFontStack,
          fontSize: 12.5,
          "&:hover": { bgcolor: "action.hover" },
        }}
      >
        <Box
          component="span"
          sx={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
        >
          {variable.secret && (
            <KeyIcon sx={{ fontSize: 13, mr: 0.5, verticalAlign: "-2px", color: "warning.main" }} />
          )}
          {variable.key}
        </Box>
        <Box
          component="span"
          sx={{
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            color: unset ? "error.main" : variable.value === null ? "text.disabled" : "text.secondary",
            fontStyle: variable.value === null ? "italic" : "normal",
          }}
        >
          {envValueText(variable)}
        </Box>
        <Stack direction="row" spacing={1} alignItems="center" sx={{ color: "text.disabled" }}>
          {variable.steps.length > 0 && (
            <Typography variant="caption" sx={{ fontFamily: "inherit" }}>
              {variable.steps.length} step{variable.steps.length === 1 ? "" : "s"}
            </Typography>
          )}
          {variable.in_cache_key && (
            <FingerprintIcon aria-label="keys the cache" sx={{ fontSize: 14 }} />
          )}
        </Stack>
      </Box>
    </Tooltip>
  );
}
