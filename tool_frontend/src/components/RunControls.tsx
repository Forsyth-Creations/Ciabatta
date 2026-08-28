/**
 * The controls a run in flight needs: what state it's in, and how to stop it.
 *
 * Shared by the run list and the run detail page, which show the same two
 * things about the same run and must not disagree about either.
 */

import { Button, Chip } from "@mui/material";
import StopIcon from "@mui/icons-material/Stop";

import { useStopRun, type RunSummary } from "../api/run";

/** Running, stopping, or finished — the three states a run can be seen in. */
export function RunStatusChip({ run }: { run: RunSummary }) {
  const stopping = run.stopping && !run.done;
  return (
    <Chip
      size="small"
      variant="outlined"
      color={run.done ? "default" : stopping ? "warning" : "success"}
      label={run.done ? "finished" : stopping ? "stopping" : "running"}
    />
  );
}

/**
 * Stop a run in flight.
 *
 * The daemon owns the run, so nothing else can stop it — closing the tab, or
 * the terminal that started it, leaves the build going. Pressing it kills the
 * step that's executing along with everything that step spawned, and the graph
 * reports the rest as never having run.
 *
 * It disables itself once pressed: the switch is already thrown, and pressing
 * it again is not an escalation.
 */
export function StopButton({ run }: { run: RunSummary }) {
  const stop = useStopRun();
  const stopping = run.stopping || stop.isPending;
  return (
    <Button
      size="small"
      color="warning"
      startIcon={<StopIcon />}
      disabled={stopping}
      onClick={() => stop.mutate(run.id)}
    >
      {stopping ? "Stopping…" : "Stop"}
    </Button>
  );
}
