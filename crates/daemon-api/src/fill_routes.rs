//! `/v1/fill-secrets*` and `/v1/fill-grants*`: the local owner's management of
//! the secrets the fill tools may type (see [`secret_store::FillSecrets`]).
//!
//! * Local owner only. The peer router refuses both prefixes to every paired
//!   device role for every method, and each handler refuses a non-local actor
//!   again (defence in depth).
//! * A value is write-only: no route returns it, and the list shows names and
//!   allow-lists only.
//! * An attributed audit record is written BEFORE each mutation (no record, no
//!   change). Records carry names, origins and ids, never a value.
//! * A bot, tool, MCP server or skill has no API token, so none of them can
//!   create a secret, widen an allow-list or add a grant.

use axum::Json;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use protocol::BotId;
use secret_store::{
    FillError, FillGrant, FillSecretInfo, FillSecretUpdate, FillSecrets, VaultKind,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::live_view::Actor;
use crate::mcp_routes::MAX_BOT_ID_LEN;
use crate::peer::PEER_DENIED_FILL_PREFIXES;
use crate::routes::ApiJson;

/// Category of a Vault entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaultKindDto {
    /// Website or application login.
    Login,
    /// Other secret (API key, PIN, card field, note secret).
    Other,
}

impl From<VaultKind> for VaultKindDto {
    fn from(k: VaultKind) -> Self {
        match k {
            VaultKind::Login => Self::Login,
            VaultKind::Other => Self::Other,
        }
    }
}

impl From<VaultKindDto> for VaultKind {
    fn from(k: VaultKindDto) -> Self {
        match k {
            VaultKindDto::Login => Self::Login,
            VaultKindDto::Other => Self::Other,
        }
    }
}

/// One fill secret as the owner sees it. Never the value.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FillSecretDto {
    /// Name the assistant writes in `secret` (for example `login/github`).
    pub name: String,
    /// Category (`login` or `other`).
    pub kind: VaultKindDto,
    /// Human-friendly site or service label.
    pub site_name: Option<String>,
    /// Non-secret login identifier (e-mail or username).
    pub login_id: Option<String>,
    /// Owner-only note.
    pub note: Option<String>,
    /// Exact web origins the secret may be typed on.
    pub origins: Vec<String>,
    /// Verified desktop program identities the secret may be typed into.
    pub programs: Vec<String>,
    /// Bot ids permitted to use this secret (empty = no bot).
    pub bot_ids: Vec<String>,
    /// Whether the bot must ask the owner before typing this secret.
    pub ask_before_use: bool,
    /// Whether a value is stored.
    pub has_value: bool,
    /// Last change, milliseconds since the epoch.
    pub updated_at_ms: u64,
}

impl From<FillSecretInfo> for FillSecretDto {
    fn from(i: FillSecretInfo) -> Self {
        Self {
            name: i.name,
            kind: i.kind.into(),
            site_name: i.site_name,
            login_id: i.login_id,
            note: i.note,
            origins: i.origins,
            programs: i.programs,
            bot_ids: i.bot_ids,
            ask_before_use: i.ask_before_use,
            has_value: i.has_value,
            updated_at_ms: i.updated_at_ms,
        }
    }
}

/// Body of `POST /v1/fill-secrets`.
#[derive(Deserialize, ToSchema)]
pub struct CreateFillSecret {
    /// Name, letters/digits/`.`/`_`/`-` with `/` separators, at most 64 bytes.
    pub name: String,
    /// The value. Write-only: no route returns it except an explicit confirmed owner reveal.
    pub value: String,
    /// Category (`login` or `other`).
    #[serde(default)]
    pub kind: Option<VaultKindDto>,
    /// Human-friendly site or service label.
    #[serde(default)]
    pub site_name: Option<String>,
    /// Non-secret login identifier (e-mail or username).
    #[serde(default)]
    pub login_id: Option<String>,
    /// Owner-only note.
    #[serde(default)]
    pub note: Option<String>,
    /// Exact origins like `https://github.com` (scheme, host, port; no wildcard).
    #[serde(default)]
    pub origins: Vec<String>,
    /// Verified desktop program identities.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Bot ids permitted to use this secret (defaults to none).
    #[serde(default)]
    pub bot_ids: Vec<String>,
    /// Whether the bot must ask the owner before typing this secret (default true).
    #[serde(default)]
    pub ask_before_use: Option<bool>,
}

