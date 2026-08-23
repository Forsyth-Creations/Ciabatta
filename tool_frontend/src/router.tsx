/**
 * The route tree.
 *
 * Code-based rather than file-based routing on purpose: file-based needs a
 * codegen step, and this bundle is compiled into a Rust binary by CI, so the
 * fewer generated-file-vs-source drift opportunities the better.
 *
 * Each of the six tools ciabatta used to serve on its own port is a route here.
 */

import { createRootRoute, createRoute, createRouter, Outlet } from "@tanstack/react-router";

import { AppShell } from "./components/AppShell";
import { AnalyzePage } from "./pages/AnalyzePage";
import { CachePage } from "./pages/CachePage";
import { AiPage } from "./pages/AiPage";
import { DashboardPage } from "./pages/DashboardPage";
import { DocsPage } from "./pages/DocsPage";
import { RunBuilderPage } from "./pages/RunBuilderPage";
import { RunPage } from "./pages/RunPage";
import { RunDetailPage } from "./pages/RunDetailPage";
import { TodoPage } from "./pages/TodoPage";
import { WatchPage } from "./pages/WatchPage";
import { WatchSessionPage } from "./pages/WatchSessionPage";
import { WorkspacePage } from "./pages/WorkspacePage";

/**
 * The shell is the root route's component, not a wrapper around
 * `RouterProvider` — it renders `Link`s and reads the current path, both of
 * which need the router context that only exists below the provider.
 */
const rootRoute = createRootRoute({
  component: () => (
    <AppShell>
      <Outlet />
    </AppShell>
  ),
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: DashboardPage,
});

const todoRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/todo",
  component: TodoPage,
});

const watchRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/watch",
  component: WatchPage,
});

const watchSessionRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/watch/$sessionId",
  component: WatchSessionPage,
});

const workspaceRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/workspace",
  component: WorkspacePage,
});

const runRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/run",
  component: RunPage,
});

// Registered before the $runId route so "builder" isn't captured as a run id.
const runBuilderRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/run/builder",
  component: RunBuilderPage,
});

const runDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/run/$runId",
  component: RunDetailPage,
});

const cacheRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/cache",
  component: CachePage,
});

const analyzeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/analyze",
  component: AnalyzePage,
});

const aiRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/ai",
  component: AiPage,
});

// The manual, served from the same bundle so it matches the running binary.
const docsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/docs",
  component: DocsPage,
});

const routeTree = rootRoute.addChildren([
  indexRoute,
  todoRoute,
  watchRoute,
  watchSessionRoute,
  workspaceRoute,
  runRoute,
  runBuilderRoute,
  runDetailRoute,
  cacheRoute,
  analyzeRoute,
  aiRoute,
  docsRoute,
]);

export const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
