import { useState } from "react";
import {
  Alert,
  Button,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  Typography,
} from "@mui/material";
import SyncIcon from "@mui/icons-material/Sync";

import { api } from "../api";
import { human } from "../format";
import { useLoad } from "../useLoad";
import { Mono, Section, Tag, When } from "./ui";

/** One of the server's `peers:`, as `GET /api/peers` reports it. */
export interface PeerStatus {
  url: string;
  is_self?: boolean;
  last_sync_at: string | null;
  last_ok_at: string | null;
  last_error: string | null;
  pulled_entries: number;
  pulled_bytes: number;
  pulled_builds: number;
  entries: number | null;
}

interface Peers {
  instance: string;
  sync_every: string;
  peers: PeerStatus[];
}

/**
 * The other servers this one pulls from (`peers:` in its config): whether
 * each synced, when, and what came over. "Sync now" runs a round straight
 * away rather than waiting for the next tick.
 */
export function PeersSection({ refreshKey }: { refreshKey: number }) {
  const { data, error, reload } = useLoad(
    () => api<Peers>("/api/peers"),
    refreshKey,
  );
  const [syncing, setSyncing] = useState(false);
  const [syncError, setSyncError] = useState<string | null>(null);

  // A server from before peers answers 404; say nothing rather than fail.
  if (error && !data) return null;
  if (!data) return null;

  const peers = data.peers;
  const syncNow = async () => {
    setSyncing(true);
    setSyncError(null);
    try {
      await api("/api/peers/sync", { method: "POST", body: "{}" });
      reload();
    } catch (e) {
      setSyncError((e as Error).message);
    } finally {
      setSyncing(false);
    }
  };

  return (
    <Section
      title="Peers"
      sub={
        peers.length
          ? `The other servers holding this cache. This one pulls what it lacks from each every ${data.sync_every}, and as soon as one is written to.`
          : "No peers. List the other servers holding this cache under `peers:` in this server's config and it keeps itself in step with them."
      }
      action={
        peers.length > 0 && (
          <Button
            size="small"
            variant="outlined"
            startIcon={<SyncIcon />}
            disabled={syncing}
            onClick={syncNow}
          >
            {syncing ? "Syncing…" : "Sync now"}
          </Button>
        )
      }
    >
      {syncError && (
        <Alert severity="error" sx={{ mb: 1.5 }}>
          {syncError}
        </Alert>
      )}
      {peers.length > 0 && (
        <TableContainer>
          <Table size="small">
            <TableHead>
              <TableRow>
                <TableCell>Server</TableCell>
                <TableCell>State</TableCell>
                <TableCell>Last synced</TableCell>
                <TableCell align="right">Holds</TableCell>
                <TableCell align="right">Pulled since start</TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {peers.map((peer) => (
                <TableRow key={peer.url}>
                  <TableCell>
                    <Mono>{peer.url}</Mono>
                    {peer.last_error && (
                      <Typography
                        variant="caption"
                        color="error"
                        sx={{ display: "block" }}
                      >
                        {peer.last_error}
                      </Typography>
                    )}
                  </TableCell>
                  <TableCell>
                    {peer.is_self ? (
                      <Tag kind="info" label="this server" />
                    ) : peer.last_error ? (
                      <Tag kind="failed" label="failing" />
                    ) : peer.last_ok_at ? (
                      <Tag kind="success" label="in step" />
                    ) : (
                      <Tag kind="default" label="not yet synced" />
                    )}
                  </TableCell>
                  <TableCell>
                    {peer.is_self ? "—" : <When at={peer.last_sync_at} />}
                  </TableCell>
                  <TableCell align="right">
                    {peer.entries == null
                      ? "—"
                      : `${peer.entries} entr${peer.entries === 1 ? "y" : "ies"}`}
                  </TableCell>
                  <TableCell align="right">
                    {peer.is_self
                      ? "—"
                      : `${peer.pulled_entries} entr${peer.pulled_entries === 1 ? "y" : "ies"} (${human(peer.pulled_bytes)}), ${peer.pulled_builds} build${peer.pulled_builds === 1 ? "" : "s"}`}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableContainer>
      )}
    </Section>
  );
}
