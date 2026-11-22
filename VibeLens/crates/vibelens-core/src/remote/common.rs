//! Pieces both remote engines share: how a run ends, event emission, the result collector,
//! cell bounds and the row buffer that honours `page_size` and [`PAGE_BYTE_BUDGET`].

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{CoreError, ErrorPayload, Result};
use crate::model::{
    Cell, ExecOptions, MAX_CELL_BYTES, PAGE_BYTE_BUDGET, QueryEvent, QueryResult, TruncatedKind,
    cell_weight, truncated_cell,
};

/// Why a statement stopped without finishing.
pub(crate) enum Stop {
    Err(CoreError),
    /// The event receiver went away: nobody is listening any more.
    Gone,
}

impl From<CoreError> for Stop {
    fn from(e: CoreError) -> Self {
        Stop::Err(e)
    }
}

pub(crate) type Run<T> = std::result::Result<T, Stop>;

/// How a script ended.
pub(crate) enum End {
    Finished,
    Failed {
        statement_index: Option<usize>,
        error: CoreError,
    },
    Gone,
}

impl End {
    pub(crate) fn into_event(self, elapsed_ms: u64) -> Option<QueryEvent> {
        match self {
            End::Finished => Some(QueryEvent::Finished { elapsed_ms }),
            End::Failed {
                statement_index,
                error,
            } => Some(QueryEvent::Failed {
                statement_index,
                error: ErrorPayload::from(&error),
            }),
            End::Gone => None,
        }
    }
}

pub(crate) async fn emit(tx: &mpsc::Sender<QueryEvent>, ev: QueryEvent) -> Run<()> {
    tx.send(ev).await.map_err(|_| Stop::Gone)
}

pub(crate) fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub(crate) async fn sleep_or_cancel(d: Duration, cancel: &CancellationToken) -> Run<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Stop::Err(CoreError::Cancelled)),
        _ = tokio::time::sleep(d) => Ok(()),
    }
}

/// `base * 2^attempt` plus up to 50% jitter.
pub(crate) fn backoff(base: Duration, attempt: u32) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.min(8));
    let r = (uuid::Uuid::new_v4().as_u128() % 1000) as u32;
    exp + exp.mul_f64(f64::from(r) / 2000.0)
}