impl std::fmt::Debug for CreateFillSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateFillSecret")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Body of `PUT /v1/fill-secrets/{name}`. Every field is optional; a value,
/// when given, replaces the stored one.
#[derive(Deserialize, ToSchema)]
pub struct UpdateFillSecret {
    /// New value (write-only).
    pub value: Option<String>,
    /// Replaces category.
    pub kind: Option<VaultKindDto>,
    /// Replaces site name (empty string clears it).
    pub site_name: Option<String>,
    /// Replaces login identifier (empty string clears it).
    pub login_id: Option<String>,
    /// Replaces owner note (empty string clears it).
    pub note: Option<String>,
    /// Replaces the allowed origins.
    pub origins: Option<Vec<String>>,
    /// Replaces the allowed programs.
    pub programs: Option<Vec<String>>,
    /// Replaces the allowed bot ids.
    pub bot_ids: Option<Vec<String>>,
    /// Replaces whether the bot asks before typing.
    pub ask_before_use: Option<bool>,
}

impl std::fmt::Debug for UpdateFillSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UpdateFillSecret(<redacted>)")
    }
}

/// Body of `POST /v1/fill-secrets/reveal`.
#[derive(Deserialize, ToSchema)]
pub struct RevealFillSecret {
    /// Secret name to reveal.
    pub name: String,
    /// Must be `true` to confirm the owner intentionally reveals the value on screen.
    pub confirm: bool,
}

impl std::fmt::Debug for RevealFillSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevealFillSecret")
            .field("name", &self.name)
            .field("confirm", &self.confirm)
            .finish()
    }
}

/// Response of `POST /v1/fill-secrets/reveal` (owner-only, `Cache-Control: no-store`).
#[derive(Serialize, ToSchema)]
pub struct RevealFillSecretResponse {
    /// Secret name.
    pub name: String,
    /// Secret value. Never logged or cached.
    pub value: String,
}

impl std::fmt::Debug for RevealFillSecretResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevealFillSecretResponse")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A standing grant: this secret on this exact origin without asking.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FillGrantDto {
    /// Id used to revoke it.
    pub id: String,
    /// The secret.
    pub secret: String,
    /// The exact origin.
    pub origin: String,
    /// Creation time, milliseconds since the epoch.
    pub created_at_ms: u64,
}

impl From<FillGrant> for FillGrantDto {
    fn from(g: FillGrant) -> Self {
        Self {
            id: g.id,
            secret: g.secret,
            origin: g.origin,
            created_at_ms: g.created_at_ms,
        }
    }
}

/// Body of `POST /v1/fill-grants`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFillGrant {
    /// The secret.
    pub secret: String,
    /// The exact origin; it must already be on the secret's allowed list.
    pub origin: String,
}

pub(crate) fn map(e: &FillError) -> ApiError {
    match e {
        FillError::InvalidName(_)
        | FillError::Reserved(_)
        | FillError::InvalidEntry(_)
        | FillError::Invalid(_) => ApiError::bad_request(e.to_string()),
        FillError::Unknown(_) => ApiError::not_found(e.to_string()),
        FillError::NotAllowed(_) => ApiError::forbidden(e.to_string()),
        FillError::Exists(_) | FillError::MissingValue(_) => ApiError::conflict(e.to_string()),
        FillError::Unavailable(_) => ApiError::unavailable(e.to_string()),
    }
}

/// The service, for the local owner only.
pub(crate) fn service(
    s: &AppState,
    actor: &Actor,
) -> Result<std::sync::Arc<FillSecrets>, ApiError> {
    if !actor.local {
        return Err(ApiError::forbidden(
            "fill secrets are managed by the local owner from this computer",
        ));
    }
    s.fill_secrets()
        .ok_or_else(|| ApiError::unavailable("this daemon has no fill secrets"))
}

/// Audit record before a mutation. No record, no change.
pub(crate) fn audit(
    s: &AppState,
    actor: &Actor,
    action: &str,
    meta: &serde_json::Value,
) -> Result<(), ApiError> {
    s.storage()
        .append_audit(s.user_id(), None, &actor.key, action, meta)
        .map(|_| ())
        .map_err(ApiError::internal)
}

/// Run blocking store / file work off the async threads.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, FillError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| map(&e))
}

/// List the fill secrets: names and allow-lists, never values.
#[utoipa::path(get, path = "/v1/fill-secrets", tag = "fill-secrets", security(("bearer" = [])),
    responses((status = 200, body = Vec<FillSecretDto>), (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 503, body = ErrorBody, description = "no fill secrets in this daemon")))]
