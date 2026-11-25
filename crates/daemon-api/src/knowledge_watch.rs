//! Keeps indexed knowledge folders fresh.
//!
//! A slow poll (no OS watcher, so it behaves the same on every platform and
//! never needs extra handles) fingerprints each enabled folder source: names,
//! sizes and modification times, never file contents. A change must stay quiet
//! for one full poll before the folder is re-indexed (debounce), steady churn
//! backs the folder off, and re-indexing is capped to a number of files per
//! minute with a pause between pages so it stays out of the user's way.
//!
//! Safety is inherited, not re-implemented: indexing goes through the same
//! `reindex_limited` as the manual button, i.e. `SelectedSource` (no symlink is
//! ever followed, secret-looking names and credential folders are skipped,
//! size/depth/entry caps, daemon state excluded) and only for sources the
//! owner enabled. Disabling a source stops its watching.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path};

use cap_fs_ext::DirExt;
use std::time::Duration;

use axum::{
    Json,
    extract::{Path as ApiPath, State},
};
use memory_store::knowledge::source_adapters::SelectedSource;
use memory_store::knowledge::{KnowledgeStore, Scope, Source, SourceKind};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

#[cfg(test)]
mod tests;

/// Tunables; tests shrink them.
#[derive(Clone, Copy)]
pub(crate) struct Params {
    pub tick_ms: i64,
    pub files_per_minute: usize,
    pub page_pause: Duration,
    pub backoff_cap_ms: i64,
    pub churn_threshold: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            tick_ms: 20_000,
            files_per_minute: 120,
            page_pause: Duration::from_millis(200),
            backoff_cap_ms: 10 * 60_000,
            churn_threshold: 3,
        }
    }
}

#[derive(Clone, Default)]
struct SourceWatch {
    fingerprint: Option<u64>,
    pending: bool,
    quiet_ticks: u32,
    churn: u32,
    failures: u32,
    next_check_ms: i64,
    backing_off: bool,
    cursor: Option<String>,
    skipped_files: usize,
    last_sync_ms: Option<i64>,
    error: Option<&'static str>,
}

/// Per-daemon watcher state: what each folder looked like and which are paused.
#[derive(Default)]
pub(crate) struct WatchHub {
    sources: parking_lot::Mutex<HashMap<Uuid, SourceWatch>>,
    paused: parking_lot::Mutex<Option<BTreeSet<Uuid>>>,
    budget: parking_lot::Mutex<Budget>,
}

const PAUSED_FILE: &str = "knowledge-watch-paused.json";

impl WatchHub {
    fn paused_set(&self, data_dir: &Path) -> BTreeSet<Uuid> {
        self.paused
            .lock()
            .get_or_insert_with(|| {
                std::fs::read(data_dir.join(PAUSED_FILE))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default()
            })
            .clone()
    }

    fn set_paused(&self, data_dir: &Path, id: Uuid, paused: bool) -> Result<(), ApiError> {
        self.paused_set(data_dir);
        let mut guard = self.paused.lock();
        let mut set = guard.clone().unwrap_or_default();
        if paused {
            set.insert(id);
        } else {
            set.remove(&id);
        }
        // Unique temporary file, persisted before acknowledging the owner's choice.
        let mut tmp = tempfile::NamedTempFile::new_in(data_dir)
            .map_err(|_| ApiError::internal("cannot save automatic update preference"))?;
        serde_json::to_writer(tmp.as_file_mut(), &set)
            .map_err(|_| ApiError::internal("cannot save automatic update preference"))?;
        tmp.persist(data_dir.join(PAUSED_FILE))
            .map_err(|_| ApiError::internal("cannot save automatic update preference"))?;
        *guard = Some(set);
        if !paused {
            // Resuming starts fresh, but cannot reset the daemon's file budget.
            self.sources.lock().remove(&id);
        }
        Ok(())
    }
}

