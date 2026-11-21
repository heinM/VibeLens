//! Spark SQL through Apache Livy's REST API.
//!
//! * **Session.** Created on the first query (`POST /sessions` with `kind: "sql"`, the profile's
//!   `proxyUser` and `sessionConf`), polled until `idle`, then switched to the connection's
//!   catalog and namespace with `USE`. It is reused for later queries and deleted when the
//!   connection closes ([`SparkEngine::close`]). A session that has died or that Livy has
//!   garbage-collected is replaced once, transparently, when a statement cannot be submitted
//!   to it. The engine never lists or joins sessions it did not create.
//! * **Statement.** `POST /sessions/{id}/statements` with `kind: "sql"`, then
//!   `GET /sessions/{id}/statements/{n}` until `available`. Rows and schema come from
//!   `output.data["application/json"]`. Cancel (and timeout) is
//!   `POST /sessions/{id}/statements/{n}/cancel`.
//! * **Identifiers from the server** (session and statement ids) must be non-negative JSON
//!   integers before they are put into a URL.
//! * **Errors** keep `evalue` (redacted, at most 4 KiB) and drop the Java traceback.
//! * **Result size.** Livy returns a SQL result in one response, cut by the server at
//!   `livy.rsc.sql.num-rows` (1000 by default) without saying so. A result with exactly
//!   [`LIVY_DEFAULT_ROW_CAP`] rows is therefore reported as truncated: it may be.
//! * **Session state.** `USE`, `SET` and `RESET` would change the shared session for every later
//!   statement (including the agent's, whose approval card names the profile's catalog), so they
//!   are refused in every mode.

use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::Method;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::RemoteTiming;
use super::common::{
    End, RowBuf, Run, Script, Stop, cap_cell, collect_script, elapsed_ms, emit, sleep_or_cancel,
    stream_script, which,
};
use super::http::{Body, HttpClient, HttpLimits, MAX_ERROR_TEXT, SMALL_BODY, bounded};
use crate::error::{CoreError, Result};
use crate::model::*;

/// Livy's default `livy.rsc.sql.num-rows`: the most rows a SQL statement returns.
pub const LIVY_DEFAULT_ROW_CAP: usize = 1000;
/// Default wait for a new session to become idle.
const DEFAULT_SESSION_START: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct SparkConfig {
    /// Livy base URL.
    pub url: String,
    /// Spark catalog name configured for this Iceberg catalog.
    pub catalog: String,
    /// The connection's default namespace (`a.b` for nested).
    pub namespace: Option<String>,
    pub auth: HttpEngineAuth,
    pub ca_cert_path: Option<String>,
    pub proxy_user: Option<String>,
    pub session_conf: BTreeMap<String, String>,
    /// `None`: 300 s.
    pub session_start_timeout: Option<Duration>,
}

pub struct SparkEngine {
    cfg: SparkConfig,
    client: HttpClient,
    timing: RemoteTiming,
    /// The Livy session this engine created, once it is idle and switched to the catalog.
    session: StdMutex<Option<Live>>,
    /// One script at a time: a Livy session runs one statement at a time anyway, and the
    /// session is created and replaced under this lock.
    run_lock: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

/// A session this engine created: its id and the unique name it was created with. Livy reuses
/// ids after a restart, so the name is what tells our session from someone else's.
#[derive(Debug, Clone)]
struct Live {
    id: u64,
    name: String,
}

/// Most columns a result may declare. A result is materialised per statement, so this bounds
/// the per-row work and the column metadata.
pub(crate) const MAX_COLUMNS: usize = 2000;

/// Deletes a session that was created but not handed over yet when the future creating it is
/// dropped (agent Stop, a timeout higher up), so that no session leaks without its id stored.
struct DeleteOnDrop {
    client: Option<HttpClient>,
    id: u64,
}

impl DeleteOnDrop {
    fn disarm(&mut self) {
        self.client = None;
    }
}

impl Drop for DeleteOnDrop {
    fn drop(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        let id = self.id;
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Ok(url) = client.url(&format!("/sessions/{id}")) {
                    let del = client.send_capped(Method::DELETE, &url, &[], None, SMALL_BODY);
                    let _ = tokio::time::timeout(Duration::from_secs(10), del).await;
                }
            });
        }
    }
}

/// What submitting a statement found.
enum Submitted {
    Id(u64),
    /// The session no longer exists (404) or is dead: it may be replaced.
    SessionGone,
}