pub(crate) async fn list_fill_secrets(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Vec<FillSecretDto>>, ApiError> {
    let svc = service(&s, &actor)?;
    let list = blocking(move || svc.list()).await?;
    Ok(Json(list.into_iter().map(Into::into).collect()))
}

pub(crate) fn canonical_bots(s: &AppState, raw: &[String]) -> Result<Vec<String>, ApiError> {
    let mut out = Vec::with_capacity(raw.len());
    for id_raw in raw {
        let trimmed = id_raw.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_BOT_ID_LEN {
            return Err(ApiError::bad_request("malformed bot id"));
        }
        let id = trimmed
            .parse()
            .map_err(|_| ApiError::bad_request("malformed bot id"))?;
        out.push(s.bot_of_user(BotId(id))?.id.to_string());
    }
    Ok(out)
}

/// Create a fill secret with its allow-lists and bot scope. The value is write-only.
#[utoipa::path(post, path = "/v1/fill-secrets", tag = "fill-secrets", security(("bearer" = [])),
    request_body = CreateFillSecret,
    responses((status = 201, body = FillSecretDto), (status = 400, body = ErrorBody, description = "bad name, value or allow-list entry"),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody, description = "unknown bot id"),
              (status = 409, body = ErrorBody, description = "the name exists; use PUT")))]
pub(crate) async fn create_fill_secret(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    ApiJson(input): ApiJson<CreateFillSecret>,
) -> Result<(StatusCode, Json<FillSecretDto>), ApiError> {
    let svc = service(&s, &actor)?;
    let bot_ids = canonical_bots(&s, &input.bot_ids)?;
    let existing = {
        let svc = svc.clone();
        blocking(move || svc.list()).await?
    };
    if existing.iter().any(|i| i.name == input.name) {
        return Err(ApiError::conflict(
            "a fill secret with this name exists; update it instead",
        ));
    }
    audit(
        &s,
        &actor,
        "fill_secret.create",
        &json!({
            "name": input.name,
            "kind": input.kind,
            "origins": input.origins,
            "programs": input.programs,
            "bot_ids": bot_ids,
            "ask_before_use": input.ask_before_use.unwrap_or(true),
        }),
    )?;
    let info = blocking(move || {
        svc.upsert_with(
            &input.name,
            FillSecretUpdate {
                value: Some(&input.value),
                origins: Some(&input.origins),
                programs: Some(&input.programs),
                bot_ids: Some(&bot_ids),
                kind: input.kind.map(Into::into),
                site_name: input.site_name.as_deref(),
                login_id: input.login_id.as_deref(),
                note: input.note.as_deref(),
                ask_before_use: input.ask_before_use,
            },
        )
    })
    .await?;
    Ok((StatusCode::CREATED, Json(info.into())))
}

