use anyhow::Context;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

const BATCH_SIZE: i64 = 50;
/// A permanently rejected row stays in SQLite for investigation, but stops
/// blocking the rest of the queue once it has exhausted its retries.
const MAX_SYNC_ATTEMPTS: i64 = 5;

#[derive(Serialize)]
struct OutboxItem {
    id: i64,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Serialize)]
struct SyncRequest {
    items: Vec<OutboxItem>,
}

#[derive(Deserialize)]
struct SyncAck {
    accepted: usize,
    #[serde(default)]
    duplicates: usize,
    #[serde(default)]
    rejected: usize,
    /// Present on current servers. Older servers omit it, so the aggregate
    /// counters remain the fallback.
    #[serde(default)]
    results: Vec<SyncAckItem>,
}

#[derive(Deserialize)]
struct SyncAckItem {
    id: i64,
    status: String,
}

struct PendingRow {
    id: i64,
    kind: String,
    payload_json: String,
}

/// Push pending outbox rows to pos-server. Returns the number of rows synced.
/// Sales go to `/api/v1/sync/sales`; all other kinds go to `/api/v1/sync/events`.
pub async fn push_pending(
    pool: &SqlitePool,
    server_url: &str,
    api_token: &str,
) -> anyhow::Result<u32> {
    let client = Client::new();
    let base = server_url.trim_end_matches('/');
    let mut synced = 0usize;

    loop {
        let rows = fetch_batch(pool).await?;
        if rows.is_empty() {
            return Ok(u32::try_from(synced).unwrap_or(u32::MAX));
        }
        // Partition by destination endpoint.
        let (sales, events): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| r.kind == "sale");

        if !sales.is_empty() {
            synced += post_batch(
                pool,
                &client,
                &format!("{base}/api/v1/sync/sales"),
                api_token,
                sales,
            )
            .await?;
        }
        if !events.is_empty() {
            synced += post_batch(
                pool,
                &client,
                &format!("{base}/api/v1/sync/events"),
                api_token,
                events,
            )
            .await?;
        }
    }
}

async fn fetch_batch(pool: &SqlitePool) -> anyhow::Result<Vec<PendingRow>> {
    let rows = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, kind, payload_json FROM sync_outbox
         WHERE attempts < ? ORDER BY id LIMIT ?",
    )
    .bind(MAX_SYNC_ATTEMPTS)
    .bind(BATCH_SIZE)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, payload_json)| PendingRow {
            id,
            kind,
            payload_json,
        })
        .collect())
}

fn sale_idempotency_key(payload_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload_json)
        .ok()?
        .get("idempotency_key")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

