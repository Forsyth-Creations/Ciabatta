//! The deploy web server, backing two flags:
//!
//! * `--gui` — a live view of a running deploy: the step flowchart, per-step
//!   logs, and fix-it buttons when a recovery node is waiting.
//! * `--build` — a visual builder that emits copy-pasteable flowchart TOML.
//!
//! Like [`crate::analyze::server`], it's a tiny dependency-free HTTP server
//! (embedded, self-contained HTML). The `--gui` view additionally accepts a
//! `POST /choose` so the browser can answer a recovery prompt, forwarded to the
//! deploy engine over a broadcast channel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};

use crate::config::CiabattaConfig;
use crate::runner::{self, DeployCtl, ProgressUpdate, RunMode, StageKind, StepChoice};

use super::resolve_deploy;

/// The embedded live-view single-page app.
const GUI_HTML: &str = include_str!("index.html");
/// The embedded flowchart-builder single-page app.
const BUILDER_HTML: &str = include_str!("builder.html");

// ─── Serializable live state ────────────────────────────────────────────────

#[derive(Serialize, Clone, Default)]
struct GuiState {
    recipes: Vec<RecipeView>,
    done: bool,
    dry_run: bool,
}

#[derive(Serialize, Clone)]
struct RecipeView {
    name: String,
    status: String,
    error: Option<String>,
    /// The four deploy phases (login → pre → deploy → post) with their live
    /// status, so the GUI can show which phase is running and where it stopped.
    stages: Vec<StageView>,
    steps: Vec<StepView>,
    edges: Vec<EdgeView>,
    logs: Vec<String>,
    pending: Option<PendingChoice>,
}

#[derive(Serialize, Clone)]
struct StageView {
    name: String,
    /// pending · running · success · skipped · failed
    status: String,
}

#[derive(Serialize, Clone)]
struct StepView {
    name: String,
    status: String,
    recover: bool,
    action: Option<String>,
    needs: Vec<String>,
    on_error: Option<String>,
    logs: Vec<String>,
}

#[derive(Serialize, Clone)]
struct EdgeView {
    from: String,
    to: String,
    kind: String,
}

#[derive(Serialize, Clone)]
struct PendingChoice {
    step: String,
    message: String,
    options: Vec<String>,
}

impl RecipeView {
    fn step_mut(&mut self, name: &str) -> Option<&mut StepView> {
        self.steps.iter_mut().find(|s| s.name == name)
    }
}

impl GuiState {
    fn recipe_mut(&mut self, name: &str) -> Option<&mut RecipeView> {
        self.recipes.iter_mut().find(|r| r.name == name)
    }

    /// Fold one progress update into the live state.
    fn apply(&mut self, update: ProgressUpdate) {
        match update {
            ProgressUpdate::Started(name) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.status = "running".into();
                }
            }
            ProgressUpdate::Log(name, line) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.logs.push(line);
                }
            }
            ProgressUpdate::StepStarted { recipe, step } => {
                if let Some(r) = self.recipe_mut(&recipe) {
                    // Reaching a step clears any prior pending choice on the recipe.
                    r.pending = None;
                    if let Some(s) = r.step_mut(&step) {
                        s.status = "running".into();
                    }
                }
            }
            ProgressUpdate::StepFinished { recipe, step, ok } => {
                if let Some(r) = self.recipe_mut(&recipe)
                    && let Some(s) = r.step_mut(&step)
                {
                    s.status = if ok { "success".into() } else { "failed".into() };
                }
            }
            ProgressUpdate::StepLog { recipe, step, line } => {
                if let Some(r) = self.recipe_mut(&recipe) {
                    if let Some(s) = r.step_mut(&step) {
                        s.logs.push(line.clone());
                    }
                    r.logs.push(format!("[{step}] {line}"));
                }
            }
            ProgressUpdate::StepNeedsChoice {
                recipe,
                step,
                message,
                options,
            } => {
                if let Some(r) = self.recipe_mut(&recipe) {
                    r.pending = Some(PendingChoice {
                        step,
                        message,
                        options,
                    });
                }
            }
            ProgressUpdate::Completed(name) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.status = "success".into();
                    r.pending = None;
                }
            }
            ProgressUpdate::Failed(name, err) => {
                if let Some(r) = self.recipe_mut(&name) {
                    r.status = "failed".into();
                    r.error = Some(err.clone());
                    r.pending = None;
                    r.logs.push(format!("✗ {err}"));
                    // Pin the blame on whichever stage was mid-flight, and mark
                    // any later stages as not reached.
                    let mut hit = false;
                    for st in &mut r.stages {
                        if st.status == "running" {
                            st.status = "failed".into();
                            hit = true;
                        } else if hit && st.status == "pending" {
                            st.status = "skipped".into();
                        }
                    }
                }
            }
            ProgressUpdate::StageStarted { recipe, stage } => {
                let label = stage.label(RunMode::Deploy);
                if let Some(r) = self.recipe_mut(&recipe)
                    && let Some(s) = r.stages.iter_mut().find(|s| s.name == label)
                {
                    s.status = "running".into();
                }
            }
            ProgressUpdate::StageFinished { recipe, stage, ran } => {
                let label = stage.label(RunMode::Deploy);
                if let Some(r) = self.recipe_mut(&recipe)
                    && let Some(s) = r.stages.iter_mut().find(|s| s.name == label)
                    // A stage that already failed stays failed.
                    && s.status != "failed"
                {
                    s.status = if ran { "success".into() } else { "skipped".into() };
                }
            }
            // Deploys don't emit stage-file-transfer progress.
            ProgressUpdate::TransferProgress { .. } => {}
        }
        self.done = self
            .recipes
            .iter()
            .all(|r| r.status == "success" || r.status == "failed");
    }
}

