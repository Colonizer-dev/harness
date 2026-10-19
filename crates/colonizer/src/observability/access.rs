//! The one rule the install-wide observability surfaces share: who may read them.
//!
//! `GET /metrics` and `GET /api/observability/status` both report the whole install — every
//! colony's counts, the exporter's own health, where every setting came from — so both are read by
//! the owner or by a token that is unbounded by construction: a token limited to some orgs or
//! repos would reach a page whose series and errors are exactly the ones it may not know, and a
//! filtered page would hide the very thing an operator scrapes for. `host_guard` has already let a
//! `read`-or-better token past the feature's `token_scope` (`Need::Bare(Scope::Read)` admits org-
//! and repo-limited tokens too), so the finer refusals live here, beside the handlers, in the
//! words each route's caller can act on.

use crate::api_tokens::{Scope, ScopedToken};
use crate::{AppError, client_error};
use axum::http::StatusCode;

/// Whether `token` may read an install-wide surface: no `fleet` scope, no org limit, no repo
/// limit. `host_guard`'s allowlist has already checked the scope reaches `read`; the `fleet` arm
/// is kept beside the limits anyway, exactly as the route table documents it, so the rule cannot
/// drift out from under the table by a future change to what `Need::Bare` admits.
fn unbounded(token: &ScopedToken) -> bool {
    token.scope != Scope::Fleet && token.orgs.is_empty() && token.repos.is_empty()
}

/// The refusal both install-wide surfaces answer with, or `None` when the caller may read on:
/// 403 for a paired phone — it authenticates as the owner for the cockpit, but the install's own
/// numbers are the operator's, and a phone is a revocable credential that lives on a device — and
/// 403 for a token with `fleet` scope or any org/repo limit. `what` names the route and `instead`
/// the way its rightful caller reaches it, so each surface refuses in its own words.
pub(super) fn install_wide_denial(token: Option<&ScopedToken>, phone: bool, what: &str, instead: &str) -> Option<AppError> {
    if phone {
        return Some(client_error(
            StatusCode::FORBIDDEN,
            &format!("{what} is the install's own state, and a phone is not the owner; {instead}"),
        ));
    }
    if let Some(token) = token
        && !unbounded(token)
    {
        return Some(client_error(
            StatusCode::FORBIDDEN,
            &format!(
                "this API token is limited to some organisations or repositories, and {what} is install-wide; \
                 use a token with no limits"
            ),
        ));
    }
    None
}
