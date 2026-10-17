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
 * - **Wide screens** get a permanent rail you can collapse by hand to its
 *   icons alone, which is remembered — useful when you want the width for a
 *   log view without losing one-click navigation. Collapsed, each icon names
 *   itself in a tooltip.
 */

import {
  AppBar,
  Badge,
  Box,
  Chip,
  CircularProgress,
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
import ArticleIcon from "@mui/icons-material/Article";
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

import { useActiveRuns } from "../api/run";
import { useColorMode } from "../state/colorMode";
import { ProjectSwitcher } from "./ProjectSwitcher";
import { HealthIndicator } from "./HealthIndicator";
import { InputPrompt } from "./InputPrompt";

const DRAWER_WIDTH = 232;
/** Wide enough for an icon and its button's padding, and no more. */
const MINI_WIDTH = 60;
const NAV_STORAGE_KEY = "ciabatta-nav-open";

/**
 * One entry per tool the daemon absorbed, plus the selected project's
 * dashboard and README. All of it is about one project; choosing which is the
 * landing page's job, reached from the logo rather than from this list.
 */
const NAV_ITEMS = [
  { to: "/dashboard", label: "Dashboard", icon: <SpaceDashboardIcon /> },
  { to: "/readme", label: "README", icon: <ArticleIcon /> },
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

  // Collapsed on a wide screen: the rail stays, down to its icons. On a
  // narrow one, closed means the overlay is gone entirely.
  const mini = !isNarrow && !open;
  const width = mini ? MINI_WIDTH : DRAWER_WIDTH;
  const widthTransition = theme.transitions.create("width", {
    easing: theme.transitions.easing.sharp,
    duration: mini
      ? theme.transitions.duration.leavingScreen
      : theme.transitions.duration.enteringScreen,
  });

  // Runs in progress, across every project: the Run icon carries a spinner
  // while there are any, so a build left running is never out of sight.
  const running = useActiveRuns();

  const entry = (item: NavItem, selected: boolean) => {
    const busy = item.to === "/run" && running > 0;
    const icon = busy ? (
      <Badge
        overlap="circular"
        anchorOrigin={{ vertical: "top", horizontal: "right" }}
        badgeContent={<CircularProgress size={10} thickness={6} color="inherit" />}
        sx={{ "& .MuiBadge-badge": { p: 0, minWidth: 0, height: "auto", color: "warning.main" } }}
      >
        {item.icon}
      </Badge>
    ) : (
      item.icon
    );
    const busyNote = busy ? ` — ${running} run${running === 1 ? "" : "s"} in progress` : "";
    return (
      <Tooltip
        key={item.to}
        title={mini || busy ? `${item.label}${busyNote}` : ""}
        placement="right"
      >
        <ListItemButton
          component={Link}
          to={item.to}
          selected={selected}
          aria-label={`${item.label}${busyNote}`}
          // Picking a destination from the overlay should get out of the way.
          onClick={() => isNarrow && setOpen(false)}
          sx={{ borderRadius: 1, mb: 0.25, justifyContent: mini ? "center" : "flex-start" }}
        >
          <ListItemIcon sx={{ minWidth: mini ? 0 : 38 }}>{icon}</ListItemIcon>
          {!mini && <ListItemText primary={item.label} />}
        </ListItemButton>
      </Tooltip>
    );
  };

  const nav = (
    <Box
      sx={{
        overflowY: "auto",
        overflowX: "hidden",
        display: "flex",
        flexDirection: "column",
        height: "100%",
      }}
    >
      <List sx={{ px: 1 }}>
        {NAV_ITEMS.map((item) =>
          entry(item, isActive(pathname, item.to)),
        )}
      </List>

      <Box sx={{ flexGrow: 1 }} />
      <Divider />
      <List sx={{ px: 1 }}>
        {FOOTER_ITEMS.map((item) => entry(item, isActive(pathname, item.to)))}
      </List>
      {!mini && (
        <>
          <Divider />
          <Box sx={{ p: 2 }}>
            <Chip size="small" variant="outlined" label="one daemon · one app" />
          </Box>
        </>
      )}
    </Box>
  );

  return (
    <Box sx={{ display: "flex", minHeight: "100vh" }}>
      {/* A step asking a question reaches the operator on every page. */}
      <InputPrompt />
      <AppBar position="fixed" sx={{ zIndex: (t) => t.zIndex.drawer + 1 }}>
        <Toolbar sx={{ gap: { xs: 1, sm: 2 } }}>
          <Tooltip
            title={
              isNarrow
                ? open
                  ? "Hide navigation"
                  : "Show navigation"
                : open
                  ? "Collapse navigation to icons"
                  : "Expand navigation"
            }
          >
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

          {/* The way to the landing page, where projects are picked: it isn't
              in the rail, because nothing else there is above the project. */}
          <Typography
            variant="h3"
            component={Link}
            to="/"
            aria-label="ciabatta — all projects"
            sx={{
              display: "flex",
              gap: 1,
              whiteSpace: "nowrap",
              color: "inherit",
              textDecoration: "none",
              "&:hover": { opacity: 0.8 },
            }}
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
        variant={isNarrow ? "temporary" : "permanent"}
        open={isNarrow ? open : true}
        onClose={() => setOpen(false)}
        ModalProps={{ keepMounted: true }}
        sx={{
          // The docked footprint follows the rail, so `main` just takes what's
          // left. A temporary drawer is position-fixed: this reserves nothing.
          width,
          flexShrink: 0,
          transition: widthTransition,
          "& .MuiDrawer-paper": {
            width,
            boxSizing: "border-box",
            overflowX: "hidden",
            transition: widthTransition,
          },
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
        }}
      >
        <Toolbar />
        {children}
      </Box>
    </Box>
  );
}

type NavItem = { to: string; label: string; icon: ReactNode };

/**
 * Highlight a nav entry when it owns the current path, including its
 * sub-routes (`/watch/3`).
 */
function isActive(pathname: string, to: string): boolean {
  return pathname === to || pathname.startsWith(`${to}/`);
}
