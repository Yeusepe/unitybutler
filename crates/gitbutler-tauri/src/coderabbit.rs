use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, bail};
use but_api::json::Error;
use but_ctx::{Context, ProjectHandleOrLegacyProjectId};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use tracing::instrument;

const CODERABBIT_REVIEW_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CODERABBIT_QUIET_UPDATE_AFTER: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct CodeRabbit {
    inner: Mutex<CodeRabbitState>,
}

#[derive(Default)]
struct CodeRabbitState {
    active: HashMap<String, ActiveReview>,
    findings: HashMap<String, Vec<CodeRabbitFinding>>,
    last_review: HashMap<String, CodeRabbitReviewSummary>,
}

struct ActiveReview {
    review_id: String,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<CodeRabbitReviewProgress>>,
    findings: Arc<Mutex<Vec<CodeRabbitFinding>>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitStatus {
    cli_available: bool,
    version: Option<String>,
    authenticated: bool,
    username: Option<String>,
    current_org: Option<String>,
    config_exists: bool,
    active_review_id: Option<String>,
    active_progress: Option<CodeRabbitReviewProgress>,
    last_review: Option<CodeRabbitReviewSummary>,
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitReviewProgress {
    phase: String,
    detail: String,
    steps: Vec<CodeRabbitReviewStep>,
    started_at_ms: u128,
    updated_at_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitReviewStep {
    label: String,
    status: CodeRabbitReviewStepStatus,
    detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodeRabbitReviewStepStatus {
    Pending,
    Running,
    Complete,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitReviewSummary {
    review_id: String,
    status: CodeRabbitReviewSummaryStatus,
    message: String,
    findings_count: usize,
    completed_at_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodeRabbitReviewSummaryStatus {
    Complete,
    NoFindings,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitReviewRequest {
    review_id: Option<String>,
    #[serde(default = "default_review_type")]
    review_type: String,
    base: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    workflows: Vec<CodeRabbitWorkflowId>,
}

fn default_review_type() -> String {
    "uncommitted".to_string()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CodeRabbitWorkflowId {
    Default,
    Performance,
    Security,
    Correctness,
}

impl Default for CodeRabbitWorkflowId {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitReviewResult {
    review_id: String,
    findings: Vec<CodeRabbitFinding>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitFindingUpdate {
    finding_id: String,
    status: CodeRabbitFindingStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CodeRabbitFindingStatus {
    Open,
    Dismissed,
    Applied,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRabbitFinding {
    id: String,
    review_id: String,
    project_id: String,
    path: String,
    old_line: Option<u32>,
    new_line: Option<u32>,
    severity: CodeRabbitSeverity,
    category: Option<String>,
    title: String,
    body: String,
    suggested_patch: Option<String>,
    workflow_id: Option<String>,
    status: CodeRabbitFindingStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CodeRabbitSeverity {
    Critical,
    Major,
    Minor,
    Info,
}

#[tauri::command(async)]
#[instrument(skip(coderabbit), err(Debug))]
pub async fn coderabbit_status(
    coderabbit: State<'_, CodeRabbit>,
    project_id: ProjectHandleOrLegacyProjectId,
) -> Result<CodeRabbitStatus, Error> {
    let workdir = project_workdir(project_id.clone())?;
    let project_key = project_id.to_string();
    let (active_review_id, active_progress, last_review) = {
        let inner = coderabbit.inner.lock();
        (
            inner
                .active
                .get(&project_key)
                .map(|active| active.review_id.clone()),
            inner
                .active
                .get(&project_key)
                .map(|active| active.progress.lock().clone()),
            inner.last_review.get(&project_key).cloned(),
        )
    };

    tokio::task::spawn_blocking(move || {
        status_for_workdir(&workdir, active_review_id, active_progress, last_review)
    })
    .await
    .context("failed to join CodeRabbit status task")?
    .map_err(Into::into)
}

#[tauri::command(async)]
#[instrument(skip(coderabbit), err(Debug))]
pub async fn coderabbit_login(
    coderabbit: State<'_, CodeRabbit>,
    project_id: ProjectHandleOrLegacyProjectId,
) -> Result<CodeRabbitStatus, Error> {
    let workdir = project_workdir(project_id.clone())?;
    let project_key = project_id.to_string();
    let (active_review_id, active_progress, last_review) = {
        let inner = coderabbit.inner.lock();
        (
            inner
                .active
                .get(&project_key)
                .map(|active| active.review_id.clone()),
            inner
                .active
                .get(&project_key)
                .map(|active| active.progress.lock().clone()),
            inner.last_review.get(&project_key).cloned(),
        )
    };
    tokio::task::spawn_blocking(move || {
        let _ = Command::new("coderabbit")
            .args(["auth", "login", "--agent"])
            .current_dir(&workdir)
            .status();
        status_for_workdir(&workdir, active_review_id, active_progress, last_review)
    })
    .await
    .context("failed to join CodeRabbit login task")?
    .map_err(Into::into)
}

#[tauri::command(async)]
#[instrument(skip(coderabbit), err(Debug))]
pub async fn coderabbit_review(
    coderabbit: State<'_, CodeRabbit>,
    project_id: ProjectHandleOrLegacyProjectId,
    request: CodeRabbitReviewRequest,
) -> Result<CodeRabbitReviewResult, Error> {
    let workdir = project_workdir(project_id.clone())?;
    let project_key = project_id.to_string();
    let review_id = request.review_id.clone().unwrap_or_else(new_review_id);
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(CodeRabbitReviewProgress::new()));
    let live_findings = Arc::new(Mutex::new(Vec::new()));

    {
        let mut inner = coderabbit.inner.lock();
        if let Some(active) = inner.active.get(&project_key) {
            return Err(
                anyhow::anyhow!("CodeRabbit review already running: {}", active.review_id).into(),
            );
        }
        inner.active.insert(
            project_key.clone(),
            ActiveReview {
                review_id: review_id.clone(),
                cancel: cancel.clone(),
                progress: progress.clone(),
                findings: live_findings.clone(),
            },
        );
    }

    let project_id_for_findings = project_key.clone();
    let project_id_for_summary = project_key.clone();
    let review_id_for_findings = review_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        run_review(
            &workdir,
            &project_id_for_findings,
            &review_id_for_findings,
            request,
            cancel,
            progress,
            live_findings,
        )
    })
    .await
    .context("failed to join CodeRabbit review task")?;

    let mut inner = coderabbit.inner.lock();
    inner.active.remove(&project_key);
    match result {
        Ok(findings) => {
            inner.findings.insert(project_key, findings.clone());
            inner.last_review.insert(
                project_id_for_summary.clone(),
                CodeRabbitReviewSummary::completed(&review_id, findings.len()),
            );
            Ok(CodeRabbitReviewResult {
                review_id,
                findings,
            })
        }
        Err(err) => {
            inner.last_review.insert(
                project_id_for_summary,
                CodeRabbitReviewSummary::failed(&review_id, err.to_string()),
            );
            Err(err.into())
        }
    }
}

#[tauri::command(async)]
#[instrument(skip(coderabbit), err(Debug))]
pub fn coderabbit_cancel(
    coderabbit: State<'_, CodeRabbit>,
    project_id: ProjectHandleOrLegacyProjectId,
    review_id: String,
) -> Result<bool, Error> {
    let project_key = project_id.to_string();
    let inner = coderabbit.inner.lock();
    let Some(active) = inner.active.get(&project_key) else {
        return Ok(false);
    };
    if active.review_id != review_id {
        return Ok(false);
    }
    active.cancel.store(true, Ordering::SeqCst);
    Ok(true)
}

#[tauri::command(async)]
#[instrument(skip(coderabbit), err(Debug))]
pub fn coderabbit_findings(
    coderabbit: State<'_, CodeRabbit>,
    project_id: ProjectHandleOrLegacyProjectId,
    review_id: Option<String>,
) -> Result<Vec<CodeRabbitFinding>, Error> {
    let project_key = project_id.to_string();
    let inner = coderabbit.inner.lock();
    let mut findings = if let Some(active) = inner.active.get(&project_key)
        && review_id
            .as_ref()
            .map(|review_id| review_id == &active.review_id)
            .unwrap_or(true)
    {
        active.findings.lock().clone()
    } else {
        inner
            .findings
            .get(&project_key)
            .cloned()
            .unwrap_or_default()
    };
    if let Some(review_id) = review_id {
        findings.retain(|finding| finding.review_id == review_id);
    }
    Ok(findings)
}

#[tauri::command(async)]
#[instrument(skip(coderabbit), err(Debug))]
pub fn coderabbit_update_finding(
    coderabbit: State<'_, CodeRabbit>,
    project_id: ProjectHandleOrLegacyProjectId,
    update: CodeRabbitFindingUpdate,
) -> Result<Option<CodeRabbitFinding>, Error> {
    let project_key = project_id.to_string();
    let mut inner = coderabbit.inner.lock();
    if let Some(active) = inner.active.get(&project_key) {
        if let Some(finding) = active
            .findings
            .lock()
            .iter_mut()
            .find(|finding| finding.id == update.finding_id)
        {
            finding.status = update.status;
            return Ok(Some(finding.clone()));
        }
    }
    let Some(findings) = inner.findings.get_mut(&project_key) else {
        return Ok(None);
    };
    let Some(finding) = findings
        .iter_mut()
        .find(|finding| finding.id == update.finding_id)
    else {
        return Ok(None);
    };
    finding.status = update.status;
    Ok(Some(finding.clone()))
}

#[tauri::command(async)]
#[instrument(err(Debug))]
pub fn coderabbit_write_default_config(
    project_id: ProjectHandleOrLegacyProjectId,
) -> Result<bool, Error> {
    let workdir = project_workdir(project_id)?;
    let path = workdir.join(".coderabbit.yaml");
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(path, DEFAULT_CODERABBIT_CONFIG).map_err(anyhow::Error::from)?;
    Ok(true)
}

const DEFAULT_CODERABBIT_CONFIG: &str = r#"# yaml-language-server: $schema=https://coderabbit.ai/integrations/schema.v2.json
reviews:
  path_filters:
    - "!**/node_modules/**"
    - "!**/target/**"
    - "!**/dist/**"
    - "!**/build/**"
    - "!**/*.unity"
    - "!**/*.prefab"
    - "!**/*.asset"
    - "!**/*.meta"
    - "!**/pnpm-lock.yaml"
    - "!**/package-lock.json"
  path_instructions:
    - path: "crates/**"
      instructions: |
        Focus on Rust correctness, locking, error handling, performance, and Git repository semantics.
    - path: "apps/desktop/**"
      instructions: |
        Focus on Svelte state, async UI behavior, Tauri command use, and user-facing regressions.
"#;

fn project_workdir(project_id: ProjectHandleOrLegacyProjectId) -> anyhow::Result<PathBuf> {
    let ctx: Context = project_id.try_into()?;
    ctx.workdir_or_fail()
}

fn status_for_workdir(
    workdir: &Path,
    active_review_id: Option<String>,
    active_progress: Option<CodeRabbitReviewProgress>,
    last_review: Option<CodeRabbitReviewSummary>,
) -> anyhow::Result<CodeRabbitStatus> {
    let version = Command::new("coderabbit")
        .arg("--version")
        .current_dir(workdir)
        .output();

    let Ok(version) = version else {
        return Ok(CodeRabbitStatus {
            cli_available: false,
            version: None,
            authenticated: false,
            username: None,
            current_org: None,
            config_exists: workdir.join(".coderabbit.yaml").exists(),
            active_review_id,
            active_progress,
            last_review,
            error: Some("CodeRabbit CLI was not found on PATH".to_string()),
        });
    };

    let version_text = String::from_utf8_lossy(&version.stdout).trim().to_string();
    let auth = Command::new("coderabbit")
        .args(["auth", "status", "--agent"])
        .current_dir(workdir)
        .output();

    let (authenticated, username, current_org, error) = match auth {
        Ok(auth) if auth.status.success() => {
            let value: Value = serde_json::from_slice(&auth.stdout).unwrap_or(Value::Null);
            (
                value
                    .get("authenticated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                value
                    .pointer("/user/username")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                value
                    .pointer("/currentOrg/name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                None,
            )
        }
        Ok(auth) => (
            false,
            None,
            None,
            Some(String::from_utf8_lossy(&auth.stderr).trim().to_string()),
        ),
        Err(err) => (false, None, None, Some(err.to_string())),
    };

    Ok(CodeRabbitStatus {
        cli_available: true,
        version: (!version_text.is_empty()).then_some(version_text),
        authenticated,
        username,
        current_org,
        config_exists: workdir.join(".coderabbit.yaml").exists(),
        active_review_id,
        active_progress,
        last_review,
        error,
    })
}

impl CodeRabbitReviewProgress {
    fn new() -> Self {
        let now = now_ms();
        Self {
            phase: "Queued".to_string(),
            detail: "Waiting to start CodeRabbit review.".to_string(),
            steps: vec![
                CodeRabbitReviewStep::pending("Prepare review scope"),
                CodeRabbitReviewStep::pending("Prepare workflow instructions"),
                CodeRabbitReviewStep::pending("Run CodeRabbit CLI"),
                CodeRabbitReviewStep::pending("Parse inline recommendations"),
            ],
            started_at_ms: now,
            updated_at_ms: now,
        }
    }
}

impl CodeRabbitReviewStep {
    fn pending(label: &str) -> Self {
        Self {
            label: label.to_string(),
            status: CodeRabbitReviewStepStatus::Pending,
            detail: None,
        }
    }
}

impl CodeRabbitReviewSummary {
    fn completed(review_id: &str, findings_count: usize) -> Self {
        let (status, message) = if findings_count == 0 {
            (
                CodeRabbitReviewSummaryStatus::NoFindings,
                "CodeRabbit completed with no recommendations.".to_string(),
            )
        } else {
            (
                CodeRabbitReviewSummaryStatus::Complete,
                format!("CodeRabbit completed with {findings_count} recommendations."),
            )
        };
        Self {
            review_id: review_id.to_string(),
            status,
            message,
            findings_count,
            completed_at_ms: now_ms(),
        }
    }

    fn failed(review_id: &str, message: String) -> Self {
        let status = if message.contains("cancelled") {
            CodeRabbitReviewSummaryStatus::Cancelled
        } else {
            CodeRabbitReviewSummaryStatus::Failed
        };
        Self {
            review_id: review_id.to_string(),
            status,
            message,
            findings_count: 0,
            completed_at_ms: now_ms(),
        }
    }
}

fn set_progress(
    progress: &Arc<Mutex<CodeRabbitReviewProgress>>,
    phase: &str,
    detail: &str,
    step_index: usize,
    step_status: CodeRabbitReviewStepStatus,
    step_detail: Option<String>,
) {
    let mut progress = progress.lock();
    progress.phase = phase.to_string();
    progress.detail = detail.to_string();
    progress.updated_at_ms = now_ms();
    if let Some(step) = progress.steps.get_mut(step_index) {
        step.status = step_status;
        step.detail = step_detail.or_else(|| Some(detail.to_string()));
    }
}

fn run_review(
    workdir: &Path,
    project_id: &str,
    review_id: &str,
    request: CodeRabbitReviewRequest,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<CodeRabbitReviewProgress>>,
    live_findings: Arc<Mutex<Vec<CodeRabbitFinding>>>,
) -> anyhow::Result<Vec<CodeRabbitFinding>> {
    set_progress(
        &progress,
        "Preparing review",
        "Resolving review scope and CodeRabbit CLI arguments.",
        0,
        CodeRabbitReviewStepStatus::Running,
        None,
    );
    let mut args = vec!["review".to_string(), "--agent".to_string()];
    args.push("--type".to_string());
    args.push(request.review_type);
    if let Some(base) = request.base {
        args.push("--base".to_string());
        args.push(base);
    }
    let files = request
        .files
        .into_iter()
        .filter(|path| !should_skip_path(path))
        .collect::<Vec<_>>();
    if !files.is_empty() {
        args.push("--files".to_string());
        args.extend(files);
    }
    set_progress(
        &progress,
        "Scope ready",
        "Filtered skipped files and prepared the review request.",
        0,
        CodeRabbitReviewStepStatus::Complete,
        None,
    );

    set_progress(
        &progress,
        "Preparing workflows",
        "Writing temporary CodeRabbit instruction files for selected workflows.",
        1,
        CodeRabbitReviewStepStatus::Running,
        None,
    );
    let instruction_files = write_workflow_instruction_files(workdir, &request.workflows)?;
    for path in &instruction_files {
        args.push("-c".to_string());
        args.push(path.to_string_lossy().to_string());
    }
    set_progress(
        &progress,
        "Workflows ready",
        "Workflow instructions are ready.",
        1,
        CodeRabbitReviewStepStatus::Complete,
        None,
    );

    set_progress(
        &progress,
        "Starting CodeRabbit",
        "Launching `coderabbit review --agent`.",
        2,
        CodeRabbitReviewStepStatus::Running,
        Some(format!("Command: coderabbit {}", args.join(" "))),
    );
    let mut child = Command::new("coderabbit")
        .args(&args)
        .current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start CodeRabbit review")?;
    set_progress(
        &progress,
        "CodeRabbit running",
        "Waiting for CodeRabbit CLI to finish reviewing the selected scope.",
        2,
        CodeRabbitReviewStepStatus::Running,
        None,
    );

    let stdout = child.stdout.take().context("missing CodeRabbit stdout")?;
    let stderr = child.stderr.take().context("missing CodeRabbit stderr")?;
    let (line_tx, line_rx) = mpsc::channel();
    let stdout_thread = spawn_line_reader(stdout, false, line_tx.clone());
    let stderr_thread = spawn_line_reader(stderr, true, line_tx);
    let started_at = Instant::now();
    let mut last_output_at = Instant::now();
    let mut last_quiet_update_at = Instant::now();
    let mut stdout = String::new();
    let mut stderr = String::new();

    let status = loop {
        while let Ok(line) = line_rx.try_recv() {
            last_output_at = Instant::now();
            if line.stderr {
                stderr.push_str(&line.line);
                stderr.push('\n');
            } else {
                handle_agent_stdout_line(
                    &progress,
                    &live_findings,
                    project_id,
                    review_id,
                    &line.line,
                );
                stdout.push_str(&line.line);
                stdout.push('\n');
            }
        }

        if cancel.load(Ordering::SeqCst) {
            set_progress(
                &progress,
                "Cancelling review",
                "Stopping the CodeRabbit CLI process.",
                3,
                CodeRabbitReviewStepStatus::Failed,
                None,
            );
            let _ = child.kill();
            bail!("CodeRabbit review was cancelled");
        }
        if started_at.elapsed() > CODERABBIT_REVIEW_TIMEOUT {
            set_progress(
                &progress,
                "CodeRabbit timed out",
                "CodeRabbit did not finish within the GitButler review timeout.",
                2,
                CodeRabbitReviewStepStatus::Failed,
                Some(format!(
                    "No completed result after {}.",
                    format_duration(CODERABBIT_REVIEW_TIMEOUT)
                )),
            );
            let _ = child.kill();
            bail!(
                "CodeRabbit review timed out after {}",
                format_duration(CODERABBIT_REVIEW_TIMEOUT)
            );
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if last_output_at.elapsed() > CODERABBIT_QUIET_UPDATE_AFTER
            && last_quiet_update_at.elapsed() > CODERABBIT_QUIET_UPDATE_AFTER
        {
            last_quiet_update_at = Instant::now();
            set_progress(
                &progress,
                "CodeRabbit still running",
                &format!(
                    "Waiting for CodeRabbit CLI output. Last output was {} ago; total elapsed {}.",
                    format_duration(last_output_at.elapsed()),
                    format_duration(started_at.elapsed())
                ),
                2,
                CodeRabbitReviewStepStatus::Running,
                None,
            );
        }
        thread::sleep(Duration::from_millis(250));
    };
    while let Ok(line) = line_rx.try_recv() {
        if line.stderr {
            stderr.push_str(&line.line);
            stderr.push('\n');
        } else {
            handle_agent_stdout_line(&progress, &live_findings, project_id, review_id, &line.line);
            stdout.push_str(&line.line);
            stdout.push('\n');
        }
    }
    set_progress(
        &progress,
        "Parsing results",
        "CodeRabbit finished; parsing agent output into inline findings.",
        2,
        CodeRabbitReviewStepStatus::Complete,
        None,
    );
    set_progress(
        &progress,
        "Parsing results",
        "CodeRabbit finished; parsing agent output into inline findings.",
        3,
        CodeRabbitReviewStepStatus::Running,
        None,
    );

    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    while let Ok(line) = line_rx.try_recv() {
        if line.stderr {
            stderr.push_str(&line.line);
            stderr.push('\n');
        } else {
            handle_agent_stdout_line(&progress, &live_findings, project_id, review_id, &line.line);
            stdout.push_str(&line.line);
            stdout.push('\n');
        }
    }

    for file in instruction_files {
        let _ = std::fs::remove_file(file);
    }

    if !status.success() {
        let message = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("CodeRabbit review failed: {message}");
    }

    let findings = live_findings.lock().clone();
    let detail = if findings.is_empty() {
        "CodeRabbit completed and returned no findings.".to_string()
    } else {
        format!("CodeRabbit returned {} findings.", findings.len())
    };
    set_progress(
        &progress,
        "Review complete",
        &detail,
        3,
        CodeRabbitReviewStepStatus::Complete,
        None,
    );
    Ok(findings)
}

struct CodeRabbitOutputLine {
    stderr: bool,
    line: String,
}

fn spawn_line_reader(
    reader: impl Read + Send + 'static,
    stderr: bool,
    tx: mpsc::Sender<CodeRabbitOutputLine>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if tx.send(CodeRabbitOutputLine { stderr, line }).is_err() {
                break;
            }
        }
    })
}

fn handle_agent_stdout_line(
    progress: &Arc<Mutex<CodeRabbitReviewProgress>>,
    live_findings: &Arc<Mutex<Vec<CodeRabbitFinding>>>,
    project_id: &str,
    review_id: &str,
    line: &str,
) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        if !line.trim().is_empty() {
            set_progress(
                progress,
                "CodeRabbit output",
                line.trim(),
                2,
                CodeRabbitReviewStepStatus::Running,
                Some(line.trim().to_string()),
            );
        }
        return;
    };

    if let Some(finding) = normalize_finding_event(project_id, review_id, &value) {
        let findings_count = {
            let mut findings = live_findings.lock();
            findings.push(finding);
            findings.len()
        };
        set_progress(
            progress,
            "CodeRabbit running",
            &format!("Received {findings_count} CodeRabbit recommendation(s) so far."),
            2,
            CodeRabbitReviewStepStatus::Running,
            Some(format!("{findings_count} recommendation(s) received")),
        );
        return;
    }

    let Some(detail) = describe_agent_event(&value) else {
        return;
    };
    set_progress(
        progress,
        "CodeRabbit running",
        &detail,
        2,
        CodeRabbitReviewStepStatus::Running,
        Some(detail.clone()),
    );
}

fn describe_agent_event(value: &Value) -> Option<String> {
    let event_type = string_at(value, &["/type", "/event", "/kind"]);
    let phase = string_at(value, &["/phase", "/status", "/state"]);
    let message = string_at(
        value,
        &[
            "/message",
            "/title",
            "/summary",
            "/detail",
            "/description",
            "/data/message",
        ],
    );

    match (event_type.as_deref(), phase, message) {
        (Some("finding"), _, _) => Some("Received a CodeRabbit recommendation.".to_string()),
        (_, Some(phase), Some(message)) => Some(format!("{phase}: {message}")),
        (_, Some(phase), None) => Some(phase),
        (_, None, Some(message)) => Some(message),
        (Some(event_type), None, None) => Some(format!("Received CodeRabbit event: {event_type}")),
        _ => None,
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else {
        format!("{minutes}m {seconds:02}s")
    }
}

fn write_workflow_instruction_files(
    workdir: &Path,
    workflows: &[CodeRabbitWorkflowId],
) -> anyhow::Result<Vec<PathBuf>> {
    let workflows = if workflows.is_empty() {
        vec![CodeRabbitWorkflowId::Default]
    } else {
        workflows.to_vec()
    };
    let mut paths = Vec::new();
    for workflow in workflows {
        let Some(instructions) = workflow_instructions(&workflow) else {
            continue;
        };
        let path = std::env::temp_dir().join(format!(
            "gitbutler-coderabbit-{}-{}.md",
            workflow_name(&workflow),
            new_review_id()
        ));
        std::fs::write(
            &path,
            format!(
                "{instructions}\n\nRepository root: {}\nSkip Unity raw scene, prefab, asset, meta, generated, dependency, and build-output files unless they are explicitly selected.",
                workdir.display()
            ),
        )?;
        paths.push(path);
    }
    Ok(paths)
}

fn workflow_name(workflow: &CodeRabbitWorkflowId) -> &'static str {
    match workflow {
        CodeRabbitWorkflowId::Default => "default",
        CodeRabbitWorkflowId::Performance => "performance",
        CodeRabbitWorkflowId::Security => "security",
        CodeRabbitWorkflowId::Correctness => "correctness",
    }
}

fn workflow_instructions(workflow: &CodeRabbitWorkflowId) -> Option<&'static str> {
    match workflow {
        CodeRabbitWorkflowId::Default => None,
        CodeRabbitWorkflowId::Performance => Some(
            "Focus this CodeRabbit review on performance risks: avoidable repeated work, expensive rendering/recomputation, N+1 IO, inefficient Git traversal, excessive allocations, and scalability issues. Report only issues that are actionable.",
        ),
        CodeRabbitWorkflowId::Security => Some(
            "Focus this CodeRabbit review on security vulnerabilities: command execution, filesystem access, credential handling, injection, unsafe deserialization, auth bypasses, and secret exposure. Report only issues that are actionable.",
        ),
        CodeRabbitWorkflowId::Correctness => Some(
            "Focus this CodeRabbit review on logic and correctness: state races, stale data, edge cases, error handling, data loss, incorrect line/path mapping, and user-visible regressions. Report only issues that are actionable.",
        ),
    }
}

fn should_skip_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    lower.contains("/node_modules/")
        || lower.contains("/target/")
        || lower.contains("/dist/")
        || lower.contains("/build/")
        || lower.ends_with(".unity")
        || lower.ends_with(".prefab")
        || lower.ends_with(".asset")
        || lower.ends_with(".meta")
}

#[cfg(test)]
fn parse_findings(project_id: &str, review_id: &str, stdout: &str) -> Vec<CodeRabbitFinding> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|value| normalize_finding_event(project_id, review_id, &value))
        .collect()
}

fn normalize_finding_event(
    project_id: &str,
    review_id: &str,
    value: &Value,
) -> Option<CodeRabbitFinding> {
    let is_finding = value
        .get("type")
        .and_then(Value::as_str)
        .map(|kind| kind.eq_ignore_ascii_case("finding"))
        .unwrap_or(false);
    if !is_finding {
        return None;
    }
    normalize_finding(project_id, review_id, value)
}

fn normalize_finding(
    project_id: &str,
    review_id: &str,
    value: &Value,
) -> Option<CodeRabbitFinding> {
    let source = value.get("finding").unwrap_or(value);
    let path = string_at(
        source,
        &[
            "/path",
            "/file",
            "/filePath",
            "/filename",
            "/location/path",
            "/location/file",
        ],
    )?;
    if should_skip_path(&path) {
        return None;
    }

    let title = string_at(source, &["/title", "/message", "/summary"])
        .unwrap_or_else(|| "CodeRabbit finding".to_string());
    let body =
        string_at(source, &["/body", "/description", "/details", "/message"]).unwrap_or_default();

    Some(CodeRabbitFinding {
        id: format!("{}-{}", review_id, uuid::Uuid::new_v4()),
        review_id: review_id.to_string(),
        project_id: project_id.to_string(),
        path,
        old_line: number_at(source, &["/oldLine", "/old_line", "/location/oldLine"]),
        new_line: number_at(
            source,
            &[
                "/newLine",
                "/line",
                "/startLine",
                "/location/line",
                "/location/newLine",
            ],
        ),
        severity: severity_at(source),
        category: string_at(source, &["/category", "/rule", "/type"]),
        title,
        body,
        suggested_patch: string_at(
            source,
            &[
                "/suggestedPatch",
                "/suggestion",
                "/fix/patch",
                "/fix/suggestion",
            ],
        ),
        workflow_id: string_at(source, &["/workflowId", "/workflow"]),
        status: CodeRabbitFindingStatus::Open,
    })
}

fn string_at(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .filter_map(|pointer| value.pointer(pointer))
        .find_map(|value| value.as_str().map(ToOwned::to_owned))
        .filter(|value| !value.trim().is_empty())
}

fn number_at(value: &Value, pointers: &[&str]) -> Option<u32> {
    pointers
        .iter()
        .filter_map(|pointer| value.pointer(pointer))
        .find_map(|value| value.as_u64().and_then(|value| u32::try_from(value).ok()))
}

fn severity_at(value: &Value) -> CodeRabbitSeverity {
    match string_at(value, &["/severity", "/level"])
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "critical" | "error" | "high" => CodeRabbitSeverity::Critical,
        "major" | "warning" | "medium" => CodeRabbitSeverity::Major,
        "info" | "informational" | "notice" => CodeRabbitSeverity::Info,
        _ => CodeRabbitSeverity::Minor,
    }
}

fn new_review_id() -> String {
    format!("{}-{}", now_ms(), uuid::Uuid::new_v4())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_finding() {
        let stdout = r#"{"type":"finding","finding":{"path":"src/main.rs","location":{"line":42},"severity":"major","title":"Slow loop","body":"Avoid repeated scans","suggestedPatch":"patch"}}"#;
        let findings = parse_findings("project", "review", stdout);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "src/main.rs");
        assert_eq!(findings[0].new_line, Some(42));
        assert!(matches!(findings[0].severity, CodeRabbitSeverity::Major));
    }

    #[test]
    fn skips_unity_raw_files() {
        let stdout = r#"{"type":"finding","path":"Assets/Main.unity","line":1,"title":"Noise"}"#;
        assert!(parse_findings("project", "review", stdout).is_empty());
    }
}