#[derive(Default)]
struct Budget {
    start_ms: Option<i64>,
    used: usize,
}

fn hash_file_state(source: &Source, files: &[String]) -> Result<u64, &'static str> {
    // Every ancestor is opened without following links, including replacements
    // made after SelectedSource enumerated the folder. No ambient path stat.
    let root =
        memory_store::knowledge::source_adapters::plain_directory(Path::new(&source.locator))
            .map_err(|_| "folder_unreadable")?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.locator.hash(&mut hasher);
    for name in files {
        name.hash(&mut hasher);
        let metadata = (|| {
            let mut parent = root.try_clone()?;
            let relative = Path::new(name);
            if let Some(ancestors) = relative.parent() {
                for part in ancestors.components() {
                    let Component::Normal(part) = part else {
                        return Err(std::io::Error::other("invalid source path"));
                    };
                    parent = parent.open_dir_nofollow(part)?;
                }
            }
            parent.symlink_metadata(
                relative
                    .file_name()
                    .ok_or_else(|| std::io::Error::other("invalid source path"))?,
            )
        })();
        match metadata {
            Ok(meta) => {
                meta.len().hash(&mut hasher);
                meta.file_type().is_symlink().hash(&mut hasher);
                if let Ok(modified) = meta.modified() {
                    modified.into_std().hash(&mut hasher);
                }
            }
            Err(_) => 0u8.hash(&mut hasher),
        }
    }
    Ok(hasher.finish())
}

fn open(s: &AppState, source: &Source) -> Result<SelectedSource, &'static str> {
    SelectedSource::open_on_host(Path::new(&source.locator), true, &s.data_dir(), false).map_err(
        |error| {
            if matches!(&error, memory_store::MemoryError::Io(io) if io.kind() == std::io::ErrorKind::NotFound) {
                "folder_missing"
            } else if error.to_string().contains("entry limit") {
                "too_many_files"
            } else {
                "folder_unreadable"
            }
        },
    )
}

fn backoff_ms(base: i64, steps: u32, cap: i64) -> i64 {
    base.saturating_mul(1_i64 << steps.min(20)).min(cap)
}

/// Look at the folder; true when it has settled after a change and should be indexed now.
fn observe(s: &AppState, source: &Source, w: &mut SourceWatch, p: &Params, now: i64) -> bool {
    if now < w.next_check_ms {
        return false;
    }
    w.next_check_ms = now + p.tick_ms;
    w.backing_off = false;
    let selected = match open(s, source) {
        Ok(selected) => selected,
        Err(code) => {
            w.error = Some(code);
            w.failures = w.failures.saturating_add(1);
            w.next_check_ms = now + backoff_ms(p.tick_ms, w.failures, p.backoff_cap_ms);
            w.backing_off = true;
            return false;
        }
    };
    if w.error.is_some_and(|code| code != "some_files_skipped") {
        w.error = None;
    }
    let fingerprint = match hash_file_state(source, selected.files()) {
        Ok(fingerprint) => fingerprint,
        Err(code) => {
            w.error = Some(code);
            w.failures = w.failures.saturating_add(1);
            w.next_check_ms = now + backoff_ms(p.tick_ms, w.failures, p.backoff_cap_ms);
            w.backing_off = true;
            return false;
        }
    };
    match w.fingerprint.replace(fingerprint) {
        // First sight (also after a restart): one catch-up scan, which is cheap
        // when nothing changed because unchanged hashes are skipped.
        None => {
            w.pending = true;
            w.quiet_ticks = 1;
        }
        Some(old) if old != fingerprint => {
            w.pending = true;
            w.quiet_ticks = 0;
            w.churn = w.churn.saturating_add(1);
            // A change can land before the scan cursor: scan again from the top.
            w.cursor = None;
            w.skipped_files = 0;
        }
        Some(_) => {
            w.quiet_ticks = w.quiet_ticks.saturating_add(1);
            w.churn = 0;
        }
    }
    if w.churn >= p.churn_threshold {
        let steps = w.churn - p.churn_threshold + 1;
        w.next_check_ms = now + backoff_ms(p.tick_ms, steps, p.backoff_cap_ms);
        w.backing_off = true;
        return false;
    }
    w.pending && w.quiet_ticks >= 1
}

