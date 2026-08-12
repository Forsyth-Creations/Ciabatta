/**
 * The landing page: what the daemon is doing right now, across every tool.
 *
 * This view didn't exist before — with six separate servers there was nowhere
 * to see a watch session and a run side by side.
 */

import {
  Box,
  Button,
  Card,
  CardActionArea,
  CardContent,
  Grid2 as Grid,
  Stack,
  Typography,
} from "@mui/material";
import ChecklistIcon from "@mui/icons-material/Checklist";
import HubIcon from "@mui/icons-material/Hub";
import MenuBookIcon from "@mui/icons-material/MenuBook";
import MonitorHeartIcon from "@mui/icons-material/MonitorHeart";
import PsychologyIcon from "@mui/icons-material/Psychology";
import RocketLaunchIcon from "@mui/icons-material/RocketLaunch";
import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";

import { PageHeader } from "../components/Page";
import { useHealth } from "../api/queries";
import { useProjectContext } from "../state/project";

interface Tool {
  to: string;
  title: string;
  blurb: string;
  icon: ReactNode;
}

const TOOLS: Tool[] = [
  {
    to: "/todo",
    title: "Todo",
    blurb: "Your personal task list, shared across every project.",
    icon: <ChecklistIcon />,
  },
  {
    to: "/watch",
    title: "Watch",
    blurb: "Run a command and stream its logs, searchable and bookmarkable.",
    icon: <MonitorHeartIcon />,
  },
  {
    to: "/run",
    title: "Run",
    blurb: "Execute a step DAG live, with fix-it branches when a step fails.",
    icon: <RocketLaunchIcon />,
  },
  {
    to: "/analyze",
    title: "Analyze",
    blurb: "Explore the codebase dependency graph and requirement traces.",
    icon: <HubIcon />,
  },
  {
    to: "/ai",
    title: "AI",
    blurb: "The architecture mind map and background assistant jobs.",
    icon: <PsychologyIcon />,
  },
];

export function DashboardPage() {
  const { data: health } = useHealth();
  const { project } = useProjectContext();

  return (
    <Box>
      <PageHeader
        title="Dashboard"
        description={
          project
            ? `Working in ${project.path}`
            : "Run a ciabatta command in a checkout to register it here."
        }
      />

      <Grid container spacing={2}>
        {TOOLS.map((tool) => (
          <Grid key={tool.to} size={{ xs: 12, sm: 6, lg: 4 }}>
            <Card sx={{ height: "100%" }}>
              <CardActionArea component={Link} to={tool.to} sx={{ height: "100%" }}>
                <CardContent>
                  <Stack direction="row" spacing={1.5} alignItems="center" sx={{ mb: 1 }}>
                    <Box sx={{ color: "primary.main", display: "flex" }}>{tool.icon}</Box>
                    <Typography variant="h3">{tool.title}</Typography>
                  </Stack>
                  <Typography variant="body2" color="text.secondary">
                    {tool.blurb}
                  </Typography>
                </CardContent>
              </CardActionArea>
            </Card>
          </Grid>
        ))}
      </Grid>

      <Stack direction="row" spacing={1} alignItems="center" sx={{ mt: 3 }}>
        <Typography variant="body2" color="text.secondary">
          New here?
        </Typography>
        <Button component={Link} to="/docs" size="small" startIcon={<MenuBookIcon />}>
          Read the docs
        </Button>
      </Stack>

      {health && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 4 }}>
          daemon v{health.version} · pid {health.pid} · started{" "}
          {new Date(health.started_at).toLocaleString()}
        </Typography>
      )}
    </Box>
  );
}
