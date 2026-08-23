/**
 * The frame every page renders inside: nav rail on the left, project switcher
 * and daemon health on the top bar.
 *
 * This is the piece that makes the six tools feel like one product — before,
 * each app had its own layout, its own colours, and its own port.
 *
 * The nav collapses two different ways, because the two cases want different
 * behaviour:
 *
 * - **Narrow screens** get a temporary overlay drawer, closed by default, that
 *   dismisses itself when you pick a destination. Several pages (watch logs,
 *   the analyze graph, run logs) are wide, and a permanent 232px rail
 *   costs more than it's worth below ~900px.
 * - **Wide screens** get a permanent rail you can still collapse by hand, which
 *   is remembered — useful when you want the full width for a log view.
 */

import {
  AppBar,
  Box,
  Chip,
  Divider,
  Drawer,
  IconButton,
  List,
  ListItemButton,
  ListItemIcon,
  ListItemText,
  Toolbar,
  Tooltip,
  Typography,
  useMediaQuery,
} from "@mui/material";
import { useTheme } from "@mui/material/styles";
import AccountTreeIcon from "@mui/icons-material/AccountTree";
import ChecklistIcon from "@mui/icons-material/Checklist";
import DarkModeIcon from "@mui/icons-material/DarkMode";
import HubIcon from "@mui/icons-material/Hub";
import InventoryIcon from "@mui/icons-material/Inventory2";
import LightModeIcon from "@mui/icons-material/LightMode";
import MenuBookIcon from "@mui/icons-material/MenuBook";
import MenuIcon from "@mui/icons-material/Menu";
import MonitorHeartIcon from "@mui/icons-material/MonitorHeart";
import PsychologyIcon from "@mui/icons-material/Psychology";
import RocketLaunchIcon from "@mui/icons-material/RocketLaunch";
import SpaceDashboardIcon from "@mui/icons-material/SpaceDashboard";
import { Link, useRouterState } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";

import { useColorMode } from "../state/colorMode";
import { ProjectSwitcher } from "./ProjectSwitcher";
import { HealthIndicator } from "./HealthIndicator";

const DRAWER_WIDTH = 232;
const NAV_STORAGE_KEY = "ciabatta-nav-open";

/** One entry per tool the daemon absorbed, plus the dashboard. */
const NAV_ITEMS = [
  { to: "/", label: "Dashboard", icon: <SpaceDashboardIcon />, exact: true },
  { to: "/todo", label: "Todo", icon: <ChecklistIcon /> },
  { to: "/watch", label: "Watch", icon: <MonitorHeartIcon /> },
  { to: "/workspace", label: "Workspace", icon: <AccountTreeIcon /> },
  { to: "/run", label: "Run", icon: <RocketLaunchIcon /> },
  { to: "/cache", label: "Cache", icon: <InventoryIcon /> },
  { to: "/analyze", label: "Analyze", icon: <HubIcon /> },
  { to: "/ai", label: "AI", icon: <PsychologyIcon /> },
] as const;

/**
 * Entries that aren't a tool. Kept out of `NAV_ITEMS` and pinned to the bottom
 * of the rail so the tool list stays a list of tools.
 */
const FOOTER_ITEMS = [{ to: "/docs", label: "Docs", icon: <MenuBookIcon /> }] as const;

