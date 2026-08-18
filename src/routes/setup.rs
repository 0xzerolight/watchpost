//! First-run setup: the page that takes a GitHub token on an install that has
//! never had one, and the gate that makes it unmissable.
//!
//! The gate is a redirect rather than an error because the alternative is
//! worse than it looks. A fresh container with no token can serve a dashboard,
//! a repo list and a settings page — all of them empty, none of them saying
//! why. One destination, one instruction.
//!
//! Two paths stay open on an unconfigured install: `/health`, because the
//! container healthcheck and the installer both poll it before a token can
//! exist, and `/assets`, because the setup page is styled by the same CSS as
//! everything else.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, html};
use tracing::warn;

use crate::collector;
use crate::config::TokenSource;
use crate::csrf::CsrfToken;
use crate::db::queries;
use crate::errors::GhError;
use crate::gh_client::GhClient;
use crate::routes::html::{NavItem, Notice, base, notice, page_header};
use crate::state::AppState;

/// The permissions the token needs, in the order the README and the doctor
/// report list them. They live next to the field because this is where the
/// token is being made, and sending the reader off to find out what to tick is
/// the friction this page exists to remove.
const PERMISSIONS: &[(&str, &str)] = &[
    ("Metadata: read", "the repository list and the basic counts"),
    (
        "Administration: read",
        "traffic views, clones, referrers and popular paths",
    ),
    ("Contents: read", "releases and asset download counts"),
    ("Pull requests: read", "the open pull request count"),
];

/// What a 401 is told to the browser. The two causes worth naming are a
/// half-copied paste and an expired token; everything else about a 401 is
/// GitHub's to explain.
const TOKEN_REJECTED: &str = "GitHub rejected that token. Check it was copied whole, and that \
                              it has not expired.";

/// Paths that answer normally on an install with no token.
fn always_open(path: &str) -> bool {
    path == "/health" || path == "/setup" || path.starts_with("/assets/")
}

/// Send an unconfigured install to the setup page, and a configured one away
/// from it.
///
/// Layered *inside* CSRF, so a POST here is validated before it arrives and a
/// page rendered here still finds a token in the request extensions.
pub async fn setup_gate(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let configured = state.gh().is_some();
    let path = req.uri().path();

    if !configured && !always_open(path) {
        return Redirect::to("/setup").into_response();
    }
    // A configured install has no wizard to show. A bookmark or a back button
    // that lands here goes to the dashboard rather than to a form that would
    // silently rotate a working token.
    if configured && path == "/setup" && req.method() == Method::GET {
        return Redirect::to("/").into_response();
    }
    next.run(req).await
}

/// GET /setup — the form, plus what the token needs to be able to do.
pub async fn setup_page(csrf: CsrfToken) -> Markup {
    render(&csrf, None)
}

/// POST /setup — validate, save, and hand the browser to the dashboard.
pub async fn setup_submit(
    State(state): State<Arc<AppState>>,
    csrf: CsrfToken,
    body: String,
) -> Response {
    let raw = form_field(&body, "token").unwrap_or_default();
    match apply_token(&state, &raw).await {
        Ok(()) => {
            // The first cycle starts here rather than on the next cron tick: a
            // user who just pasted a token is looking at an empty dashboard,
            // and an hour of nothing reads as a broken install.
            let spawned = Arc::clone(&state);
            tokio::spawn(async move {
                collector::try_run_cycle(spawned).await;
            });

            // The whole page changes, so htmx is told to navigate rather than
            // to swap a fragment into the form it just submitted.
            let mut resp = StatusCode::OK.into_response();
            resp.headers_mut()
                .insert("hx-redirect", HeaderValue::from_static("/"));
            resp
        }
        // 200, not 4xx: htmx only swaps a 4xx for the 422 the event forms
        // answer with, so an error status here would discard the re-rendered
        // form and leave the page unchanged. See `assets/htmx-config.js`.
        Err(msg) => render(&csrf, Some(msg)).into_response(),
    }
}

