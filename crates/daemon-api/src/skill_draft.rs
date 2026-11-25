//! "Save as skill" from a chat (`/v1/chats/{id}/skill-draft`, `/v1/skills/draft/*`).
//!
//! `skill-draft` proposes a `SKILL.md` from the newest successful run of a chat. It is built only
//! from tool names and argument names; values are never copied unless they are harmless, so a
//! secret typed in the chat cannot end up in the saved skill. `preview` re-scans an edited draft
//! with the hub's static scanner. `install` writes it into the shared skills folder only when the
//! owner confirms the exact fingerprint of what they reviewed. Local owner only; nothing runs.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, Path, State, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use protocol::{ChatId, RunId};
use serde::{Deserialize, Serialize};
use skills::draft::{self, DraftCall, DraftInput, DraftParam, DraftTool};
use skills::hub::scan_dir;
use skills::owner::PreparedSkill;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{ApiError, ErrorBody};
use crate::skill_hub::{HubFindingDto, HubPreviewDto, label};
use crate::skill_management::{ManagedSkillDto, create_prepared, input, local_owner, owner_error};
use crate::{AppState, PeerIdentity};

/// A tool the draft needs, with what it can do.
#[derive(Debug, Serialize, ToSchema)]
pub struct DraftToolDto {
    pub name: String,
    /// `read`, `write`, `destructive` or `unknown`.
    pub effect: String,
}

/// A value the skill asks for each time it runs.
#[derive(Debug, Serialize, ToSchema)]
pub struct DraftParamDto {
    pub name: String,
    /// A harmless example; absent when the real value was not saved.
    pub example: Option<String>,
    /// True when the value looked private and was left out.
    pub withheld: bool,
}

/// The proposed skill plus its safety scan.
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillDraftDto {
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
    /// Permissions the skill will rely on.
    pub tools: Vec<DraftToolDto>,
    pub parameters: Vec<DraftParamDto>,
    pub steps: Vec<String>,
    /// How many argument values were left out because they looked private.
    pub withheld: usize,
    /// Complete SKILL.md the owner can edit.
    pub skill_md: String,
    /// True when the run had read untrusted or private content.
    pub tainted: bool,
    pub preview: HubPreviewDto,
}

/// An edited draft to scan or install.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftTextInput {
    pub skill_md: String,
}

/// Install request: the reviewed text plus the fingerprint the owner confirmed.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftInstallInput {
    pub skill_md: String,
    /// `fingerprint` of the preview the owner confirmed.
    pub accept: String,
    /// The chat the draft was proposed from; the daemon re-checks its exposure.
    pub chat_id: Uuid,
    /// The owner ticked the box saying they read the warnings. The daemon requires it when the
    /// source run read untrusted or private content or the scan found anything above low risk.
    #[serde(default)]
    pub acknowledged: bool,
}

fn scan_markdown(state: &AppState, text: &str) -> Result<(PreparedSkill, HubPreviewDto), ApiError> {
    let prepared = PreparedSkill::markdown(text).map_err(owner_error)?;
    crate::skill_management::check_secrets(state, &prepared)?;
    let skill = skills::parse_skill(text, skills::Source::User, None)
        .map_err(|_| owner_error(skills::owner::OwnerError::Invalid))?;
    let dir = tempfile::tempdir().map_err(|_| ApiError::internal("skill scan failed"))?;
    std::fs::write(dir.path().join(skills::SKILL_FILE), text)
        .map_err(|_| ApiError::internal("skill scan failed"))?;
    let report = scan_dir(dir.path()).map_err(|_| ApiError::internal("skill scan failed"))?;
    let redact = |t: &str| state.agent().redact_external_text(t.to_owned());
    let dto = HubPreviewDto {
        name: redact(&skill.name),
        description: redact(&skill.description),
        version: redact(&skill.version),
        risk: label(&report.risk()),
        active: report.active,
        files: report.files,
        total_bytes: report.total_bytes,
        fingerprint: report.fingerprint.clone(),
        findings: report
            .findings
            .iter()
            .map(|f| HubFindingDto {
                severity: label(&f.severity),
                kind: label(&f.kind),
                file: f.file.clone(),
                line: f.line,
                message: redact(&f.message),
            })
            .collect(),
    };
    Ok((prepared, dto))
}

fn effect_label(effect: tool_registry::Effect) -> &'static str {
    match effect {
        tool_registry::Effect::Read => "read",
        tool_registry::Effect::Write => "write",
        tool_registry::Effect::Destructive => "destructive",
    }
}

/// String literals a tool schema declares in an `enum`, by argument name.
fn schema_enums(schema: &serde_json::Value) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let Some(props) = schema["properties"].as_object() else {
        return out;
    };
    for (key, prop) in props {
        if let Some(list) = prop["enum"].as_array() {
            let literals: Vec<String> = list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            if literals.len() == list.len() {
                out.insert(key.clone(), literals);
            }
        }
    }
    out
}

/// Successful calls of the run, oldest first.
fn successful_calls(state: &AppState, run: RunId) -> Result<Vec<DraftCall>, ApiError> {
    let registry = state.agent().tool_registry();
    let mut out = Vec::new();
    for step in state.storage().list_steps(run)? {
        if step.name != "tools" || step.status != "succeeded" {
            continue;
        }
        let Some(calls) = step.input["calls"].as_array() else {
            continue;
        };
        let results = step
            .output
            .as_ref()
            .and_then(|o| o["results"].as_array())
            .cloned()
            .unwrap_or_default();
        for call in calls {
            let (Some(id), Some(name)) = (call["id"].as_str(), call["name"].as_str()) else {
                continue;
            };
            let ok = results
                .iter()
                .any(|r| r["id"].as_str() == Some(id) && r["is_error"].as_bool() == Some(false));
            if !ok {
                continue;
            }
            let (effect, enums) = registry
                .get(name)
                .map(|t| {
                    let spec = t.spec();
                    (
                        Some(effect_label(spec.effect).to_owned()),
                        schema_enums(&spec.input_schema),
                    )
                })
                .unwrap_or_default();
            out.push(DraftCall {
                tool: name.to_owned(),
                arguments: match &call["arguments"] {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Null => "{}".to_owned(),
                    other => other.to_string(),
                },
                effect,
                enums,
            });
        }
    }
    Ok(out)
}

