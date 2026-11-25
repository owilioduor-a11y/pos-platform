use anyhow::Context;
use reqwest::Client;
use serde::Serialize;
use sqlx::SqlitePool;

const BATCH_SIZE: i64 = 50;

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
) -> anyhow::Result<usize> {
    let client = Client::new();
    let base = server_url.trim_end_matches('/');
    let mut synced = 0usize;

    loop {
        let rows = fetch_batch(pool).await?;
        if rows.is_empty() {
            return Ok(synced);
        }
        synced += rows.len();

        // Partition by destination endpoint.
        let (sales, events): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| r.kind == "sale");

        if !sales.is_empty() {
            post_batch(
                pool,
                &client,
                &format!("{base}/api/v1/sync/sales"),
                api_token,
                sales,
            )
            .await?;
        }
        if !events.is_empty() {
            post_batch(
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
         ORDER BY id LIMIT ?",
    )
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

async fn post_batch(
    pool: &SqlitePool,
    client: &Client,
    url: &str,
    api_token: &str,
    rows: Vec<PendingRow>,
) -> anyhow::Result<()> {
    let items: Vec<OutboxItem> = rows
        .iter()
        .map(|r| OutboxItem {
            id: r.id,
            kind: r.kind.clone(),
            payload: serde_json::from_str(&r.payload_json).unwrap_or(serde_json::Value::Null),
        })
        .collect();

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

    for row in &rows {
        sqlx::query("DELETE FROM sync_outbox WHERE id = ?")
            .bind(row.id)
            .execute(pool)
            .await?;

        // Mark the mirrored sales-table row as synced when applicable.
        if row.kind == "sale" {
            if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&row.payload_json) {
                if let Some(key) = payload.get("idempotency_key").and_then(|v| v.as_str()) {
                    sqlx::query("UPDATE sales SET status = 'synced' WHERE idempotency_key = ?")
                        .bind(key)
                        .execute(pool)
                        .await?;
                }
            }
        }
    }
    Ok(())
}