impl SparkEngine {
    /// `secret` is the password (basic) or token (bearer) from the vault; ignored for `none`.
    pub fn new(cfg: SparkConfig, secret: Option<SecretString>) -> Result<Self> {
        Self::with_limits(cfg, secret, HttpLimits::default())
    }

    /// [`new`](Self::new) with other HTTP limits (tests).
    pub fn with_limits(
        cfg: SparkConfig,
        secret: Option<SecretString>,
        limits: HttpLimits,
    ) -> Result<Self> {
        super::config::check_session_conf(&cfg.session_conf, &cfg.catalog)?;
        let client = HttpClient::new(
            "Livy",
            &cfg.url,
            &cfg.auth,
            secret,
            cfg.ca_cert_path.as_deref(),
            limits,
        )?;
        Ok(Self {
            cfg,
            client,
            timing: RemoteTiming::default(),
            session: StdMutex::new(None),
            run_lock: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        })
    }

    /// Overrides polling and retry pacing.
    #[must_use]
    pub fn with_timing(mut self, timing: RemoteTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Same event contract as `PgHandle::execute_stream`. `opts.read_only` → reject anything
    /// that is not provably a read ([`super::check_spark_read_only`]) before anything is sent.
    pub async fn execute_stream(
        &self,
        query_id: String,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        stream_script(self, query_id, sql, opts, cancel, tx).await
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does). Errors are returned as `Err`.
    pub async fn execute_collect(&self, sql: &str, opts: ExecOptions) -> Result<QueryResult> {
        collect_script(self, sql, opts, CancellationToken::new()).await
    }

    /// [`execute_collect`](Self::execute_collect) that stops when `cancel` fires, after the
    /// running statement has been cancelled on the server.
    pub async fn execute_collect_cancellable(
        &self,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
    ) -> Result<QueryResult> {
        collect_script(self, sql, opts, cancel).await
    }

    /// The id of the Livy session this engine holds, if any (diagnostics and tests).
    pub fn session_id(&self) -> Option<u64> {
        self.live().map(|l| l.id)
    }

    fn live(&self) -> Option<Live> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Deletes the Livy session, best effort, if it is still the one this engine created. Later
    /// runs fail. A statement still running in the session ends with it.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let live = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(live) = live
            && matches!(self.check_live(&live).await, Ok(true))
        {
            self.delete_session(live.id).await;
        }
    }

    fn set_session(&self, live: Option<Live>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = live;
    }

    /// Whether the session behind `live.id` is still ours (same name, same proxy user) and
    /// usable. After a Livy restart the id may belong to someone else's session.
    async fn check_live(&self, live: &Live) -> Run<bool> {
        let resp = self
            .request(Method::GET, &format!("/sessions/{}", live.id), None)
            .await?;
        if !resp.is_success() {
            return Ok(false);
        }
        let v = parse(&resp.body)?;
        let name_ok = v.get("name").and_then(Value::as_str) == Some(live.name.as_str());
        let want_proxy = self.proxy_user();
        let proxy_ok = match want_proxy {
            Some(p) => v.get("proxyUser").and_then(Value::as_str) == Some(p),
            None => true,
        };
        let state = v.get("state").and_then(Value::as_str).unwrap_or("");
        let usable = !matches!(
            state,
            "dead" | "killed" | "error" | "shutting_down" | "success"
        );
        Ok(name_ok && proxy_ok && usable)
    }

    fn proxy_user(&self) -> Option<&str> {
        self.cfg
            .proxy_user
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }

    fn url(&self, path: &str) -> Run<url::Url> {
        self.client.url(path).map_err(Stop::Err)
    }

    /// A request whose answer carries no rows: bounded by [`SMALL_BODY`].
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Run<super::http::Response> {
        let url = self.url(path)?;
        Ok(self
            .client
            .send_capped(method, &url, &[], body.map(Body::Json), SMALL_BODY)
            .await?)
    }

    /// A statement status request, whose final answer carries the rows.
    async fn request_result(&self, path: &str) -> Run<super::http::Response> {
        let url = self.url(path)?;
        Ok(self.client.send(Method::GET, &url, &[], None).await?)
    }

    /// The session to run in, creating it when there is none.
    async fn ensure_session(&self, cancel: &CancellationToken) -> Run<u64> {
        if let Some(live) = self.live() {
            return Ok(live.id);
        }
        let live = self.create_session(cancel).await?;
        let id = live.id;
        self.set_session(Some(live));
        Ok(id)
    }

