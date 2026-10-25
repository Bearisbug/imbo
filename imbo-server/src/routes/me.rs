//! API-003 logout, API-004/005 me, API-006/007 devices, API-014 agents.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    Account, AccountResponse, Agent, AgentsResponse, Device, DeviceRef, DevicesResponse,
    MeResponse, RenameRequest,
};
use imbo_proto::frames::CLOSE_UNAUTHORIZED;
use imbo_proto::text::validate_name;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::error::{ApiResult, err};
use crate::state::St;

async fn revoke(st: &St, account_id: i32, device: Uuid) -> ApiResult<bool> {
    let r = sqlx::query("UPDATE devices SET revoked_at = now() WHERE id = $1 AND account_id = $2 AND revoked_at IS NULL")
        .bind(device)
        .bind(account_id)
        .execute(&st.db)
        .await?;
    if r.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query(
        "UPDATE agents SET online = false, last_online_at = now() WHERE device_id = $1 AND online",
    )
    .bind(device)
    .execute(&st.db)
    .await?;
    st.hub.close(device, CLOSE_UNAUTHORIZED, "revoked");
    Ok(true)
}

pub async fn logout(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<StatusCode> {
    c.require_person()?;
    revoke(&st, c.account_id, c.device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_me(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<MeResponse>> {
    let (name, email): (String, String) =
        sqlx::query_as("SELECT name, email FROM accounts WHERE id = $1")
            .bind(c.account_id)
            .fetch_one(&st.db)
            .await?;
    let device_name: String = sqlx::query_scalar("SELECT name FROM devices WHERE id = $1")
        .bind(c.device_id)
        .fetch_one(&st.db)
        .await?;
    Ok(Json(MeResponse {
        account: Account {
            id: c.account_id,
            name,
            email: Some(email),
        },
        device: DeviceRef {
            id: c.device_id,
            name: device_name,
        },
    }))
}

pub async fn rename(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<RenameRequest>,
) -> ApiResult<Json<AccountResponse>> {
    c.require_person()?;
    let name = validate_name(&req.name).map_err(|m| err(ErrorCode::InvalidName, m))?;
    let email: String =
        sqlx::query_scalar("UPDATE accounts SET name = $2 WHERE id = $1 RETURNING email")
            .bind(c.account_id)
            .bind(&name)
            .fetch_one(&st.db)
            .await?;
    Ok(Json(AccountResponse {
        account: Account {
            id: c.account_id,
            name,
            email: Some(email),
        },
    }))
}

pub async fn list_devices(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<DevicesResponse>> {
    c.require_person()?;
    let rows: Vec<(Uuid, String, String, DateTime<Utc>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT id, name, platform, created_at, last_seen_at FROM devices
         WHERE account_id = $1 AND revoked_at IS NULL ORDER BY created_at",
    )
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let devices = rows
        .into_iter()
        .map(|(id, name, platform, created_at, last_seen_at)| Device {
            current: id == c.device_id,
            id,
            name,
            platform,
            created_at,
            last_seen_at,
        })
        .collect();
    Ok(Json(DevicesResponse { devices }))
}

pub async fn revoke_device(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    c.require_person()?;
    let id = Uuid::parse_str(&id).map_err(|_| err(ErrorCode::NotFound, "No such computer."))?;
    if !revoke(&st, c.account_id, id).await? {
        return Err(err(ErrorCode::NotFound, "No such computer."));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_agents(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<AgentsResponse>> {
    let rows: Vec<(Uuid, String, String, String, String, bool, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, short_id, session_id, title, tool, online, device_id FROM agents
         WHERE account_id = $1 AND tool <> 'terminal' AND (online OR last_online_at > now() - interval '7 days')
         ORDER BY online DESC, last_online_at DESC NULLS LAST",
    )
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let agents = rows
        .into_iter()
        .map(
            |(id, short_id, session_id, title, tool, online, device_id)| Agent {
                id,
                short_id,
                session_id,
                title,
                tool,
                online,
                device_id,
            },
        )
        .collect();
    Ok(Json(AgentsResponse { agents }))
}