/// Build the initial live state (all steps pending) from the resolved deploys.
fn initial_state(
    config: &CiabattaConfig,
    root: &std::path::Path,
    names: &[String],
    dry_run: bool,
) -> Result<GuiState> {
    let mut recipes = Vec::new();
    for name in names {
        let entry = config
            .recipes
            .get(name)
            .with_context(|| format!("Recipe '{name}' not found"))?;
        let deploy = entry
            .deploy_recipe()
            .with_context(|| format!("Recipe '{name}' has no [deploy] definition"))?;
        let resolved = resolve_deploy(deploy, name, root)?;

        let mut steps = Vec::new();
        let mut edges = Vec::new();
        for step in &resolved.steps {
            for dep in &step.needs {
                edges.push(EdgeView {
                    from: dep.clone(),
                    to: step.name.clone(),
                    kind: "needs".into(),
                });
            }
            if let Some(t) = step.on_error.as_deref() {
                edges.push(EdgeView {
                    from: step.name.clone(),
                    to: t.to_string(),
                    kind: "error".into(),
                });
            }
            if let Some(t) = step.retry.as_deref() {
                edges.push(EdgeView {
                    from: step.name.clone(),
                    to: t.to_string(),
                    kind: "retry".into(),
                });
            }
            steps.push(StepView {
                name: step.name.clone(),
                status: "pending".into(),
                recover: step.recover,
                action: step.script.clone().or_else(|| step.run.clone()),
                needs: step.needs.clone(),
                on_error: step.on_error.clone(),
                logs: Vec::new(),
            });
        }

        let stages = StageKind::ALL
            .iter()
            .map(|s| StageView {
                name: s.label(RunMode::Deploy).to_string(),
                status: "pending".into(),
            })
            .collect();

        recipes.push(RecipeView {
            name: name.clone(),
            status: "pending".into(),
            error: None,
            stages,
            steps,
            edges,
            logs: Vec::new(),
            pending: None,
        });
    }
    Ok(GuiState {
        recipes,
        done: false,
        dry_run,
    })
}

// ─── `--gui`: live deploy view ──────────────────────────────────────────────

/// Run a deploy while serving its live view at `http://127.0.0.1:port`.
pub async fn serve_gui(
    config: CiabattaConfig,
    root: PathBuf,
    names: Vec<String>,
    env_vars: HashMap<String, String>,
    dry_run: bool,
    port: u16,
) -> Result<()> {
    // Fail fast on a bad flowchart before we bind a port or open a browser.
    let state = Arc::new(Mutex::new(initial_state(&config, &root, &names, dry_run)?));

    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind 127.0.0.1:{port} ({e}). Try a different --port.")
    })?;

    // Broadcast bus carrying recovery choices from the browser to the engine.
    let (choice_tx, _) = broadcast::channel::<StepChoice>(64);
    let (prog_tx, mut prog_rx) = mpsc::channel::<ProgressUpdate>(256);

    // Drive the deploy in the background, interactive so recovery nodes wait for
    // a browser choice.
    let ctl = DeployCtl {
        interactive: true,
        choices: Some(choice_tx.clone()),
    };
    {
        let config = config.clone();
        let root = root.clone();
        let names = names.clone();
        let env_vars = env_vars.clone();
        tokio::spawn(async move {
            let _ = runner::run_all_ctl(
                &config,
                &root,
                &names,
                &env_vars,
                dry_run,
                RunMode::Deploy,
                ctl,
                prog_tx,
            )
            .await;
        });
    }

    // Fold progress updates into the shared state.
    {
        let state = state.clone();
        tokio::spawn(async move {
            while let Some(update) = prog_rx.recv().await {
                state.lock().unwrap().apply(update);
            }
        });
    }

    let url = format!("http://127.0.0.1:{port}");
    println!("\nDeploy view ready at {url}");
    println!("Press Ctrl-C to stop.");
    open_browser(&url);

    // Serve until interrupted (state keeps updating; logs remain readable after
    // the deploy finishes).
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping deploy view.");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (socket, _) = accepted?;
                let state = state.clone();
                let choice_tx = choice_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_gui(socket, &state, &choice_tx).await {
                        eprintln!("deploy view: connection error: {e}");
                    }
                });
            }
        }
    }
}