    async fn create_session(&self, cancel: &CancellationToken) -> Run<Live> {
        let name = format!("vibelens-{}", uuid::Uuid::new_v4().simple());
        let mut body = json!({
            "kind": "sql",
            "name": name,
            "conf": self.cfg.session_conf,
        });
        if let Some(p) = self.proxy_user() {
            body["proxyUser"] = json!(p);
        }
        // Not raced against cancellation: abandoning it could leave a session behind that we
        // have no id for. The request timeout bounds it.
        let resp = self.request(Method::POST, "/sessions", Some(body)).await?;
        if !resp.is_success() {
            return Err(self
                .client
                .status_error("the request to start a session", &resp)
                .into());
        }
        let v = parse(&resp.body)?;
        let id = livy_id(&v, "session")?;
        // From here until the session is stored, dropping this future deletes it.
        let mut guard = DeleteOnDrop {
            client: Some(self.client.clone()),
            id,
        };
        let ready = match self.wait_idle(id, cancel).await {
            Ok(()) => self.switch_catalog(id, cancel).await,
            Err(stop) => Err(stop),
        };
        guard.disarm();
        if let Err(stop) = ready {
            self.delete_session(id).await;
            return Err(stop);
        }
        // `close()` may have run while the session was starting: it would not have seen it.
        if self.closed.load(Ordering::SeqCst) {
            self.delete_session(id).await;
            return Err(CoreError::NotConnected("the Spark connection was closed".into()).into());
        }
        Ok(Live { id, name })
    }