/// The newest finished run of a chat and whether it read untrusted or private content.
fn latest_run(state: &AppState, chat: ChatId) -> Result<(storage::Run, bool), ApiError> {
    let run = state
        .storage()
        .latest_succeeded_run_for_chat(state.user_id(), chat)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "skill_draft_no_run",
                "this chat has no finished run to turn into a skill",
            )
        })?;
    let tainted = state
        .storage()
        .list_messages(chat)?
        .iter()
        .any(|m| m.run_id == Some(run.id) && (m.exposure_untrusted || m.exposure_private));
    Ok((run, tainted))
}

fn draft_for_chat(state: &AppState, chat_id: Uuid) -> Result<SkillDraftDto, ApiError> {
    state.agent().refresh_external_redactor();
    let chat = state.chat_of_user(ChatId(chat_id))?;
    let (run, tainted) = latest_run(state, chat.id)?;
    let calls = successful_calls(state, run.id)?;
    if calls.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "skill_draft_no_steps",
            "the last run used no tools, so there is nothing to save as a skill",
        ));
    }
    let redact = |t: &str| state.agent().redact_external_text(t.to_owned());
    let d = draft::build(
        &DraftInput {
            title: chat.title,
            calls,
        },
        &redact,
    );
    let (_, preview) = scan_markdown(state, &d.markdown)?;
    Ok(SkillDraftDto {
        name: d.name,
        description: d.description,
        keywords: d.keywords,
        tools: d
            .tools
            .into_iter()
            .map(|DraftTool { name, effect }| DraftToolDto { name, effect })
            .collect(),
        parameters: d
            .parameters
            .into_iter()
            .map(
                |DraftParam {
                     name,
                     example,
                     withheld,
                 }| DraftParamDto {
                    name,
                    example,
                    withheld,
                },
            )
            .collect(),
        steps: d.steps,
        withheld: d.withheld,
        skill_md: d.markdown,
        tainted,
        preview,
    })
}

/// Propose a skill from the newest successful run of a chat. Read-only.
#[utoipa::path(post, path = "/v1/chats/{id}/skill-draft", tag = "skills", security(("bearer" = [])),
    params(("id" = Uuid, Path, description = "Chat id")),
    responses((status = 200, body = SkillDraftDto), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn draft_from_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<PeerIdentity>>,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<SkillDraftDto>, ApiError> {
    local_owner(peer.as_ref())?;
    state.require_owner(&headers)?;
    let dto = tokio::task::spawn_blocking(move || draft_for_chat(&state, chat_id))
        .await
        .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok(Json(dto))
}

/// Scan an edited draft; nothing is installed.
#[utoipa::path(post, path = "/v1/skills/draft/preview", tag = "skills", security(("bearer" = [])),
    request_body = DraftTextInput,
    responses((status = 200, body = HubPreviewDto), (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn draft_preview(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<DraftTextInput>, JsonRejection>,
) -> Result<Json<HubPreviewDto>, ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let dto = tokio::task::spawn_blocking(move || {
        state.agent().refresh_external_redactor();
        scan_markdown(&state, &body.skill_md).map(|(_, dto)| dto)
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok(Json(dto))
}

/// Install the reviewed draft when its fingerprint is the one the owner confirmed.
#[utoipa::path(post, path = "/v1/skills/draft/install", tag = "skills", security(("bearer" = [])),
    request_body = DraftInstallInput,
    responses((status = 201, body = ManagedSkillDto), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 409, body = ErrorBody),
              (status = 422, body = ErrorBody)))]
pub(crate) async fn draft_install(
    State(state): State<AppState>,
    peer: Option<Extension<PeerIdentity>>,
    json: Result<Json<DraftInstallInput>, JsonRejection>,
) -> Result<(StatusCode, Json<ManagedSkillDto>), ApiError> {
    local_owner(peer.as_ref())?;
    let body = input(json)?;
    let dto = tokio::task::spawn_blocking(move || {
        let _guard = state.0.skill_mutations.lock();
        state.agent().refresh_external_redactor();
        let (prepared, preview) = scan_markdown(&state, &body.skill_md)?;
        if !preview.fingerprint.eq_ignore_ascii_case(&body.accept) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "skill_draft_changed",
                "the skill changed since the preview; review it again",
            ));
        }
        // The dialog asks for a tick, but the daemon does not rely on the dialog.
        let chat = state.chat_of_user(ChatId(body.chat_id))?;
        let (_, tainted) = latest_run(&state, chat.id)?;
        let risky = matches!(preview.risk.as_str(), "medium" | "high");
        if (tainted || risky) && !body.acknowledged {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "skill_draft_ack_required",
                "this skill comes from a run that read outside content or the scan found warnings; confirm you reviewed it",
            ));
        }
        create_prepared(&state, &prepared, "skill.draft_install_requested")
    })
    .await
    .map_err(|_| ApiError::internal("skill worker failed"))??;
    Ok((StatusCode::CREATED, Json(dto)))
}
