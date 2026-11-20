//! Create-only shared Markdown notes. A prepared write freezes exact bytes,
//! source revisions and an open destination capability. It confers no approval;
//! callers must hold it until their normal user-approval gate authorizes it.

use std::io::Write;
use std::path::Path;

use cap_fs_ext::{DirExt, MetadataExt};
use cap_std::fs::{Dir, File};
#[cfg(windows)]
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::source_adapters::trusted_directory;
use super::{KnowledgeStore, Scope};
use crate::{MemoryError, Result};

#[cfg(windows)]
#[path = "brain_windows.rs"]
mod windows_publish;

/// Largest complete note accepted by the approval card and disk writer.
pub const MAX_BRAIN_BYTES: usize = 64 * 1024;

/// Remove private Windows publication names left by a terminated daemon.
/// A published note is a different directory entry and is never removed. Live
/// Windows staging handles deny deletion, so another daemon's active write is
/// skipped. Linux normally has no staging name because it uses `O_TMPFILE`.
///
/// # Errors
/// Unsafe brain root, directory enumeration failure, or unexpected disk error.
pub fn cleanup_stale_staging(data_dir: &Path) -> Result<usize> {
    let state = trusted_directory(data_dir)?;
    let brain = match state.open_dir_nofollow("brain") {
        Ok(brain) => brain,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut removed = 0;
    for entry in brain.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = name
            .strip_prefix(".praxi-brain-")
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        if !Uuid::parse_str(id).is_ok_and(|uuid| uuid.to_string() == id) {
            continue;
        }
        let metadata = brain.symlink_metadata(name)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            continue;
        }
        // Unlink exactly the reserved name relative to the held brain handle.
        // Even a name replacement cannot turn this into removal of its target.
        match brain.remove_file(name) {
            Ok(()) => removed += 1,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(removed)
}

/// Model-proposed content. No bot, root, trust, grant or absolute path field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrainWriteInput {
    /// New ordinary Markdown filename, relative to the shared brain.
    pub filename: String,
    /// Body to publish, shown verbatim in the approval card.
    pub content: String,
    /// Sources already visible to this bot. Empty means the current run alone.
    #[serde(default)]
    pub sources: Vec<Uuid>,
}

/// Exact facts shown to the user before publication.
#[derive(Debug, Clone, Serialize)]
pub struct BrainPreview {
    /// Fixed card discriminator.
    pub kind: &'static str,
    /// Destination relative to the application state directory.
    pub path: String,
    /// Complete Markdown, including daemon-authored source labels.
    pub content: String,
    /// Hash of those exact UTF-8 bytes.
    pub sha256: String,
    /// Daemon-owned bot identity.
    pub bot_id: String,
    /// Daemon-owned run identity.
    pub run_id: String,
    /// Source ids and versions captured at preparation time.
    pub sources: Vec<BrainSource>,
}

/// A source bound into a publication approval.
#[derive(Debug, Clone, Serialize)]
pub struct BrainSource {
    /// Scoped source id.
    pub id: Uuid,
    /// Version approved by the user.
    pub revision: u64,
    /// Display label (never an authority claim).
    pub label: String,
}

/// An immutable pending operation. Consume it exactly once after approval.
pub struct PreparedBrainWrite {
    scope: Scope,
    state: Dir,
    destination: Dir,
    filename: String,
    preview: BrainPreview,
}

struct StagedBrainNote {
    directory: Dir,
    file: Option<File>,
    name: Option<String>,
}

impl StagedBrainNote {
    #[cfg(target_os = "linux")]
    fn new(directory: &Dir) -> Result<Self> {
        use rustix::fs::{Mode, OFlags, openat};

        // No pathname exists while the approved bytes are being written or
        // while source admission waits. Another writer cannot replace or edit
        // a visible staging name and turn it into the approved publication.
        let fd = openat(
            directory,
            ".",
            OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|e| {
            invalid(&format!(
                "cannot create atomic brain staging: the Linux filesystem must support O_TMPFILE ({e})"
            ))
        })?;
        Ok(Self {
            directory: directory.try_clone()?,
            file: Some(File::from_std(std::fs::File::from(fd))),
            name: None,
        })
    }

    #[cfg(windows)]
    fn new(directory: &Dir) -> Result<Self> {
        // Sharing checks prevent ordinary writes/deletion. Publication also
        // binds the held file handle: it never trusts this mutable pathname.
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        let directory = directory.try_clone()?;
        let name = format!(".praxi-brain-{}.tmp", Uuid::now_v7());
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ);
        let file = directory.open_with(&name, &options)?;
        Ok(Self {
            directory,
            file: Some(file),
            name: Some(name),
        })
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    fn new(_directory: &Dir) -> Result<Self> {
        Err(invalid(
            "atomic shared-brain publication currently requires Linux or Windows",
        ))
    }

