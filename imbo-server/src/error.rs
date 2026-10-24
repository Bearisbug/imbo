//! Problem+json responses (DESIGN §14).

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use imbo_proto::{Candidate, ErrorCode, Problem};
use tracing::error;

tokio::task_local! {
    /// `Imbo-Request-Id` of the request being handled.
    pub static REQUEST_ID: String;
}

#[derive(Debug)]
pub struct ApiError(pub Problem);

pub type ApiResult<T> = Result<T, ApiError>;

pub fn err(code: ErrorCode, detail: impl Into<String>) -> ApiError {
    ApiError(Problem::new(code, detail))
}

impl ApiError {
    pub fn candidates(mut self, c: Vec<Candidate>) -> Self {
        self.0.candidates = Some(c);
        self
    }

    pub fn retry_after(mut self, s: u64) -> Self {
        self.0.retry_after_s = Some(s.max(1));
        self
    }

    pub fn attempts_left(mut self, n: u32) -> Self {
        self.0.attempts_left = Some(n);
        self
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        error!(error = %e, "database error");
        err(
            ErrorCode::Unavailable,
            "The server's database is unavailable. Try again shortly.",
        )
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        error!(error = %e, "internal error");
        err(
            ErrorCode::Unavailable,
            "The server hit an internal error. Try again shortly.",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut p = self.0;
        if p.instance.is_none() {
            p.instance = REQUEST_ID.try_with(|id| format!("/requests/{id}")).ok();
        }
        let status = StatusCode::from_u16(p.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let retry = p.retry_after_s;
        let mut res = (status, serde_json::to_string(&p).unwrap_or_default()).into_response();
        let h = res.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(s) = retry
            && let Ok(v) = HeaderValue::from_str(&s.to_string())
        {
            h.insert(header::RETRY_AFTER, v);
        }
        res
    }
}
