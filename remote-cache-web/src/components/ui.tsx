import type { ReactNode } from "react";
import {
  Box,
  Card,
  CardContent,
  Chip,
  TableCell,
  TableRow,
  Tooltip,
  Typography,
  useTheme,
} from "@mui/material";

import { ago } from "../format";
import { chartColors, monoFontStack } from "../theme";

export function useChartColors() {
  return chartColors(useTheme().palette.mode);
}

/** A section heading with a one-line explanation under it. */
export function Section({
  title,
  sub,
  action,
  children,
}: {
  title: string;
  sub?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Box component="section" sx={{ mt: 4 }}>
      <Box sx={{ display: "flex", alignItems: "flex-end", gap: 2, flexWrap: "wrap", mb: 1.5 }}>
        <Box sx={{ flex: 1, minWidth: 200 }}>
          <Typography variant="h2">{title}</Typography>
          {sub && (
            <Typography variant="body2" color="text.secondary">
              {sub}
            </Typography>
          )}
        </Box>
        {action}
      </Box>
      {children}
    </Box>
  );
}

/** A KPI: label, headline value, and the context that makes it mean something. */
export function StatTile({
  label,
  value,
  detail,
  children,
}: {
  label: string;
  value: ReactNode;
  detail?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Card sx={{ height: "100%" }}>
      <CardContent sx={{ py: 1.5, "&:last-child": { pb: 1.5 } }}>
        <Typography variant="overline" color="text.secondary" sx={{ lineHeight: 1.6 }}>
          {label}
        </Typography>
        <Typography
          sx={{ fontSize: "1.6rem", fontWeight: 600, lineHeight: 1.2 }}
          noWrap
          component="div"
        >
          {value}
        </Typography>
        {detail && (
          <Typography variant="caption" color="text.secondary" noWrap component="div">
            {detail}
          </Typography>
        )}
        {children}
      </CardContent>
    </Card>
  );
}

/** A titled card that holds one chart. */
export function ChartCard({
  title,
  sub,
  action,
  children,
}: {
  title: string;
  sub?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Card sx={{ height: "100%" }}>
      <CardContent>
        <Box sx={{ display: "flex", alignItems: "flex-start", gap: 1, mb: 1 }}>
          <Box sx={{ flex: 1 }}>
            <Typography variant="h3">{title}</Typography>
            {sub && (
              <Typography variant="caption" color="text.secondary">
                {sub}
              </Typography>
            )}
          </Box>
          {action}
        </Box>
        {children}
      </CardContent>
    </Card>
  );
}

/** Shown where a chart would be, when there's nothing to plot yet. */
export function NoData({ height, children }: { height: number; children: ReactNode }) {
  return (
    <Box
      sx={{
        height,
        display: "grid",
        placeItems: "center",
        textAlign: "center",
        px: 3,
        color: "text.secondary",
        border: 1,
        borderColor: "divider",
        borderStyle: "dashed",
        borderRadius: 1,
      }}
    >
      <Typography variant="body2">{children}</Typography>
    </Box>
  );
}

/** A relative time with the absolute one on hover. */
export function When({ at }: { at: string | null | undefined }) {
  if (!at) {
    return (
      <Typography component="span" variant="inherit" color="text.secondary">
        never
      </Typography>
    );
  }
  return (
    <Tooltip title={new Date(at).toLocaleString()}>
      <span>{ago(at)}</span>
    </Tooltip>
  );
}

export function Mono({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <Box component="span" title={title} sx={{ fontFamily: monoFontStack, fontSize: "0.85em" }}>
      {children}
    </Box>
  );
}

const TAG_COLOURS: Record<string, "success" | "warning" | "error" | "default"> = {
  hit: "success",
  upload: "success",
  success: "success",
  reused: "success",
  miss: "warning",
  stopped: "warning",
  skipped: "warning",
  failed: "error",
};

export function Tag({ kind, label }: { kind: string; label?: string }) {
  return (
    <Chip
      size="small"
      variant="outlined"
      color={TAG_COLOURS[kind] ?? "default"}
      label={label ?? kind}
      sx={{ height: 22 }}
    />
  );
}

export function EmptyRow({ colSpan, children }: { colSpan: number; children: ReactNode }) {
  return (
    <TableRow>
      <TableCell colSpan={colSpan} sx={{ color: "text.secondary" }}>
        {children}
      </TableCell>
    </TableRow>
  );
}

/** Backticked names in server prose read as code. */
export function Prose({ text }: { text: string }) {
  const parts = text.split(/`([^`]+)`/g);
  return <>{parts.map((part, i) => (i % 2 === 1 ? <Mono key={i}>{part}</Mono> : part))}</>;
}