/// Change a fill secret's value, Vault metadata, bot scope, or allow-lists.
#[utoipa::path(put, path = "/v1/fill-secrets/{name}", tag = "fill-secrets", security(("bearer" = [])),
    params(("name" = String, Path, description = "Secret name, may contain `/`")),
    request_body = UpdateFillSecret,
    responses((status = 200, body = FillSecretDto), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn update_fill_secret(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(name): Path<String>,
    ApiJson(input): ApiJson<UpdateFillSecret>,
) -> Result<Json<FillSecretDto>, ApiError> {
    let svc = service(&s, &actor)?;
    if !svc.exists(&name) {
        return Err(ApiError::not_found(format!("fill secret {name}")));
    }
    let bot_ids = input
        .bot_ids
        .as_deref()
        .map(|ids| canonical_bots(&s, ids))
        .transpose()?;
    audit(
        &s,
        &actor,
        "fill_secret.update",
        &json!({
            "name": name,
            "value_changed": input.value.is_some(),
            "origins": input.origins,
            "programs": input.programs,
            "bot_ids": bot_ids,
            "ask_before_use": input.ask_before_use,
        }),
    )?;
    let info = blocking(move || {
        svc.upsert_with(
            &name,
            FillSecretUpdate {
                value: input.value.as_deref(),
                origins: input.origins.as_deref(),
                programs: input.programs.as_deref(),
                bot_ids: bot_ids.as_deref(),
                kind: input.kind.map(Into::into),
                site_name: input.site_name.as_deref(),
                login_id: input.login_id.as_deref(),
                note: input.note.as_deref(),
                ask_before_use: input.ask_before_use,
            },
        )
    })
    .await?;
    Ok(Json(info.into()))
}

/// Reveal one fill secret value to the local owner after explicit confirmation.
/// Audited before reading; returned with `Cache-Control: no-store`.
#[utoipa::path(post, path = "/v1/fill-secrets/reveal", tag = "fill-secrets", security(("bearer" = [])),
    request_body = RevealFillSecret,
    responses((status = 200, body = RevealFillSecretResponse),
              (status = 400, body = ErrorBody, description = "confirmation required"),
              (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn reveal_fill_secret(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    ApiJson(input): ApiJson<RevealFillSecret>,
) -> Result<(HeaderMap, Json<RevealFillSecretResponse>), ApiError> {
    let svc = service(&s, &actor)?;
    if !input.confirm {
        return Err(ApiError::bad_request(
            "owner confirmation is required to reveal a secret",
        ));
    }
    if !svc.exists(&input.name) {
        return Err(ApiError::not_found(format!("fill secret {}", input.name)));
    }
    audit(
        &s,
        &actor,
        "fill_secret.reveal",
        &json!({"name": input.name}),
    )?;
    let name = input.name.clone();
    let value = blocking(move || svc.resolve_value_for_owner(&name)).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((
        headers,
        Json(RevealFillSecretResponse {
            name: input.name,
            value: value.expose().to_owned(),
        }),
    ))
}

/// Delete a fill secret, its value and its standing grants.
#[utoipa::path(delete, path = "/v1/fill-secrets/{name}", tag = "fill-secrets", security(("bearer" = [])),
    params(("name" = String, Path, description = "Secret name, may contain `/`")),
    responses((status = 204), (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn delete_fill_secret(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let svc = service(&s, &actor)?;
    if !svc.exists(&name) {
        return Err(ApiError::not_found(format!("fill secret {name}")));
    }
    audit(&s, &actor, "fill_secret.delete", &json!({"name": name}))?;
    blocking(move || svc.delete(&name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// List the standing grants.
#[utoipa::path(get, path = "/v1/fill-grants", tag = "fill-secrets", security(("bearer" = [])),
    responses((status = 200, body = Vec<FillGrantDto>), (status = 403, body = ErrorBody, description = "not the local owner")))]
pub(crate) async fn list_fill_grants(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Vec<FillGrantDto>>, ApiError> {
    let svc = service(&s, &actor)?;
    Ok(Json(svc.grants().into_iter().map(Into::into).collect()))
}

/// Create a standing grant: "this secret on this exact origin without asking".
#[utoipa::path(post, path = "/v1/fill-grants", tag = "fill-secrets", security(("bearer" = [])),
    request_body = CreateFillGrant,
    responses((status = 201, body = FillGrantDto), (status = 400, body = ErrorBody, description = "the origin is not on the secret's allowed list"),
              (status = 403, body = ErrorBody, description = "not the local owner"), (status = 404, body = ErrorBody)))]
pub(crate) async fn create_fill_grant(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    ApiJson(input): ApiJson<CreateFillGrant>,
) -> Result<(StatusCode, Json<FillGrantDto>), ApiError> {
    let svc = service(&s, &actor)?;
    if !svc.exists(&input.secret) {
        return Err(ApiError::not_found(format!("fill secret {}", input.secret)));
    }
    audit(
        &s,
        &actor,
        "fill_grant.create",
        &json!({"secret": input.secret, "origin": input.origin}),
    )?;
    let grant = blocking(move || svc.grant_create(&input.secret, &input.origin)).await?;
    Ok((StatusCode::CREATED, Json(grant.into())))
}

/// Revoke a standing grant.
#[utoipa::path(delete, path = "/v1/fill-grants/{id}", tag = "fill-secrets", security(("bearer" = [])),
    params(("id" = String, Path, description = "Grant id")),
    responses((status = 204), (status = 403, body = ErrorBody, description = "not the local owner"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn revoke_fill_grant(
    State(s): State<AppState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let svc = service(&s, &actor)?;
    if !svc.grants().iter().any(|g| g.id == id) {
        return Err(ApiError::not_found(format!("fill grant {id}")));
    }
    audit(&s, &actor, "fill_grant.revoke", &json!({"id": id}))?;
    blocking(move || svc.grant_revoke(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Whether `check_path` (decoded, slash-normalised) is under a fill prefix.
pub(crate) fn is_fill_path(check_path: &str) -> bool {
    PEER_DENIED_FILL_PREFIXES.iter().any(|p| {
        check_path
            .strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}