    fn file(&mut self) -> &mut File {
        self.file.as_mut().expect("staged file is open until drop")
    }

    #[cfg(target_os = "linux")]
    fn publish(&self, filename: &str) -> Result<()> {
        use std::os::fd::AsRawFd;

        use rustix::fs::{AtFlags, CWD, linkat};

        let file = self.file.as_ref().expect("staged file is open until drop");
        // The procfs fd reference binds the original unnamed inode, unlike a
        // link from a mutable staging name. This is the unprivileged form of
        // linkat(AT_EMPTY_PATH); the destination stays relative to its capability.
        let source = format!("/proc/self/fd/{}", file.as_raw_fd());
        linkat(
            CWD,
            &source,
            &self.directory,
            filename,
            AtFlags::SYMLINK_FOLLOW,
        )
        .map_err(|e| {
            invalid(&format!(
                "cannot publish atomic brain note through /proc/self/fd: {e}"
            ))
        })?;
        Ok(())
    }

    #[cfg(windows)]
    fn publish(&self, filename: &str) -> Result<()> {
        windows_publish::publish(self, filename)?;
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    #[allow(clippy::unused_self)] // keep the signature aligned with the platform implementations
    fn publish(&self, _filename: &str) -> Result<()> {
        Err(invalid(
            "atomic shared-brain publication currently requires Linux or Windows",
        ))
    }
}

impl Drop for StagedBrainNote {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Some(name) = &self.name {
            let _ = self.directory.remove_file(name);
        }
    }
}

fn invalid(message: &str) -> MemoryError {
    MemoryError::Invalid(message.into())
}