    /// Polls the new session until it is `idle`, within the session start timeout.
    async fn wait_idle(&self, id: u64, cancel: &CancellationToken) -> Run<()> {
        let limit = self
            .cfg
            .session_start_timeout
            .unwrap_or(DEFAULT_SESSION_START);
        let deadline = Instant::now() + limit;
        let mut delay = self.timing.poll_initial;
        let path = format!("/sessions/{id}");
        loop {
            let resp = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Stop::Err(CoreError::Cancelled)),
                r = self.request(Method::GET, &path, None) => r?,
            };
            if !resp.is_success() {
                return Err(self
                    .client
                    .status_error("the session status request", &resp)
                    .into());
            }
            let v = parse(&resp.body)?;
            let state = v.get("state").and_then(Value::as_str).unwrap_or("");
            match state {
                "idle" => return Ok(()),
                "not_started" | "starting" | "busy" | "" => {}
                other => {
                    return Err(CoreError::Database(format!(
                        "the Spark session could not start: Livy reports it {}",
                        bounded(other, 64)
                    ))
                    .into());
                }
            }
            if Instant::now() >= deadline {
                return Err(CoreError::Timeout(format!(
                    "the Spark session was not ready after {} s; deleted it. A busy cluster may \
                     need a longer session start timeout",
                    limit.as_secs()
                ))
                .into());
            }
            sleep_or_cancel(delay, cancel).await?;
            delay = (delay * 2).min(self.timing.poll_max);
        }
    }

    /// `USE catalog.namespace` as the session's first statement.
    async fn switch_catalog(&self, id: u64, cancel: &CancellationToken) -> Run<()> {
        let mut target = quote_ident(self.cfg.catalog.trim());
        if let Some(ns) = self
            .cfg
            .namespace
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            for part in ns.split('.') {
                target.push('.');
                target.push_str(&quote_ident(part));
            }
        }
        let code = format!("USE {target}");
        let n = match self.submit(id, &code).await? {
            Submitted::Id(n) => n,
            Submitted::SessionGone => {
                return Err(CoreError::Database(
                    "the new Spark session ended before it could be used".into(),
                )
                .into());
            }
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        match self
            .wait_statement(id, n, Some(deadline), None, cancel)
            .await
        {
            Ok(_) => Ok(()),
            Err(Stop::Err(CoreError::Database(m))) => Err(CoreError::Database(format!(
                "the Spark session could not switch to {target}: {m}"
            ))
            .into()),
            Err(other) => Err(other),
        }
    }

    async fn submit(&self, session: u64, code: &str) -> Run<Submitted> {
        let resp = self
            .request(
                Method::POST,
                &format!("/sessions/{session}/statements"),
                Some(json!({ "kind": "sql", "code": code })),
            )
            .await?;
        if resp.status == 404 {
            return Ok(Submitted::SessionGone);
        }
        if !resp.is_success() {
            // Livy answers 400/500 "Session is in state dead" for a session that died.
            let text = String::from_utf8_lossy(&resp.body).to_ascii_lowercase();
            if ["dead", "killed", "shutting_down", "error"]
                .iter()
                .any(|s| text.contains(&format!("state {s}")))
            {
                return Ok(Submitted::SessionGone);
            }
            return Err(self.client.status_error("the statement", &resp).into());
        }
        let v = parse(&resp.body)?;
        Ok(Submitted::Id(livy_id(&v, "statement")?))
    }

    /// Polls a statement until it is `available`; returns its `output`. On cancel or timeout
    /// the statement is cancelled on the server.
    async fn wait_statement(
        &self,
        session: u64,
        n: u64,
        deadline: Option<Instant>,
        timeout_ms: Option<u64>,
        cancel: &CancellationToken,
    ) -> Run<Value> {
        let res = self
            .poll_statement(session, n, deadline, timeout_ms, cancel)
            .await;
        if matches!(
            res,
            Err(Stop::Gone | Stop::Err(CoreError::Cancelled | CoreError::Timeout(_)))
        ) {
            self.cancel_best_effort(session, n).await;
        }
        res
    }

    async fn poll_statement(
        &self,
        session: u64,
        n: u64,
        deadline: Option<Instant>,
        timeout_ms: Option<u64>,
        cancel: &CancellationToken,
    ) -> Run<Value> {
        let path = format!("/sessions/{session}/statements/{n}");
        let mut delay = self.timing.poll_initial;
        loop {
            let resp = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Stop::Err(CoreError::Cancelled)),
                _ = until(deadline) => return Err(timeout(timeout_ms).into()),
                r = self.request_result(&path) => r?,
            };
            if resp.status == 404 {
                return Err(CoreError::Database(
                    "the Spark session ended while the statement was running".into(),
                )
                .into());
            }
            if !resp.is_success() {
                return Err(self
                    .client
                    .status_error("the statement status request", &resp)
                    .into());
            }
            let mut v = parse(&resp.body)?;
            match v.get("state").and_then(Value::as_str).unwrap_or("") {
                "available" => return Ok(v.get_mut("output").map(Value::take).unwrap_or_default()),
                "waiting" | "running" | "" => {}
                "cancelling" | "cancelled" => return Err(CoreError::Cancelled.into()),
                other => {
                    return Err(CoreError::Database(format!(
                        "the Spark statement ended in state {}",
                        bounded(other, 64)
                    ))
                    .into());
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(timeout(timeout_ms).into());
            }
            sleep_or_cancel(delay, cancel).await?;
            delay = (delay * 2).min(self.timing.poll_max);
        }
    }

    async fn cancel_best_effort(&self, session: u64, n: u64) {
        let path = format!("/sessions/{session}/statements/{n}/cancel");
        let req = self.request(Method::POST, &path, Some(json!({})));
        // Best effort: a failure here must not mask the reason the statement ended.
        match tokio::time::timeout(self.timing.cancel_timeout, req).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => tracing::warn!("Livy statement cancel failed"),
            Err(_) => tracing::warn!("Livy statement cancel timed out"),
        }
    }

    async fn delete_session(&self, id: u64) {
        let path = format!("/sessions/{id}");
        let req = self.request(Method::DELETE, &path, None);
        match tokio::time::timeout(self.timing.cancel_timeout, req).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => tracing::warn!("Livy session delete failed"),
            Err(_) => tracing::warn!("Livy session delete timed out"),
        }
    }

    async fn run_statement(
        &self,
        idx: usize,
        stmt: &str,
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<()> {
        if cancel.is_cancelled() {
            return Err(Stop::Err(CoreError::Cancelled));
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(CoreError::NotConnected("the Spark connection was closed".into()).into());
        }
        let mut replaced = false;
        let (session, n, t0) = loop {
            let session = self.ensure_session(cancel).await?;
            let t0 = Instant::now();
            match self.submit(session, stmt).await? {
                Submitted::Id(n) => break (session, n, t0),
                Submitted::SessionGone if !replaced => {
                    // Died or garbage-collected since the last query: start a new one, once.
                    tracing::info!("Livy session {session} is gone; starting a new one");
                    replaced = true;
                    self.set_session(None);
                }
                Submitted::SessionGone => {
                    self.set_session(None);
                    return Err(CoreError::Database(
                        "the Spark session ended right after it was started".into(),
                    )
                    .into());
                }
            }
        };
        let deadline = opts.timeout_ms.map(|ms| t0 + Duration::from_millis(ms));
        let mut output = self
            .wait_statement(session, n, deadline, opts.timeout_ms, cancel)
            .await?;
        if output.get("status").and_then(Value::as_str) == Some("error") {
            return Err(CoreError::Database(self.failure_message(&output)).into());
        }
        // Taken out of the answer, not copied: a result can be large.
        let mut result = output
            .get_mut("data")
            .and_then(|d| d.get_mut("application/json"))
            .map(Value::take)
            .unwrap_or(Value::Null);
        let fields = match result
            .get_mut("schema")
            .and_then(|s| s.get_mut("fields"))
            .map(Value::take)
        {
            Some(Value::Array(f)) => f,
            _ => Vec::new(),
        };
        if fields.len() > MAX_COLUMNS {
            return Err(CoreError::Other(format!(
                "Livy reported {} columns; VibeLens reads at most {MAX_COLUMNS}",
                fields.len()
            ))
            .into());
        }
        let mut row_count = 0usize;
        let mut truncated = false;
        if !fields.is_empty() {
            let mut types = Vec::with_capacity(fields.len());
            let mut columns = Vec::with_capacity(fields.len());
            for f in &fields {
                let name = f.get("name").and_then(Value::as_str).unwrap_or("");
                let ty = type_name(f.get("type"));
                columns.push(ColumnMeta {
                    name: bounded(name, 1024),
                    type_name: bounded(&ty, 1024),
                    type_oid: 0,
                });
                types.push(ty);
            }
            emit(
                tx,
                QueryEvent::Columns {
                    statement_index: idx,
                    columns,
                },
            )
            .await?;
            let rows = match result.get_mut("data").map(Value::take) {
                Some(Value::Array(r)) => r,
                _ => Vec::new(),
            };
            let total = rows.len();
            let mut buf = RowBuf::new(idx, opts.page_size);
            for row in rows {
                if row_count >= opts.max_rows {
                    truncated = true;
                    break;
                }
                let Value::Array(values) = row else {
                    return Err(
                        CoreError::Other("Livy sent a row that is not a list".into()).into(),
                    );
                };
                if values.len() != types.len() {
                    return Err(CoreError::Other(format!(
                        "Livy sent a row of {} values for {} columns",
                        values.len(),
                        types.len()
                    ))
                    .into());
                }
                let cells = types
                    .iter()
                    .zip(values)
                    .map(|(t, v)| spark_cell(t, v))
                    .collect();
                buf.push(cells, tx).await?;
                row_count += 1;
            }
            buf.flush(tx).await?;
            // Livy cuts SQL results at `livy.rsc.sql.num-rows` without saying so.
            if total == LIVY_DEFAULT_ROW_CAP {
                truncated = true;
            }
        }
        emit(
            tx,
            QueryEvent::Stats {
                statement_index: idx,
                engine: "spark".to_string(),
                bytes_scanned: None,
                cost_usd_estimate: None,
                engine_query_id: Some(format!("{session}/{n}")),
            },
        )
        .await?;
        emit(
            tx,
            QueryEvent::StatementDone {
                statement_index: idx,
                command_tag: crate::athena::command_tag(stmt),
                rows_affected: None,
                row_count,
                truncated,
                elapsed_ms: elapsed_ms(t0),
            },
        )
        .await
    }

    /// `Spark query failed: <ename>: <evalue>`, redacted and at most 4 KiB. The traceback is
    /// dropped.
    fn failure_message(&self, output: &Value) -> String {
        let ename = output
            .get("ename")
            .and_then(Value::as_str)
            .unwrap_or("Error");
        let evalue = output
            .get("evalue")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        format!(
            "Spark query failed: {}",
            self.client
                .redact(&format!("{}: {evalue}", bounded(ename, 128)))
        )
    }
}

