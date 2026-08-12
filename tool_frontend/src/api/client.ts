/**
 * The single place this app talks to the daemon.
 *
 * Auth: the daemon injects its API token into the served `index.html` as
 * `<meta name="ciabatta-token">` (see `src/daemon/assets.rs`). Reading the page
 * already implies being able to read `~/.ciabatta/daemon.json`, so handing the
 * token to the page costs a local user nothing — and it means mutating routes
 * stay closed when the daemon is bound to something other than loopback.
 *
 * In `yarn dev` there's no injected tag, because Vite serves its own
 * index.html. The daemon accepts the token as a query parameter too, so dev
 * mode reads it from `?token=` or localStorage instead.
 */

const TOKEN_STORAGE_KEY = "ciabatta-token";

/** Resolve the API token once, at module load. */
function resolveToken(): string {
  const meta = document.querySelector<HTMLMetaElement>('meta[name="ciabatta-token"]');
  if (meta?.content) return meta.content;

  // Dev-server fallback: ?token=... wins and is remembered for later reloads.
  const fromQuery = new URLSearchParams(window.location.search).get("token");
  if (fromQuery) {
    localStorage.setItem(TOKEN_STORAGE_KEY, fromQuery);
    return fromQuery;
  }

  return localStorage.getItem(TOKEN_STORAGE_KEY) ?? "";
}

export const token = resolveToken();

/**
 * Thrown for any non-2xx response, carrying the daemon's error message.
 *
 * `body` is the parsed JSON when there was any, for the few failures the app
 * acts on rather than just displays — a run rejected for missing environment
 * variables names them in `missing_env` so the launcher can prompt.
 */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly body: Record<string, unknown> | null = null,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** Whether the app has a token at all — drives the "no token" banner in dev. */
export const hasToken = token.length > 0;

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: {
      ...(init?.body ? { "Content-Type": "application/json" } : {}),
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...init?.headers,
    },
  });

  if (!response.ok) {
    const { message, body } = await errorPayload(response);
    throw new ApiError(response.status, message, body);
  }

  // 204 and empty bodies are legitimate for deletes.
  const text = await response.text();
  return (text ? JSON.parse(text) : null) as T;
}

/** Pull the message — and any structured fields — out of an error response. */
async function errorPayload(
  response: Response,
): Promise<{ message: string; body: Record<string, unknown> | null }> {
  const text = await response.text().catch(() => "");
  if (!text) return { message: `${response.status} ${response.statusText}`, body: null };
  try {
    const parsed = JSON.parse(text) as Record<string, unknown>;
    return { message: (parsed.error as string) ?? text, body: parsed };
  } catch {
    return { message: text, body: null };
  }
}

/**
 * Fetch a route that returns a file, as text plus the filename the daemon
 * chose in its Content-Disposition header.
 *
 * Separate from `request` because that one parses JSON; these routes serve a
 * transcript that must arrive verbatim.
 */
async function requestText(path: string): Promise<{ text: string; filename: string }> {
  const response = await fetch(path, {
    headers: token ? { Authorization: `Bearer ${token}` } : {},
  });
  if (!response.ok) {
    const { message, body } = await errorPayload(response);
    throw new ApiError(response.status, message, body);
  }
  const disposition = response.headers.get("Content-Disposition") ?? "";
  const filename = /filename="([^"]+)"/.exec(disposition)?.[1] ?? "download.txt";
  return { text: await response.text(), filename };
}

export const api = {
  get: <T>(path: string) => request<T>(path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, {
      method: "POST",
      body: body === undefined ? undefined : JSON.stringify(body),
    }),
  delete: <T>(path: string) => request<T>(path, { method: "DELETE" }),
  text: requestText,
};

/** Hand the browser a string as a downloaded file. */
export function downloadText(filename: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "text/plain;charset=utf-8" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  // Revoking immediately can race the download in some browsers; a tick is
  // enough for the click to have been handled.
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/**
 * Build an SSE URL with the token attached.
 *
 * `EventSource` can't set request headers, which is why the daemon's auth
 * middleware also accepts `?token=`. It only ever travels over loopback.
 */
export function streamUrl(path: string, params: Record<string, string> = {}): string {
  const url = new URL(path, window.location.origin);
  for (const [key, value] of Object.entries(params)) {
    url.searchParams.set(key, value);
  }
  if (token) url.searchParams.set("token", token);
  return url.toString();
}
