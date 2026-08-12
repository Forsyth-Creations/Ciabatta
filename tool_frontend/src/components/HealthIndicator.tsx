/**
 * Whether the daemon behind this page is still answering.
 *
 * Worth showing prominently: the page is served *by* the daemon, so if it goes
 * away the tab keeps rendering happily while every action silently fails. The
 * health query retries forever, so this recovers on its own after a restart.
 */

import { Chip, Tooltip } from "@mui/material";
import CircleIcon from "@mui/icons-material/Circle";

import { useHealth } from "../api/queries";

export function HealthIndicator() {
  const { data, isError, isLoading } = useHealth();

  if (isLoading) {
    return <Chip size="small" variant="outlined" label="connecting…" />;
  }

  if (isError || !data) {
    return (
      <Tooltip title="The daemon isn't answering. Start it with `ciabatta daemon serve`, or just run any ciabatta command.">
        <Chip
          size="small"
          color="error"
          variant="outlined"
          icon={<CircleIcon sx={{ fontSize: 10 }} />}
          label="daemon down"
        />
      </Tooltip>
    );
  }

  return (
    <Tooltip title={`pid ${data.pid} · up since ${new Date(data.started_at).toLocaleString()}`}>
      <Chip
        size="small"
        color="success"
        variant="outlined"
        icon={<CircleIcon sx={{ fontSize: 10 }} />}
        label={`v${data.version}`}
      />
    </Tooltip>
  );
}