fn validate_filename(name: &str) -> Result<()> {
    let stem = name
        .strip_suffix(".md")
        .ok_or_else(|| invalid("brain filename must end in .md"))?;
    let device = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if stem.is_empty()
        || name.len() > 100
        || !stem.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || stem.contains("..")
        || stem.ends_with('.')
        || matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((device.starts_with("COM") || device.starts_with("LPT"))
            && device.len() == 4
            && device.ends_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']))
    {
        return Err(invalid(
            "brain filename must be a plain local Markdown filename",
        ));
    }
    Ok(())
}

impl PreparedBrainWrite {
    /// Freeze the destination and complete source-labelled note before approval.
    ///
    /// # Errors
    /// Invalid filename, existing destination, changed/disabled/foreign source,
    /// excessive content, or unsafe filesystem path.
    pub fn prepare(
        data_dir: &Path,
        store: &KnowledgeStore,
        scope: Scope,
        run_id: Uuid,
        input: &BrainWriteInput,
    ) -> Result<Self> {
        if store.scope() != scope {
            return Err(invalid("brain owner does not match its knowledge store"));
        }
        validate_filename(&input.filename)?;
        if input.content.trim().is_empty()
            || input.content.len() > MAX_BRAIN_BYTES
            || input.sources.len() > 16
        {
            return Err(invalid("brain content or source count exceeds limits"));
        }
        if crate::contains_secret(&input.content) {
            return Err(invalid("shared brain content contains secret-like data"));
        }
        let state = trusted_directory(data_dir)?;
        match state.create_dir("brain") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let destination = state.open_dir_nofollow("brain")?;
        match destination.symlink_metadata(&input.filename) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(invalid(
                    "brain destination already exists; choose a new filename",
                ));
            }
        }
        let mut sources = Vec::new();
        for id in &input.sources {
            if sources.iter().any(|source: &BrainSource| source.id == *id) {
                return Err(invalid("duplicate brain source"));
            }
            let source = store
                .source(*id)?
                .ok_or_else(|| invalid("brain source is not visible to this bot"))?;
            if !source.enabled {
                return Err(invalid("brain source is disabled"));
            }
            sources.push(BrainSource {
                id: *id,
                revision: source.revision,
                label: source.label,
            });
        }
        // JSON strings are valid YAML scalars and cannot break the frontmatter.
        let provenance = serde_json::to_string(&sources)?;
        let content = format!(
            "---\npraxi_kind: shared_note\ntrust: untrusted\nbot_id: {}\nrun_id: {}\nsources: {provenance}\n---\n\n{}\n",
            serde_json::to_string(&scope.bot_id.to_string())?,
            serde_json::to_string(&run_id.to_string())?,
            input.content
        );
        if content.len() > MAX_BRAIN_BYTES {
            return Err(invalid("source-labelled note exceeds approval byte limit"));
        }
        let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        let preview = BrainPreview {
            kind: "knowledge_brain_write",
            path: format!("brain/{}", input.filename),
            content,
            sha256,
            bot_id: scope.bot_id.to_string(),
            run_id: run_id.to_string(),
            sources,
        };
        Ok(Self {
            scope,
            state,
            destination,
            filename: input.filename.clone(),
            preview,
        })
    }

    /// Frozen approval details. File contents can never change this metadata.
    #[must_use]
    pub fn preview(&self) -> &BrainPreview {
        &self.preview
    }

    fn check_destination(&self) -> Result<()> {
        let current = self.state.open_dir_nofollow("brain")?.dir_metadata()?;
        let approved = self.destination.dir_metadata()?;
        if current.dev() != approved.dev() || current.ino() != approved.ino() {
            return Err(invalid(
                "approved brain directory changed; request approval again",
            ));
        }
        Ok(())
    }

    /// Publish after exact-call approval; refuse if a source changed or a human
    /// created the target while the card was open. The final link is atomic and
    /// cannot replace an existing file, including a symlink.
    /// Linux requires an O_TMPFILE-capable filesystem and /proc/self/fd;
    /// Windows links the held file handle and pins staging against writes.
    /// Other platforms fail closed instead of using a mutable staging pathname.
    ///
    /// # Errors
    /// Source changed/revoked, target appeared, or disk I/O failed.
    pub fn commit(self, store: &KnowledgeStore) -> Result<String> {
        if store.scope() != self.scope {
            return Err(invalid("approved brain owner changed"));
        }
        self.check_destination()?;
        let staging = self.stage()?;
        self.publish(store, &staging)
    }

    fn stage(&self) -> Result<StagedBrainNote> {
        let mut staging = StagedBrainNote::new(&self.destination)?;
        staging.file().write_all(self.preview.content.as_bytes())?;
        staging.file().sync_all()?;
        Ok(staging)
    }

    fn publish(self, store: &KnowledgeStore, staging: &StagedBrainNote) -> Result<String> {
        if store.scope() != self.scope {
            return Err(invalid("approved brain owner changed"));
        }
        let revisions: Vec<_> = self
            .preview
            .sources
            .iter()
            .map(|bound| (bound.id, bound.revision))
            .collect();
        // Staging, fsync and waiting for the source guard may yield to a
        // directory replacement. Check at publication admission too.
        store.with_current_sources(&revisions, || {
            self.check_destination()?;
            staging.publish(&self.filename)
        })?;
        Ok(self.preview.path)
    }
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod tests {
    use super::super::{NewSource, SourceKind};
    use super::*;
    use crate::Trust;
    use protocol::{BotId, UserId};

    fn fixture() -> (tempfile::TempDir, KnowledgeStore, Scope) {
        let dir = tempfile::tempdir().unwrap();
        let scope = Scope {
            user_id: UserId::new(),
            bot_id: BotId::new(),
        };
        let store = KnowledgeStore::open_in_memory(scope).unwrap();
        (dir, store, scope)
    }

    #[test]
    fn freezes_exact_bytes_and_never_overwrites_a_human_note() {
        let (dir, store, scope) = fixture();
        let input = BrainWriteInput {
            filename: "decision.md".into(),
            content: "Use SQLite.".into(),
            sources: vec![],
        };
        let prepared =
            PreparedBrainWrite::prepare(dir.path(), &store, scope, Uuid::now_v7(), &input).unwrap();
        assert!(prepared.preview().content.contains("trust: untrusted"));
        std::fs::write(dir.path().join("brain/decision.md"), "human edit").unwrap();
        assert!(prepared.commit(&store).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("brain/decision.md")).unwrap(),
            "human edit"
        );
        assert!(
            PreparedBrainWrite::prepare(
                dir.path(),
                &store,
                scope,
                Uuid::now_v7(),
                &BrainWriteInput {
                    filename: "../escape.md".into(),
                    ..input
                }
            )
            .is_err()
        );
    }

    #[test]
    fn removed_source_invalidates_pending_publication() {
        let (dir, store, scope) = fixture();
        let source = store
            .add_source(NewSource {
                kind: SourceKind::Note,
                label: "Evidence".into(),
                locator: "note:one".into(),
                trust: Trust::Untrusted,
            })
            .unwrap();
        let input = BrainWriteInput {
            filename: "decision.md".into(),
            content: "A decision".into(),
            sources: vec![source.id],
        };
        let prepared =
            PreparedBrainWrite::prepare(dir.path(), &store, scope, Uuid::now_v7(), &input).unwrap();
        store.delete_source(source.id, source.revision).unwrap();
        assert!(prepared.commit(&store).is_err());
        assert!(!dir.path().join("brain/decision.md").exists());
    }

    fn attempted_staging_change_cannot_change_approved_bytes(replace: bool) {
        let (dir, store, scope) = fixture();
        let input = BrainWriteInput {
            filename: "decision.md".into(),
            content: "Only the exact approved decision may be published.".into(),
            sources: vec![],
        };
        let prepared =
            PreparedBrainWrite::prepare(dir.path(), &store, scope, Uuid::now_v7(), &input).unwrap();
        let approved = prepared.preview().content.clone();
        let staging = prepared.stage().unwrap();
        // This is the deterministic interval between fsync and publication
        // admission. A held SQLite source guard can extend it arbitrarily.
        if let Some(name) = &staging.name {
            let path = dir.path().join("brain").join(name);
            if replace {
                if std::fs::remove_file(&path).is_ok() {
                    std::fs::write(&path, "UNAPPROVED-REPLACEMENT").unwrap();
                }
            } else {
                let _ = std::fs::write(&path, "UNAPPROVED-IN-PLACE-EDIT");
            }
        } else {
            // An unnamed stage exposes no path another writer can replace.
            assert_eq!(
                std::fs::read_dir(dir.path().join("brain")).unwrap().count(),
                0
            );
        }
        prepared.publish(&store, &staging).unwrap();
        drop(staging);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("brain/decision.md")).unwrap(),
            approved,
            "publication used bytes that were never approved"
        );
        assert_eq!(
            std::fs::read_dir(dir.path().join("brain")).unwrap().count(),
            1
        );
    }

    #[test]
    fn replacing_staging_path_cannot_publish_unapproved_bytes() {
        attempted_staging_change_cannot_change_approved_bytes(true);
    }

    #[test]
    fn editing_staging_path_cannot_publish_unapproved_bytes() {
        attempted_staging_change_cannot_change_approved_bytes(false);
    }

    fn replace_brain_directory(brain: &Path, moved: &Path) -> bool {
        match std::fs::rename(brain, moved) {
            Ok(()) => true,
            Err(error) => {
                #[cfg(windows)]
                {
                    // cap_std pins open Windows directory handles against
                    // deletion/rename. That is an equally safe outcome.
                    assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
                    false
                }
                #[cfg(not(windows))]
                {
                    panic!("cannot exercise directory replacement: {error}")
                }
            }
        }
    }

    #[test]
    fn replaced_brain_directory_cannot_retarget_publication() {
        let (dir, store, scope) = fixture();
        let input = BrainWriteInput {
            filename: "decision.md".into(),
            content: "A decision".into(),
            sources: vec![],
        };
        let prepared =
            PreparedBrainWrite::prepare(dir.path(), &store, scope, Uuid::now_v7(), &input).unwrap();
        let approved = prepared.preview().content.clone();
        if !replace_brain_directory(&dir.path().join("brain"), &dir.path().join("moved-brain")) {
            prepared.commit(&store).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("brain/decision.md")).unwrap(),
                approved
            );
            return;
        }
        std::fs::create_dir(dir.path().join("brain")).unwrap();
        assert!(prepared.commit(&store).is_err());
        assert!(!dir.path().join("moved-brain/decision.md").exists());
        assert!(!dir.path().join("brain/decision.md").exists());
    }

    #[test]
    fn directory_replaced_while_publication_waits_cannot_receive_approved_note() {
        let (dir, store, scope) = fixture();
        let store = std::sync::Arc::new(store);
        let input = BrainWriteInput {
            filename: "decision.md".into(),
            content: "A decision with an exact approved destination.".into(),
            sources: vec![],
        };
        let prepared =
            PreparedBrainWrite::prepare(dir.path(), &store, scope, Uuid::now_v7(), &input).unwrap();
        let approved = prepared.preview().content.clone();
        // Stage first, then hold the source guard through the replacement. This
        // also covers unnamed Linux staging without polling for a visible file.
        let staging = prepared.stage().unwrap();
        let publication_barrier = store.conn();
        let worker_store = std::sync::Arc::clone(&store);
        let worker = std::thread::spawn(move || prepared.publish(&worker_store, &staging));
        let brain = dir.path().join("brain");
        let moved = dir.path().join("moved-brain");
        if !replace_brain_directory(&brain, &moved) {
            drop(publication_barrier);
            worker.join().unwrap().unwrap();
            assert_eq!(
                std::fs::read_to_string(brain.join("decision.md")).unwrap(),
                approved
            );
            assert!(!moved.exists());
            return;
        }
        std::fs::create_dir(&brain).unwrap();
        drop(publication_barrier);
        assert!(
            worker.join().unwrap().is_err(),
            "publication accepted a replaced approved directory"
        );
        assert!(!moved.join("decision.md").exists());
        assert!(!brain.join("decision.md").exists());
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
    }
}