impl Script for SparkEngine {
    async fn run(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End {
        // Every check runs over the whole script before anything is sent.
        for (i, stmt) in statements.iter().enumerate() {
            if let Some(why) = super::spark_scan::substitution_refusal(stmt) {
                return End::Failed {
                    statement_index: None,
                    error: CoreError::PolicyDenied(format!(
                        "{}not sent to Spark because {why}",
                        which(i, statements.len())
                    )),
                };
            }
            if let Some(kind) = super::spark_scan::session_statement(stmt) {
                return End::Failed {
                    statement_index: None,
                    error: CoreError::PolicyDenied(format!(
                        "{}{kind} is not available on Spark here: it would change the shared \
                         session for every later statement. Qualify names as \
                         catalog.namespace.table instead; session settings belong in the \
                         connection's Spark configuration",
                        which(i, statements.len())
                    )),
                };
            }
        }
        if opts.read_only
            && let Err(error) = super::spark_scan::check_spark_read_only(statements)
        {
            return End::Failed {
                statement_index: None,
                error,
            };
        }
        // Waiting for another run to finish can be cancelled too.
        let _one_at_a_time = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return End::Failed {
                    statement_index: None,
                    error: CoreError::Cancelled,
                };
            }
            g = self.run_lock.lock() => g,
        };
        // The stored id must still name our session: after a Livy restart ids start again at 0
        // and could belong to someone else's session.
        if let Some(live) = self.live() {
            match self.check_live(&live).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(
                        "Livy session {} is no longer ours; starting a new one",
                        live.id
                    );
                    self.set_session(None);
                }
                Err(Stop::Gone) => return End::Gone,
                Err(Stop::Err(error)) => {
                    return End::Failed {
                        statement_index: None,
                        error,
                    };
                }
            }
        }
        for (idx, stmt) in statements.iter().enumerate() {
            match self.run_statement(idx, stmt, opts, cancel, tx).await {
                Ok(()) => {}
                Err(Stop::Gone) => return End::Gone,
                Err(Stop::Err(error)) => {
                    return End::Failed {
                        statement_index: Some(idx),
                        error,
                    };
                }
            }
        }
        End::Finished
    }
}