/// A remote engine's statement loop. `Started` and the terminal event belong to the caller.
pub(crate) trait Script {
    async fn run(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End;
}

/// Same event contract as `PgHandle::execute_stream`: `Started` first and exactly one of
/// `Finished` / `Failed` last (unless the receiver disappears). `Err` only if `Started` could
/// not be delivered.
pub(crate) async fn stream_script<S: Script>(
    engine: &S,
    query_id: String,
    sql: &str,
    opts: ExecOptions,
    cancel: CancellationToken,
    tx: mpsc::Sender<QueryEvent>,
) -> Result<()> {
    let statements = crate::sql::split_statements(sql);
    tx.send(QueryEvent::Started {
        query_id,
        statement_count: statements.len(),
    })
    .await
    .map_err(|_| CoreError::Other("query result receiver was dropped".into()))?;
    let t0 = Instant::now();
    let end = engine.run(&statements, &opts, &cancel, &tx).await;
    if let Some(terminal) = end.into_event(elapsed_ms(t0)) {
        // If the receiver vanished since the last event there is nobody to tell.
        let _ = tx.send(terminal).await;
    }
    Ok(())
}

/// Bounded, materialised execution: the result of the last statement that returns rows (or of
/// the last statement when none does). Errors are returned as `Err`.
pub(crate) async fn collect_script<S: Script>(
    engine: &S,
    sql: &str,
    opts: ExecOptions,
    cancel: CancellationToken,
) -> Result<QueryResult> {
    let statements = crate::sql::split_statements(sql);
    if statements.is_empty() {
        return Ok(QueryResult::default());
    }
    let (tx, mut rx) = mpsc::channel::<QueryEvent>(32);
    let runner = async {
        let end = engine.run(&statements, &opts, &cancel, &tx).await;
        drop(tx);
        end
    };
    let collector = async {
        let mut collected = Collector::default();
        while let Some(ev) = rx.recv().await {
            collected.push(ev);
        }
        collected
    };
    let (end, collected) = tokio::join!(runner, collector);
    match end {
        End::Finished => collected
            .into_result()
            .ok_or_else(|| CoreError::Other("no statement produced a result".into())),
        End::Failed { error, .. } => Err(error),
        End::Gone => Err(CoreError::Other("result collector went away".into())),
    }
}

/// Folds an event stream into the result of the last row-returning statement.
#[derive(Default)]
pub(crate) struct Collector {
    current: QueryResult,
    last_rows: Option<QueryResult>,
    last_any: Option<QueryResult>,
}

impl Collector {
    pub(crate) fn push(&mut self, ev: QueryEvent) {
        match ev {
            QueryEvent::Columns { columns, .. } => {
                self.current = QueryResult {
                    columns,
                    ..QueryResult::default()
                }
            }
            QueryEvent::Rows { rows, .. } => self.current.rows.extend(rows),
            QueryEvent::StatementDone {
                command_tag,
                rows_affected,
                row_count,
                truncated,
                elapsed_ms,
                ..
            } => {
                let mut done = std::mem::take(&mut self.current);
                done.command_tag = command_tag;
                done.rows_affected = rows_affected;
                done.row_count = row_count;
                done.truncated = truncated;
                done.elapsed_ms = elapsed_ms;
                if done.columns.is_empty() {
                    self.last_any = Some(done);
                } else {
                    self.last_rows = Some(done);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn into_result(self) -> Option<QueryResult> {
        self.last_rows.or(self.last_any)
    }
}

/// `cell` with any value larger than [`MAX_CELL_BYTES`] replaced by a [`truncated_cell`] marker.
/// Text keeps a text preview; a nested value (array, map, row) keeps a JSON preview.
pub(crate) fn cap_cell(cell: Cell) -> Cell {
    match cell {
        Cell::String(s) if s.len() > MAX_CELL_BYTES => {
            truncated_cell(TruncatedKind::Text, s.len(), s.as_bytes())
        }
        c @ (Cell::Array(_) | Cell::Object(_)) if cell_weight(&c) > MAX_CELL_BYTES => {
            let text = c.to_string();
            truncated_cell(TruncatedKind::Json, text.len(), text.as_bytes())
        }
        c => c,
    }
}

/// Rows waiting to be sent: flushed at `page_size` rows or [`PAGE_BYTE_BUDGET`] bytes.
pub(crate) struct RowBuf {
    idx: usize,
    page_size: usize,
    rows: Vec<Vec<Cell>>,
    weight: usize,
}

impl RowBuf {
    pub(crate) fn new(idx: usize, page_size: usize) -> Self {
        Self {
            idx,
            page_size: page_size.max(1),
            rows: Vec::new(),
            weight: 0,
        }
    }

    pub(crate) async fn push(&mut self, row: Vec<Cell>, tx: &mpsc::Sender<QueryEvent>) -> Run<()> {
        self.weight += row.iter().map(cell_weight).sum::<usize>();
        self.rows.push(row);
        if self.rows.len() >= self.page_size || self.weight >= PAGE_BYTE_BUDGET {
            self.flush(tx).await?;
        }
        Ok(())
    }

    pub(crate) async fn flush(&mut self, tx: &mpsc::Sender<QueryEvent>) -> Run<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        self.weight = 0;
        emit(
            tx,
            QueryEvent::Rows {
                statement_index: self.idx,
                rows: std::mem::take(&mut self.rows),
            },
        )
        .await
    }
}

/// A statement-level refusal message prefix: `statement 2 of 3: ` for scripts, empty otherwise.
pub(crate) fn which(i: usize, n: usize) -> String {
    if n > 1 {
        format!("statement {} of {n}: ", i + 1)
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::is_truncated_cell;

    #[test]
    fn oversized_cells_become_markers() {
        let big = "x".repeat(MAX_CELL_BYTES + 1);
        let c = cap_cell(json!(big));
        assert!(is_truncated_cell(&c));
        assert_eq!(c["$truncated"]["kind"], "text");
        assert_eq!(c["$truncated"]["bytes"], MAX_CELL_BYTES + 1);
        let c = cap_cell(json!([big]));
        assert!(is_truncated_cell(&c));
        assert_eq!(c["$truncated"]["kind"], "json");
        assert_eq!(cap_cell(json!("small")), json!("small"));
        assert_eq!(cap_cell(json!([1, 2])), json!([1, 2]));
    }

    #[tokio::test]
    async fn rows_flush_at_the_page_size_and_at_the_byte_budget() {
        let (tx, mut rx) = mpsc::channel(16);
        let mut b = RowBuf::new(0, 2);
        for i in 0..3 {
            assert!(b.push(vec![json!(i)], &tx).await.is_ok());
        }
        assert!(b.flush(&tx).await.is_ok());
        let mut sizes = Vec::new();
        while let Ok(QueryEvent::Rows { rows, .. }) = rx.try_recv() {
            sizes.push(rows.len());
        }
        assert_eq!(sizes, [2, 1]);

        let mut b = RowBuf::new(0, 1000);
        let cell = json!("y".repeat(PAGE_BYTE_BUDGET / 2));
        for _ in 0..2 {
            assert!(b.push(vec![cell.clone()], &tx).await.is_ok());
        }
        assert!(matches!(rx.try_recv(), Ok(QueryEvent::Rows { rows, .. }) if rows.len() == 2));
    }
}
