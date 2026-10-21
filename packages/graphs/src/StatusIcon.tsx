/**
 * The one icon a step's (or a run's) status is drawn as, everywhere it appears.
 *
 * Status used to be carried by colour alone — a coloured border on a graph node,
 * a coloured chip in the run list. Colour is a poor sole channel for this: it
 * needs a legend, it collapses for anyone who can't separate red from green, and
 * on a node the border is competing with the focus ring for the same edge of the
 * same box. A glyph says which of five states this is without any of that, and
 * keeping it in one place is what stops the graph and the list from disagreeing
 * about what "done" looks like.
 */

import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import WarningIcon from "@mui/icons-material/Warning";
import AutorenewIcon from "@mui/icons-material/Autorenew";
import CloudDownloadIcon from "@mui/icons-material/CloudDownload";
import HourglassTopIcon from "@mui/icons-material/HourglassTop";
import RemoveCircleOutlineIcon from "@mui/icons-material/RemoveCircleOutline";
import RadioButtonUncheckedIcon from "@mui/icons-material/RadioButtonUnchecked";
import PauseCircleOutlineIcon from "@mui/icons-material/PauseCircleOutline";
import Tooltip from "@mui/material/Tooltip";
import type { SvgIconComponent } from "@mui/icons-material";
import type { Theme } from "@mui/material/styles";

/** A step's state, as the engine reports it. */
export type StepStatus =
  | "pending"
  | "running"
  | "processing"
  | "restoring"
  | "success"
  | "failed"
  | "skipped"
  | "warning";

/** A run's verdict, which is a step status plus "somebody stopped it". */
export type RunStatus = StepStatus | "stopped";

interface Look {
  Icon: SvgIconComponent;
  colour: (theme: Theme) => string;
  label: string;
  /** Whether it turns — reserved for the one state that is still changing. */
  spins?: boolean;
  /** Whether it fades in and out: changing, but not by doing work. */
  pulses?: boolean;
}

const LOOKS: Record<RunStatus, Look> = {
  running: {
    Icon: AutorenewIcon,
    colour: (theme) => theme.palette.warning.main,
    label: "running",
    spins: true,
  },
  // The cache is deciding whether it can be reused — hashing its inputs,
  // resolving its Cargo closure. Work, but not the step's own: info-coloured
  // like a restore, so a long wait doesn't read as a long build.
  processing: {
    Icon: HourglassTopIcon,
    colour: (theme) => theme.palette.info.main,
    label: "processing",
    pulses: true,
  },
  // Its outputs are coming down from the remote cache: in motion, like
  // running, but nothing is being built — drawn in the info colour so a long
  // download doesn't read as a long build.
  restoring: {
    Icon: CloudDownloadIcon,
    colour: (theme) => theme.palette.info.main,
    label: "downloading from the remote cache",
    pulses: true,
  },
  success: {
    Icon: CheckCircleIcon,
    colour: (theme) => theme.palette.success.main,
    label: "succeeded",
  },
  failed: {
    Icon: ErrorIcon,
    colour: (theme) => theme.palette.error.main,
    label: "failed",
  },
  // Failed, but marked `warn_on_error`: it counts as done, so the run carries
  // on — drawn as its own state, because "passed" would hide the failure and
  // "failed" would say the run stopped when it didn't.
  warning: {
    Icon: WarningIcon,
    colour: (theme) => theme.palette.warning.main,
    label: "failed as a warning — the run carried on",
  },
  skipped: {
    Icon: RemoveCircleOutlineIcon,
    colour: (theme) => theme.palette.text.disabled,
    label: "skipped",
  },
  stopped: {
    Icon: PauseCircleOutlineIcon,
    colour: (theme) => theme.palette.text.secondary,
    label: "stopped",
  },
  pending: {
    Icon: RadioButtonUncheckedIcon,
    colour: (theme) => theme.palette.text.disabled,
    label: "not started yet",
  },
};

/** The colour a status is drawn in, for the things that aren't the icon. */
export function statusColour(status: string, theme: Theme): string {
  return (LOOKS[status as RunStatus] ?? LOOKS.pending).colour(theme);
}

/** What a status is called, for a tooltip or a sentence. */
export function statusLabel(status: string): string {
  return (LOOKS[status as RunStatus] ?? LOOKS.pending).label;
}

export function StatusIcon({
  status,
  size = 16,
  title,
}: {
  status: string;
  size?: number;
  /** Override the tooltip, or pass null for none — inside a node label, where
   *  the node has its own hover behaviour. */
  title?: string | null;
}) {
  const look = LOOKS[status as RunStatus] ?? LOOKS.pending;
  const icon = (
    <look.Icon
      sx={{
        fontSize: size,
        flexShrink: 0,
        color: look.colour,
        ...(look.spins
          ? {
              animation: "ciabatta-spin 1.6s linear infinite",
              "@keyframes ciabatta-spin": {
                from: { transform: "rotate(0deg)" },
                to: { transform: "rotate(360deg)" },
              },
            }
          : {}),
        ...(look.pulses
          ? {
              animation: "ciabatta-pulse 1.4s ease-in-out infinite",
              "@keyframes ciabatta-pulse": {
                "0%, 100%": { opacity: 1 },
                "50%": { opacity: 0.35 },
              },
            }
          : {}),
      }}
    />
  );

  if (title === null) return icon;
  return <Tooltip title={title ?? look.label}>{icon}</Tooltip>;
}