/// Index one small page, then release both workers so foreground work can run.
fn index(
    s: &AppState,
    owner: Scope,
    id: Uuid,
    w: &mut SourceWatch,
    p: &Params,
    now: i64,
) -> Result<(), ApiError> {
    if s.shutdown_token().is_cancelled()
        || s.0.knowledge_watch.paused_set(&s.data_dir()).contains(&id)
    {
        return Ok(());
    }
    let mut budget = s.0.knowledge_watch.budget.lock();
    let start = *budget.start_ms.get_or_insert(now);
    if now - start >= 60_000 {
        budget.start_ms = Some(now);
        budget.used = 0;
    }
    if budget.used >= p.files_per_minute {
        w.next_check_ms = budget.start_ms.unwrap_or(now) + 60_000;
        return Ok(());
    }
    s.agent().refresh_external_redactor();
    let store = KnowledgeStore::open(s.data_dir(), owner)
        .map_err(|_| ApiError::internal("knowledge store"))?;
    let Some(source) = store
        .source(id)
        .map_err(|_| ApiError::internal("knowledge store"))?
    else {
        return Ok(());
    };
    let page_size = p.files_per_minute.saturating_sub(budget.used).min(8);
    // Reserve attempts before I/O: unreadable and oversized files also cost
    // budget, as does a partially completed page that returns an error.
    budget.used += page_size;
    let done =
        crate::knowledge::reindex_limited(s, owner, &source, w.cursor.as_deref(), page_size)?;
    let attempted = done.indexing.processed + done.indexing.failures.len();
    budget.used -= page_size.saturating_sub(attempted);
    w.skipped_files += done.indexing.failures.len();
    w.cursor = done.indexing.next_cursor;
    if w.cursor.is_some() {
        if budget.used >= p.files_per_minute {
            w.next_check_ms = budget.start_ms.unwrap_or(now) + 60_000;
        }
    } else {
        w.pending = false;
        w.last_sync_ms = Some(now);
        w.failures = 0;
        w.error = (w.skipped_files > 0).then_some("some_files_skipped");
        w.skipped_files = 0;
    }
    Ok(())
}

fn watched_sources(s: &AppState, owner: Scope) -> Vec<Source> {
    let Ok(Some(store)) = KnowledgeStore::open_existing(s.data_dir(), owner) else {
        return Vec::new();
    };
    store
        .list_sources()
        .unwrap_or_default()
        .into_iter()
        .filter(|src| {
            src.kind == SourceKind::Folder && src.enabled && !src.locator.starts_with("imported:")
        })
        .collect()
}

/// One pass over every bot's enabled folder sources.
pub(crate) async fn tick(s: &AppState, p: Params, now: i64) {
    let Ok(bots) = s.storage().list_bots(s.user_id()) else {
        return;
    };
    let mut live = BTreeSet::new();
    for bot in bots {
        let owner = Scope {
            user_id: bot.user_id,
            bot_id: bot.id,
        };
        let sources = {
            let s = s.clone();
            match crate::knowledge::read_blocking(move || Ok(watched_sources(&s, owner))).await {
                Ok(sources) => sources,
                Err(_) => continue,
            }
        };
        for source in sources {
            live.insert(source.id);
            if !s
                .0
                .knowledge_watch
                .paused_set(&s.data_dir())
                .contains(&source.id)
            {
                step(s, owner, source, p, now).await;
                tokio::time::sleep(p.page_pause).await;
            }
        }
    }
    s.0.knowledge_watch
        .sources
        .lock()
        .retain(|id, _| live.contains(id));
}