/// Validate a token against GitHub and, if it holds, save it and start using
/// it. `Err` carries copy that is safe to render.
///
/// `GET /rate_limit` is the probe. It is the one endpoint that proves the
/// token authenticates without spending any of the budget it reports on, and
/// it answers for a token with no repository permissions at all — which is the
/// right outcome, since a missing permission costs one endpoint rather than
/// the install.
pub async fn apply_token(state: &AppState, raw: &str) -> Result<(), String> {
    let token = raw.trim();
    if token.is_empty() {
        return Err("Paste a token to continue.".to_owned());
    }

    let gh = GhClient::new(token, state.cfg.github_api_base.clone())
        .map_err(|_| "That token cannot be sent in an HTTP header.".to_owned())?;

    match gh.rate_limit().await {
        Ok(_) => {}
        Err(GhError::Status { status: 401, .. }) => {
            return Err(TOKEN_REJECTED.to_owned());
        }
        Err(e) => {
            // The category reaches the browser; the detail stays in the log.
            warn!(error = %e, "setup token check failed");
            return Err(e.user_message());
        }
    }

    let owned = token.to_owned();
    state
        .db
        .call(move |c| queries::set_setting(c, queries::GITHUB_TOKEN_KEY, &owned))
        .await
        .map_err(|e| {
            warn!(error = %e, "could not save the token");
            "Could not save the token — see the container log.".to_owned()
        })?;

    state.install_token(gh, token, TokenSource::Database);
    Ok(())
}

/// One field out of an urlencoded body.
///
/// Hand-parsed for the same reason [`crate::routes::settings`] parses the
/// picker's checkboxes by hand: the bodies this app posts are small and
/// `serde_urlencoded` would be a dependency on a shape that never varies.
pub fn form_field(body: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(body.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// The page. `error` is the notice a rejected submission carries back.
fn render(csrf: &CsrfToken, error: Option<String>) -> Markup {
    base(
        "Setup",
        NavItem::None,
        csrf,
        html! {
            (page_header("Set up watchpost", None, None))
            section {
                p {
                    "watchpost reads your repositories through the GitHub API, so it needs a \
                     personal access token. A "
                    a href="https://github.com/settings/personal-access-tokens/new"
                        target="_blank" rel="noopener" { "fine-grained token" }
                    " is the better choice; under "
                    strong { "Repository permissions" }
                    " grant:"
                }
                ul {
                    @for (name, why) in PERMISSIONS {
                        li { strong { (name) } " — " (why) }
                    }
                }
                p {
                    "A classic token with the "
                    code { "repo" }
                    " scope also works. A missing permission costs that one part of a \
                     collection pass rather than the pass, so you can start with less and \
                     come back to it."
                }
                (token_form(error))
            }
        },
    )
}

/// The field itself, swapped in place when a submission comes back rejected.
fn token_form(error: Option<String>) -> Markup {
    html! {
        form id="setup-form"
            hx-post="/setup"
            hx-target="this"
            hx-swap="outerHTML"
            hx-disabled-elt="find button[type=submit]" {
            @if let Some(text) = error {
                (notice(Notice::Error, html! { (text) }))
            }
            label for="setup-token" { "Personal access token" }
            // `type=password` so a screen-shared or shoulder-surfed setup does
            // not put the token on display; autocomplete is off because a
            // browser password manager offering to save it would be storing a
            // credential for github.com against this host.
            input type="password"
                id="setup-token"
                name="token"
                autocomplete="off"
                spellcheck="false"
                placeholder="github_pat_… or ghp_…"
                required;
            div class="wp-actions" {
                button type="submit" { "Save token" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_open_covers_the_paths_a_setup_install_still_needs() {
        assert!(always_open("/health"));
        assert!(always_open("/setup"));
        assert!(always_open("/assets/app.css"));
        assert!(!always_open("/"));
        assert!(!always_open("/settings"));
        // Prefix collisions are not assets: only the directory is open.
        assert!(!always_open("/assets"));
        assert!(!always_open("/healthz"));
    }

    #[test]
    fn form_field_reads_the_named_key_only() {
        assert_eq!(
            form_field("token=ghp_x&other=y", "token").as_deref(),
            Some("ghp_x")
        );
        assert_eq!(form_field("other=y", "token"), None);
        // Percent-encoding is decoded, so a pasted token with padding arrives
        // as the user sees it and `apply_token` can trim it.
        assert_eq!(
            form_field("token=%20ghp_x", "token").as_deref(),
            Some(" ghp_x")
        );
    }
}
