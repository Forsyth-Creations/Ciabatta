/**
 * Small layout primitives shared by every page, so the six tools stay visually
 * consistent without each one re-inventing a header.
 */

import { Alert, Box, CircularProgress, Stack, Typography } from "@mui/material";
import type { ReactNode } from "react";

import { useProjectContext } from "../state/project";

interface PageHeaderProps {
  title: string;
  description?: string;
  actions?: ReactNode;
}

export function PageHeader({ title, description, actions }: PageHeaderProps) {
  return (
    <Stack
      direction="row"
      alignItems="flex-start"
      justifyContent="space-between"
      spacing={2}
      sx={{ mb: 3 }}
    >
      <Box>
        <Typography variant="h1">{title}</Typography>
        {description && (
          <Typography variant="body2" color="text.secondary" sx={{ mt: 0.5, maxWidth: "60ch" }}>
            {description}
          </Typography>
        )}
      </Box>
      {actions && (
        <Stack direction="row" spacing={1} sx={{ flexShrink: 0 }}>
          {actions}
        </Stack>
      )}
    </Stack>
  );
}

export function Loading({ label = "Loading…" }: { label?: string }) {
  return (
    <Stack direction="row" spacing={1.5} alignItems="center" sx={{ py: 4 }}>
      <CircularProgress size={20} />
      <Typography variant="body2" color="text.secondary">
        {label}
      </Typography>
    </Stack>
  );
}

/** Renders an API error the way the daemon reported it. */
export function ErrorNote({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    <Alert severity="error" sx={{ my: 2 }}>
      {message}
    </Alert>
  );
}

/**
 * Gate for pages whose data is per-project. Shows a useful prompt instead of
 * firing a request that would fail when nothing is registered yet.
 */
export function RequireProject({ children }: { children: (projectId: string) => ReactNode }) {
  const { projectId, isLoading } = useProjectContext();

  if (isLoading) return <Loading label="Finding your projects…" />;

  if (!projectId) {
    return (
      <Alert severity="info">
        No project is registered yet. Run any ciabatta command inside a checkout —
        for example <code>ciabatta analyze</code> — and it will appear in the
        switcher above.
      </Alert>
    );
  }

  return <>{children(projectId)}</>;
}