async fn step(s: &AppState, owner: Scope, source: Source, p: Params, now: i64) {
    let hub = &s.0.knowledge_watch;
    let mut w = hub
        .sources
        .lock()
        .get(&source.id)
        .cloned()
        .unwrap_or_default();
    let id = source.id;
    // Reads only names and metadata; bounded by the entry cap.
    let observed = {
        let s2 = s.clone();
        crate::knowledge::read_blocking(move || {
            let settled = observe(&s2, &source, &mut w, &p, now);
            Ok((w, settled))
        })
        .await
    };
    let Ok((w, settled)) = observed else {
        return;
    };
    if !settled {
        hub.sources.lock().insert(id, w);
        return;
    }
    // An exhausted background budget must not cancel foreground embedding work.
    let deferred_until = {
        let budget = hub.budget.lock();
        budget
            .start_ms
            .filter(|start| now < start + 60_000 && budget.used >= p.files_per_minute)
            .map(|start| start + 60_000)
    };
    if let Some(until) = deferred_until {
        let mut w = w;
        w.next_check_ms = until;
        hub.sources.lock().insert(id, w);
        return;
    }
    // Same exclusion as the manual button; if the owner is changing this
    // bot's knowledge right now, simply try again next poll.
    let Ok(authority) = crate::knowledge::try_authority(s, owner.bot_id) else {
        hub.sources.lock().insert(id, w);
        return;
    };
    let Ok(permit) = crate::knowledge::worker_permit(s.data_dir()) else {
        hub.sources.lock().insert(id, w);
        return;
    };
    if crate::knowledge_runtime::quiesce_bot(s, owner.bot_id)
        .await
        .is_err()
    {
        hub.sources.lock().insert(id, w);
        return;
    }
    let s2 = s.clone();
    let mut backup = w.clone();
    let result = crate::knowledge::blocking_with_permit(permit, move || {
        let _authority = authority;
        let mut w = w;
        let ok = index(&s2, owner, id, &mut w, &p, now).is_ok();
        Ok((w, ok))
    })
    .await;
    if let Ok((mut done, ok)) = result {
        if !ok {
            done.error = Some("sync_failed");
            done.failures = done.failures.saturating_add(1);
            done.next_check_ms = now + backoff_ms(p.tick_ms, done.failures, p.backoff_cap_ms);
            done.backing_off = true;
        }
        backup = done;
    }
    hub.sources.lock().insert(id, backup);
}

/// Start the background poll. It ends with the daemon.
pub(crate) fn spawn(state: &AppState) {
    let state = state.clone();
    let stop = state.shutdown_token();
    tokio::spawn(async move {
        let p = Params::default();
        let mut timer = tokio::time::interval(Duration::from_millis(p.tick_ms.unsigned_abs()));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        timer.tick().await;
        loop {
            tokio::select! {
                () = stop.cancelled() => break,
                _ = timer.tick() => {}
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            tick(&state, p, now).await;
        }
    });
}

/// What the Knowledge view shows for a watched folder.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeWatchState {
    /// Up to date; checking for changes.
    Watching,
    /// A change was seen; re-indexing shortly.
    Waiting,
    /// Files change constantly or an error occurred; checking less often.
    BackingOff,
    /// The owner paused automatic updates.
    Paused,
}

/// A problem in plain terms; the UI maps `code` to words and a fix.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KnowledgeWatchError {
    /// `folder_missing`, `folder_unreadable`, `too_many_files`, `some_files_skipped`, `sync_failed`.
    pub code: String,
}

/// Automatic-update status of one folder source.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeWatchStatus {
    pub source_id: Uuid,
    pub state: KnowledgeWatchState,
    pub indexed_files: usize,
    pub last_sync_ms: Option<i64>,
    pub error: Option<KnowledgeWatchError>,
}

/// Pause or resume automatic updates.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeWatchPause {
    pub paused: bool,
}