async fn handle_gui(
    mut socket: TcpStream,
    state: &Arc<Mutex<GuiState>>,
    choice_tx: &broadcast::Sender<StepChoice>,
) -> Result<()> {
    let req = read_request(&mut socket).await?;
    let Some(req) = req else { return Ok(()) };

    if req.method == "POST" && req.path.starts_with("/choose") {
        // Body: {"recipe":"..","step":"..","option":N}
        if let Ok(choice) = serde_json::from_str::<ChoiceBody>(&req.body) {
            let _ = choice_tx.send(StepChoice {
                recipe: choice.recipe,
                step: choice.step,
                option: choice.option,
            });
        }
        return respond(&mut socket, "200 OK", "application/json", b"{\"ok\":true}").await;
    }

    if req.path.starts_with("/state.json") {
        let json = { serde_json::to_vec(&*state.lock().unwrap())? };
        return respond(&mut socket, "200 OK", "application/json; charset=utf-8", &json).await;
    }

    if req.path == "/" || req.path.starts_with("/index") {
        return respond(
            &mut socket,
            "200 OK",
            "text/html; charset=utf-8",
            GUI_HTML.as_bytes(),
        )
        .await;
    }

    respond(&mut socket, "404 Not Found", "text/plain", b"not found").await
}

#[derive(serde::Deserialize)]
struct ChoiceBody {
    recipe: String,
    step: String,
    option: usize,
}

// ─── `--build`: flowchart builder ───────────────────────────────────────────

/// Serve the visual flowchart builder at `http://127.0.0.1:port`. Authoring only
/// — it needs no project and runs nothing.
pub async fn serve_builder(port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!("Failed to bind 127.0.0.1:{port} ({e}). Try a different --port.")
    })?;

    let url = format!("http://127.0.0.1:{port}");
    println!("\nFlowchart builder ready at {url}");
    println!("Design your pipeline, then copy the TOML into a flowchart file.");
    println!("Press Ctrl-C to stop.");
    open_browser(&url);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping builder.");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (mut socket, _) = accepted?;
                tokio::spawn(async move {
                    let _ = respond(
                        &mut socket,
                        "200 OK",
                        "text/html; charset=utf-8",
                        BUILDER_HTML.as_bytes(),
                    )
                    .await;
                });
            }
        }
    }
}

// ─── HTTP helpers ───────────────────────────────────────────────────────────

struct Request {
    method: String,
    path: String,
    body: String,
}

/// Read an HTTP request, including a `Content-Length` body if present. Returns
/// `None` on an empty/closed connection.
async fn read_request(socket: &mut TcpStream) -> Result<Option<Request>> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    // Read until we have the full header block.
    let header_end = loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(None);
            }
            break buf.len();
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 1 << 20 {
            break buf.len(); // guard against unbounded headers
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end.min(buf.len())]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let content_length = head
        .lines()
        .find_map(|l| {
            let l = l.trim();
            l.strip_prefix("Content-Length:")
                .or_else(|| l.strip_prefix("content-length:"))
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buf[header_end.min(buf.len())..].to_vec();
    while body.len() < content_length {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(Some(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
    }))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

async fn respond(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body).await?;
    socket.flush().await?;
    Ok(())
}

/// Best-effort: open `url` in the platform browser. Never fails the command.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let candidates: [(&str, &[&str]); 1] = [("open", &[])];
    #[cfg(target_os = "windows")]
    let candidates: [(&str, &[&str]); 1] = [("cmd", &["/C", "start", ""])];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: [(&str, &[&str]); 2] = [("xdg-open", &[]), ("gio", &["open"])];

    for (cmd, args) in candidates {
        let mut command = std::process::Command::new(cmd);
        command.args(args).arg(url);
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        if command.spawn().is_ok() {
            return;
        }
    }
}