/// A session or statement id from Livy: a non-negative JSON integer, or the answer is refused.
/// It goes into URLs, so nothing else may pass.
fn livy_id(v: &Value, what: &str) -> Result<u64> {
    v.get("id").and_then(Value::as_u64).ok_or_else(|| {
        CoreError::Other(format!(
            "Livy returned a {what} id that is not a non-negative integer; refusing to use it"
        ))
    })
}

fn parse(body: &[u8]) -> Result<Value> {
    serde_json::from_slice(body).map_err(|e| {
        CoreError::Other(format!(
            "Livy sent an answer that is not the expected JSON: {}",
            bounded(&e.to_string(), MAX_ERROR_TEXT)
        ))
    })
}

fn timeout(timeout_ms: Option<u64>) -> CoreError {
    CoreError::Timeout(format!(
        "Spark statement still running after {} ms; cancelled it",
        timeout_ms.unwrap_or_default()
    ))
}

/// Resolves when `deadline` passes; never without one.
async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

/// `` `name` `` with backticks doubled.
fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// A Spark type as text: `long`, `decimal(10,2)`, or `struct` / `array` / `map` for nested
/// types (which Spark's JSON schema writes as objects).
fn type_name(t: Option<&Value>) -> String {
    match t {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => o
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("struct")
            .to_string(),
        _ => "unknown".to_string(),
    }
}

/// Converts one Spark value: `long` and `decimal` become strings (JS numbers lose precision
/// past 2^53), everything else is kept as Livy sent it. Oversized values become markers.
fn spark_cell(ty: &str, v: Value) -> Cell {
    let base = ty
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let cell = match v {
        Value::Number(n) if matches!(base.as_str(), "long" | "bigint" | "decimal") => {
            Value::String(n.to_string())
        }
        other => other,
    };
    cap_cell(cell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_must_be_non_negative_integers() {
        assert_eq!(livy_id(&json!({"id": 7}), "session").unwrap(), 7);
        for bad in [
            json!({"id": -1}),
            json!({"id": 1.5}),
            json!({"id": "7"}),
            json!({"id": "../admin"}),
            json!({"id": null}),
            json!({}),
        ] {
            assert!(livy_id(&bad, "session").is_err(), "{bad}");
        }
    }

    #[test]
    fn identifiers_are_quoted_with_backticks_doubled() {
        assert_eq!(quote_ident("lake"), "`lake`");
        assert_eq!(quote_ident("a`b"), "`a``b`");
    }

    #[test]
    fn cells_and_types() {
        assert_eq!(
            spark_cell("long", json!(9007199254740993u64)),
            json!("9007199254740993")
        );
        assert_eq!(spark_cell("decimal(10,2)", json!(1.5)), json!("1.5"));
        assert_eq!(spark_cell("integer", json!(3)), json!(3));
        assert_eq!(spark_cell("string", json!("x")), json!("x"));
        assert_eq!(type_name(Some(&json!("long"))), "long");
        assert_eq!(
            type_name(Some(&json!({"type": "array", "elementType": "long"}))),
            "array"
        );
    }
}