fn status_of(
    s: &AppState,
    store: &KnowledgeStore,
    source: &Source,
) -> Result<KnowledgeWatchStatus, ApiError> {
    let hub = &s.0.knowledge_watch;
    let w = hub
        .sources
        .lock()
        .get(&source.id)
        .cloned()
        .unwrap_or_default();
    let paused = hub.paused_set(&s.data_dir()).contains(&source.id);
    let state = if paused {
        KnowledgeWatchState::Paused
    } else if w.backing_off {
        KnowledgeWatchState::BackingOff
    } else if w.pending {
        KnowledgeWatchState::Waiting
    } else {
        KnowledgeWatchState::Watching
    };
    let indexed_files = store
        .list_documents(source.id)
        .map_err(|_| ApiError::internal("knowledge store"))?
        .len();
    Ok(KnowledgeWatchStatus {
        source_id: source.id,
        state,
        indexed_files,
        last_sync_ms: w.last_sync_ms,
        error: w.error.map(|code| KnowledgeWatchError {
            code: code.to_owned(),
        }),
    })
}

fn folder_statuses(s: &AppState, owner: Scope) -> Result<Vec<KnowledgeWatchStatus>, ApiError> {
    let Some(store) = KnowledgeStore::open_existing(s.data_dir(), owner)
        .map_err(|_| ApiError::internal("knowledge store"))?
    else {
        return Ok(Vec::new());
    };
    store
        .list_sources()
        .map_err(|_| ApiError::internal("knowledge store"))?
        .iter()
        .filter(|src| {
            src.kind == SourceKind::Folder && src.enabled && !src.locator.starts_with("imported:")
        })
        .map(|src| status_of(s, &store, src))
        .collect()
}

/// Automatic-update status of every enabled folder source of a bot.
#[utoipa::path(get, path = "/v1/bots/{bot}/knowledge/watch", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path)), responses((status = 200, body = Vec<KnowledgeWatchStatus>), (status = 404, body = ErrorBody)))]
pub(crate) async fn list_watch(
    State(s): State<AppState>,
    ApiPath(bot): ApiPath<Uuid>,
) -> Result<Json<Vec<KnowledgeWatchStatus>>, ApiError> {
    let owner = crate::knowledge::scope(&s, bot)?;
    crate::knowledge::read_blocking(move || folder_statuses(&s, owner).map(Json)).await
}

/// Pause or resume automatic updates of one folder. Owner only.
#[utoipa::path(put, path = "/v1/bots/{bot}/knowledge/sources/{source}/watch", tag = "knowledge", security(("bearer" = [])), params(("bot" = Uuid, Path), ("source" = Uuid, Path)), request_body = KnowledgeWatchPause, responses((status = 200, body = KnowledgeWatchStatus), (status = 404, body = ErrorBody)))]
pub(crate) async fn set_watch_paused(
    State(s): State<AppState>,
    ApiPath((bot, id)): ApiPath<(Uuid, Uuid)>,
    ApiJson(input): ApiJson<KnowledgeWatchPause>,
) -> Result<Json<KnowledgeWatchStatus>, ApiError> {
    let owner = crate::knowledge::scope(&s, bot)?;
    let authority = crate::knowledge::try_authority(&s, owner.bot_id)?;
    crate::knowledge::read_blocking(move || {
        let _authority = authority;
        let store = KnowledgeStore::open_existing(s.data_dir(), owner)
            .map_err(|_| ApiError::internal("knowledge store"))?
            .ok_or_else(|| ApiError::not_found("knowledge source"))?;
        let source = store
            .source(id)
            .map_err(|_| ApiError::internal("knowledge store"))?
            .filter(|src| src.kind == SourceKind::Folder)
            .ok_or_else(|| ApiError::not_found("knowledge folder"))?;
        s.0.knowledge_watch
            .set_paused(&s.data_dir(), id, input.paused)?;
        status_of(&s, &store, &source).map(Json)
    })
    .await
}
