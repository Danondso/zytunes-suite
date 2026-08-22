//! Optional Bearer-token auth middleware.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// When `expected` is `Some(token)`, require `Authorization: Bearer <token>`.
/// When `None`, all requests pass through.
pub async fn require_bearer(
    expected: Option<String>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(token) = expected.as_deref() else {
        return Ok(next.run(req).await);
    };
    let ok = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| constant_time_eq(t, token));
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Compare two strings without leaking equality-per-byte via early exit —
/// only their length is observable, not where a mismatch first occurs.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Axum middleware layer factory: clones the optional token into each request.
pub async fn auth_middleware(
    axum::extract::State(token): axum::extract::State<Option<String>>,
    req: Request,
    next: Next,
) -> Response {
    match require_bearer(token, req, next).await {
        Ok(resp) => resp,
        Err(status) => status.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_string_equality() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "secre1"));
        assert!(!constant_time_eq("secret", "secrets"));
        assert!(!constant_time_eq("secret", ""));
        assert!(constant_time_eq("", ""));
    }
}
