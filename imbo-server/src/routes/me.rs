//! API-003 logout, API-004/005 me, API-006/007 devices, API-014 agents.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::MAX_TITLE_CHARS;
use imbo_proto::api::{
    Account, AccountResponse, Agent, AgentResponse, AgentsResponse, Device, DeviceRef,
    DevicesResponse, MeResponse, RenameRequest,
};
use imbo_proto::frames::CLOSE_UNAUTHORIZED;
use imbo_proto::text::{strip_control, validate_name};
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
    let (name, email, avatar): (String, String, Option<Uuid>) =
        sqlx::query_as("SELECT name, email, avatar_id FROM accounts WHERE id = $1")
            .bind(c.account_id)
            .fetch_one(&st.db)
            .await?;
    // ROLE-Admin is a web sign-in with a listed email (ADR-040).
    let admin = c.web.is_some() && super::admin::is_admin(&st, c.account_id).await?;
    let device = if c.web_person() {
        None
    } else {
        let name: String = sqlx::query_scalar("SELECT name FROM devices WHERE id = $1")
            .bind(c.device_id)
            .fetch_one(&st.db)
            .await?;
        Some(DeviceRef {
            id: c.device_id,
            name,
        })
    };
    let access = crate::billing::access(&st.db, &st.cfg, c.account_id).await?;
    Ok(Json(MeResponse {
        account: Account {
            id: c.account_id,
            name,
            email: Some(email),
            avatar_url: crate::avatar::url_of(&st.cfg, avatar),
            admin,
        },
        device,
        access: Some(access.view()),
    }))
}

pub async fn rename(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<RenameRequest>,
) -> ApiResult<Json<AccountResponse>> {
    c.require_person()?;
    let name = validate_name(&req.name).map_err(|m| err(ErrorCode::InvalidName, m))?;
    let (email, avatar): (String, Option<Uuid>) =
        sqlx::query_as("UPDATE accounts SET name = $2 WHERE id = $1 RETURNING email, avatar_id")
            .bind(c.account_id)
            .bind(&name)
            .fetch_one(&st.db)
            .await?;
    Ok(Json(AccountResponse {
        account: Account {
            id: c.account_id,
            name,
            email: Some(email),
            avatar_url: crate::avatar::url_of(&st.cfg, avatar),
            admin: false,
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
    let agents = agents_of(&st, c.account_id, None).await?;
    Ok(Json(AgentsResponse { agents }))
}

/// API-014's rows: my sessions seen in the last 7 days, or just `only`.
async fn agents_of(st: &St, me: i32, only: Option<Uuid>) -> ApiResult<Vec<Agent>> {
    let rows: Vec<(
        Uuid,
        String,
        String,
        String,
        String,
        String,
        bool,
        Option<Uuid>,
        Option<String>,
        Option<DateTime<Utc>>,
        i64,
    )> = sqlx::query_as(
        "SELECT ag.id, ag.short_id, ag.session_id, coalesce(ag.custom_title, ag.title), ag.title, ag.tool, ag.online, ag.device_id, d.name, ag.last_online_at,
                (SELECT count(*) FROM deliveries dl JOIN messages m ON m.id = dl.message_id
                 JOIN agents t ON t.id = dl.agent_id
                 WHERE m.from_agent_id = ag.id AND t.account_id = $1 AND t.tool = 'terminal'
                   AND dl.state IN ('pending', 'pushed')) AS unread
         FROM agents ag LEFT JOIN devices d ON d.id = ag.device_id
         WHERE ag.account_id = $1 AND ag.tool <> 'terminal' AND (ag.online OR ag.last_online_at > now() - interval '7 days')
           AND ($2::uuid IS NULL OR ag.id = $2)
         ORDER BY ag.online DESC, ag.last_online_at DESC NULLS LAST",
    )
    .bind(me)
    .bind(only)
    .fetch_all(&st.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                short_id,
                session_id,
                title,
                tool_title,
                tool,
                online,
                device_id,
                device_name,
                last_online_at,
                unread,
            )| Agent {
                id,
                short_id,
                session_id,
                title,
                tool_title: Some(tool_title),
                tool,
                online,
                device_id,
                device_name,
                last_online_at,
                unread,
            },
        )
        .collect())
}

#[derive(serde::Deserialize)]
pub struct TitleIn {
    title: Option<String>,
}

/// API-073: the name I give one of my sessions shows everywhere in place of
/// the tool's title; an empty one brings the tool's title back (ADR-045).
pub async fn rename_agent(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
    Body(req): Body<TitleIn>,
) -> ApiResult<Json<AgentResponse>> {
    let not_found = || err(ErrorCode::NotFound, "No such session.");
    let id = Uuid::parse_str(&id).map_err(|_| not_found())?;
    let title = strip_control(req.title.as_deref().unwrap_or_default(), false)
        .trim()
        .to_string();
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(err(
            ErrorCode::InvalidRequest,
            "A session name can be at most 200 characters.",
        ));
    }
    if agents_of(&st, c.account_id, Some(id)).await?.is_empty() {
        return Err(not_found());
    }
    sqlx::query("UPDATE agents SET custom_title = $3 WHERE id = $1 AND account_id = $2")
        .bind(id)
        .bind(c.account_id)
        .bind((!title.is_empty()).then_some(title))
        .execute(&st.db)
        .await?;
    let agent = agents_of(&st, c.account_id, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or_else(not_found)?;
    Ok(Json(AgentResponse { agent }))
}
