//! Which of an account's sessions each contact may see (RULE-018,
//! RULE-019, ADR-062). The rule itself is the SQL function
//! `imbo_session_visible` from migration 0014.

use imbo_proto::api::{SHARE_ALL, SHARE_CHOSEN, SHARE_NONE};
use sqlx::PgConnection;
use uuid::Uuid;

/// RULE-019: my session wrote to `peer`, so `peer` sees it from now on.
/// Called inside the send transaction, which already holds my contacts row.
pub async fn reveal(
    tx: &mut PgConnection,
    owner: i32,
    peer: i32,
    agent: Uuid,
) -> Result<(), sqlx::Error> {
    let share: Option<String> = sqlx::query_scalar(
        "SELECT share FROM contacts WHERE account_id = $1 AND peer_id = $2 FOR UPDATE",
    )
    .bind(owner)
    .bind(peer)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(share) = share else {
        return Ok(());
    };
    if share == SHARE_ALL {
        return Ok(());
    }
    let seen: bool = sqlx::query_scalar(
        "SELECT imbo_session_visible(account_id, $2, id, device_id) FROM agents WHERE id = $1",
    )
    .bind(agent)
    .bind(peer)
    .fetch_one(&mut *tx)
    .await?;
    if seen {
        return Ok(());
    }
    if share == SHARE_NONE {
        sqlx::query("UPDATE contacts SET share = $3 WHERE account_id = $1 AND peer_id = $2")
            .bind(owner)
            .bind(peer)
            .bind(SHARE_CHOSEN)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO contact_shares (id, account_id, peer_id, agent_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(owner)
    .bind(peer)
    .bind(agent)
    .execute(&mut *tx)
    .await?;
    Ok(())
}
