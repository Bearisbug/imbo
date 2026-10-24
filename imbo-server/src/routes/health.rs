//! API-025 health, with the operating fields of DESIGN §21.

use std::time::SystemTime;

use axum::Json;
use axum::extract::State;
use imbo_proto::ErrorCode;
use imbo_proto::api::Health;

use crate::error::{ApiResult, err};
use crate::state::St;

fn disk_free_pct(path: &std::path::Path) -> Option<f64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 || s.f_blocks == 0 {
        return None;
    }
    Some((s.f_bavail as f64 / s.f_blocks as f64 * 1000.0).round() / 10.0)
}

pub async fn health(State(st): State<St>) -> ApiResult<Json<Health>> {
    let ok: Result<i32, _> = sqlx::query_scalar("SELECT 1").fetch_one(&st.db).await;
    if ok.is_err() {
        return Err(err(ErrorCode::Unavailable, "Database unavailable."));
    }
    let stuck: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM deliveries d JOIN agents a ON a.id = d.agent_id
         WHERE d.state IN ('pending', 'pushed') AND a.online AND d.created_at < now() - interval '5 minutes'",
    )
    .fetch_one(&st.db)
    .await?;
    let p95: Option<f64> = sqlx::query_scalar(
        "SELECT percentile_cont(0.95) WITHIN GROUP (ORDER BY extract(epoch FROM d.delivered_at - m.created_at) * 1000)
         FROM deliveries d JOIN messages m ON m.id = d.message_id
         WHERE d.via = 'inject' AND d.attempts = 1 AND d.delivered_at > now() - interval '1 hour'",
    )
    .fetch_one(&st.db)
    .await?;
    let backup_age_h = st.cfg.backup_stamp.as_ref().and_then(|p| {
        let modified = std::fs::metadata(p).ok()?.modified().ok()?;
        let age = SystemTime::now().duration_since(modified).ok()?;
        Some((age.as_secs_f64() / 360.0).round() / 10.0)
    });
    Ok(Json(Health {
        status: "ok".into(),
        name: "imbo-server".into(),
        version: crate::VERSION.into(),
        db: "ok".into(),
        ws_devices: st.hub.count(),
        stuck_deliveries: Some(stuck),
        backup_age_h,
        disk_free_pct: disk_free_pct(&st.cfg.data_dir),
        ack_p95_ms: p95.map(|v| v.round()),
    }))
}