export function AppShell({ children }: { children: ReactNode }) {
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const { mode, toggle: onToggleMode } = useColorMode();

  const theme = useTheme();
  const isNarrow = useMediaQuery(theme.breakpoints.down("md"));

  // On wide screens the rail starts open unless it was collapsed last time.
  const [open, setOpen] = useState(() => localStorage.getItem(NAV_STORAGE_KEY) !== "false");

  // Crossing the breakpoint resets to that layout's sensible default: hidden on
  // narrow, remembered on wide. Without this, shrinking the window would leave
  // an overlay drawer already open on top of the content.
  useEffect(() => {
    setOpen(isNarrow ? false : localStorage.getItem(NAV_STORAGE_KEY) !== "false");
  }, [isNarrow]);

  const toggleNav = () => {
    const next = !open;
    setOpen(next);
    // Only the wide-screen preference is worth remembering.
    if (!isNarrow) localStorage.setItem(NAV_STORAGE_KEY, String(next));
  };

  const nav = (
    <Box sx={{ overflow: "auto", display: "flex", flexDirection: "column", height: "100%" }}>
      <List sx={{ px: 1 }}>
        {NAV_ITEMS.map((item) => (
          <ListItemButton
            key={item.to}
            component={Link}
            to={item.to}
            selected={isActive(pathname, item.to, "exact" in item && item.exact)}
            // Picking a destination from the overlay should get out of the way.
            onClick={() => isNarrow && setOpen(false)}
            sx={{ borderRadius: 1, mb: 0.25 }}
          >
            <ListItemIcon sx={{ minWidth: 38 }}>{item.icon}</ListItemIcon>
            <ListItemText primary={item.label} />
          </ListItemButton>
        ))}
      </List>

      <Box sx={{ flexGrow: 1 }} />
      <Divider />
      <List sx={{ px: 1 }}>
        {FOOTER_ITEMS.map((item) => (
          <ListItemButton
            key={item.to}
            component={Link}
            to={item.to}
            selected={isActive(pathname, item.to)}
            onClick={() => isNarrow && setOpen(false)}
            sx={{ borderRadius: 1 }}
          >
            <ListItemIcon sx={{ minWidth: 38 }}>{item.icon}</ListItemIcon>
            <ListItemText primary={item.label} />
          </ListItemButton>
        ))}
      </List>
      <Divider />
      <Box sx={{ p: 2 }}>
        <Chip size="small" variant="outlined" label="one daemon · one app" />
      </Box>
    </Box>
  );

  return (
    <Box sx={{ display: "flex", minHeight: "100vh" }}>
      <AppBar position="fixed" sx={{ zIndex: (t) => t.zIndex.drawer + 1 }}>
        <Toolbar sx={{ gap: { xs: 1, sm: 2 } }}>
          <Tooltip title={open ? "Hide navigation" : "Show navigation"}>
            <IconButton
              edge="start"
              onClick={toggleNav}
              size="small"
              aria-label="Toggle navigation"
              aria-expanded={open}
            >
              <MenuIcon />
            </IconButton>
          </Tooltip>

          <Typography
            variant="h3"
            component="div"
            sx={{ display: "flex", gap: 1, whiteSpace: "nowrap" }}
          >
            <span aria-hidden>🍞</span>
            <Box component="span" sx={{ display: { xs: "none", sm: "inline" } }}>
              ciabatta
            </Box>
          </Typography>

          <Box sx={{ flexGrow: 1 }} />

          <ProjectSwitcher />
          <HealthIndicator />

          <Tooltip title={mode === "dark" ? "Switch to light" : "Switch to dark"}>
            <IconButton onClick={onToggleMode} size="small" aria-label="Toggle colour mode">
              {mode === "dark" ? <LightModeIcon /> : <DarkModeIcon />}
            </IconButton>
          </Tooltip>
        </Toolbar>
      </AppBar>

      <Drawer
        // Temporary drawers float over the content and dim it; permanent ones
        // reserve layout space. Below md the space isn't there to reserve.
        variant={isNarrow ? "temporary" : "persistent"}
        open={open}
        onClose={() => setOpen(false)}
        ModalProps={{ keepMounted: true }}
        sx={{
          // Constant width: a persistent drawer keeps its docked footprint when
          // closed, and `main`'s negative margin below is what reclaims it.
          // A temporary drawer is position-fixed, so this reserves nothing.
          width: DRAWER_WIDTH,
          flexShrink: 0,
          "& .MuiDrawer-paper": { width: DRAWER_WIDTH, boxSizing: "border-box" },
        }}
      >
        <Toolbar />
        {nav}
      </Drawer>

      <Box
        component="main"
        sx={{
          flexGrow: 1,
          p: { xs: 2, md: 3 },
          minWidth: 0,
          // A persistent drawer doesn't push content, so reclaim the space
          // ourselves when it's collapsed.
          transition: theme.transitions.create("margin", {
            easing: theme.transitions.easing.sharp,
            duration: theme.transitions.duration.leavingScreen,
          }),
          ...(!isNarrow && !open && { marginLeft: `-${DRAWER_WIDTH}px` }),
        }}
      >
        <Toolbar />
        {children}
      </Box>
    </Box>
  );
}

/**
 * Highlight a nav entry when it owns the current path. Only the dashboard
 * matches exactly; the rest stay lit on their sub-routes (`/watch/3`).
 */
function isActive(pathname: string, to: string, exact = false): boolean {
  if (exact) return pathname === to;
  return pathname === to || pathname.startsWith(`${to}/`);
}