async fn mark_row_synced(pool: &SqlitePool, row: &PendingRow) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM sync_outbox WHERE id = ?")
        .bind(row.id)
        .execute(pool)
        .await?;
    if row.kind == "sale" {
        if let Some(key) = sale_idempotency_key(&row.payload_json) {
            sqlx::query("UPDATE sales SET status = 'synced' WHERE idempotency_key = ?")
                .bind(key)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

async fn mark_row_failed(pool: &SqlitePool, row: &PendingRow, error: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE sync_outbox SET attempts = attempts + 1, last_error = ? WHERE id = ?")
        .bind(error)
        .bind(row.id)
        .execute(pool)
        .await?;
    if row.kind == "sale" {
        if let Some(key) = sale_idempotency_key(&row.payload_json) {
            sqlx::query("UPDATE sales SET status = 'failed' WHERE idempotency_key = ?")
                .bind(key)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

async fn post_batch(
    pool: &SqlitePool,
    client: &Client,
    url: &str,
    api_token: &str,
    rows: Vec<PendingRow>,
) -> anyhow::Result<usize> {
    let request = async {
        let items: Vec<OutboxItem> = rows
            .iter()
            .map(|r| {
                let payload = serde_json::from_str(&r.payload_json)
                    .with_context(|| format!("outbox row {} has invalid JSON", r.id))?;
                Ok(OutboxItem {
                    id: r.id,
                    kind: r.kind.clone(),
                    payload,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let response = client
            .post(url)
            .bearer_auth(api_token)
            .json(&SyncRequest { items })
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("sync failed: {status} {body}");
        }
        response
            .json::<SyncAck>()
            .await
            .with_context(|| format!("invalid sync acknowledgement from {url}"))
    }
    .await;

    // A transport/parse failure gives us no per-row verdict. Count an attempt
    // for every row so one bad row cannot wedge the queue forever.
    let ack = match request {
        Ok(ack) => ack,
        Err(error) => {
            for row in &rows {
                mark_row_failed(pool, row, &error.to_string()).await?;
            }
            return Err(error);
        }
    };

    let mut cleared = 0usize;
    let mut rejected_ids = Vec::new();

    if ack.results.is_empty() {
        // Older servers only return aggregate counters.
        if ack.rejected > 0 || ack.accepted + ack.duplicates != rows.len() {
            let error = format!(
                "sync rejected {}/{} rows (accepted {}, duplicates {})",
                ack.rejected,
                rows.len(),
                ack.accepted,
                ack.duplicates
            );
            for row in &rows {
                mark_row_failed(pool, row, &error).await?;
            }
            anyhow::bail!("{error}");
        }
        for row in &rows {
            mark_row_synced(pool, row).await?;
            cleared += 1;
        }
        return Ok(cleared);
    }

    for row in &rows {
        let status = ack
            .results
            .iter()
            .find(|item| item.id == row.id)
            .map(|item| item.status.as_str());
        match status {
            Some("accepted") | Some("duplicate") => {
                mark_row_synced(pool, row).await?;
                cleared += 1;
            }
            Some("rejected") => {
                mark_row_failed(pool, row, "server rejected row").await?;
                rejected_ids.push(row.id);
            }
            Some(other) => {
                mark_row_failed(pool, row, &format!("unknown sync status '{other}'")).await?;
                rejected_ids.push(row.id);
            }
            None => {
                mark_row_failed(pool, row, "server omitted row from acknowledgement").await?;
                rejected_ids.push(row.id);
            }
        }
    }

    if !rejected_ids.is_empty() {
        anyhow::bail!("sync rejected outbox rows: {rejected_ids:?}");
    }
    Ok(cleared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_ack_defaults_duplicates_for_event_batches() {
        let ack: SyncAck = serde_json::from_str(r#"{"accepted":1,"rejected":0}"#).unwrap();
        assert_eq!(ack.accepted, 1);
        assert_eq!(ack.duplicates, 0);
        assert_eq!(ack.rejected, 0);
        assert!(ack.results.is_empty());
    }

    #[test]
    fn sync_ack_reads_per_row_results() {
        let ack: SyncAck = serde_json::from_str(
            r#"{"accepted":1,"duplicates":0,"rejected":1,
                "results":[{"id":1,"status":"accepted"},{"id":2,"status":"rejected"}]}"#,
        )
        .unwrap();
        assert_eq!(ack.results.len(), 2);
        assert_eq!(ack.results[1].status, "rejected");
    }

    #[test]
    fn rejected_outbox_rows_are_counted_then_skipped_at_cap() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
            sqlx::query(
                "CREATE TABLE sync_outbox (
                    id INTEGER PRIMARY KEY,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT
                 )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE TABLE sales (idempotency_key TEXT PRIMARY KEY, status TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();
            let key = "01997f00-0000-7000-8000-000000000001";
            sqlx::query("INSERT INTO sales (idempotency_key, status) VALUES (?, 'pending_sync')")
                .bind(key)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO sync_outbox (id, kind, payload_json) VALUES (1, 'sale', ?)")
                .bind(format!(r#"{{"idempotency_key":"{key}"}}"#))
                .execute(&pool)
                .await
                .unwrap();

            let row = PendingRow {
                id: 1,
                kind: "sale".into(),
                payload_json: format!(r#"{{"idempotency_key":"{key}"}}"#),
            };
            mark_row_failed(&pool, &row, "server rejected row")
                .await
                .unwrap();

            let attempts: i64 = sqlx::query_scalar("SELECT attempts FROM sync_outbox WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
            let last_error: Option<String> =
                sqlx::query_scalar("SELECT last_error FROM sync_outbox WHERE id = 1")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let sale_status: String =
                sqlx::query_scalar("SELECT status FROM sales WHERE idempotency_key = ?")
                    .bind(key)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(attempts, 1);
            assert_eq!(last_error.as_deref(), Some("server rejected row"));
            assert_eq!(sale_status, "failed");
            assert_eq!(fetch_batch(&pool).await.unwrap().len(), 1);

            sqlx::query("UPDATE sync_outbox SET attempts = ? WHERE id = 1")
                .bind(MAX_SYNC_ATTEMPTS)
                .execute(&pool)
                .await
                .unwrap();
            assert!(fetch_batch(&pool).await.unwrap().is_empty());
        });
    }
}
