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
import AutorenewIcon from "@mui/icons-material/Autorenew";
import RemoveCircleOutlineIcon from "@mui/icons-material/RemoveCircleOutline";
import RadioButtonUncheckedIcon from "@mui/icons-material/RadioButtonUnchecked";
import PauseCircleOutlineIcon from "@mui/icons-material/PauseCircleOutline";
import Tooltip from "@mui/material/Tooltip";
import type { SvgIconComponent } from "@mui/icons-material";
import type { Theme } from "@mui/material/styles";

import type { StepStatus } from "../api/run";

/** A run's verdict, which is a step status plus "somebody stopped it". */
export type RunStatus = StepStatus | "stopped";

interface Look {
  Icon: SvgIconComponent;
  colour: (theme: Theme) => string;
  label: string;
  /** Whether it turns — reserved for the one state that is still changing. */
  spins?: boolean;
}

const LOOKS: Record<RunStatus, Look> = {
  running: {
    Icon: AutorenewIcon,
    colour: (theme) => theme.palette.warning.main,
    label: "running",
    spins: true,
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
      }}
    />
  );

  if (title === null) return icon;
  return <Tooltip title={title ?? look.label}>{icon}</Tooltip>;
}
