//! Async [`DesktopBackend`] implementation for macOS (screenshots only for now).

use std::time::Duration;

use async_trait::async_trait;
use desktop_api::{
    Action, ActionResult, Capabilities, DesktopBackend, DesktopError, Image, Quality, Region,
    Snapshot, WindowInfo,
    snapshot::{AppSpec, Pid},
};

use crate::capture;

/// A wedged window server should fail fast instead of stalling the agent loop.
const CAPTURE_LIMIT: Duration = Duration::from_secs(8);

async fn bounded<T>(
    op: &str,
    limit: Duration,
    fut: impl std::future::Future<Output = Result<T, DesktopError>>,
) -> Result<T, DesktopError> {
    match tokio::time::timeout(limit, fut).await {
        Ok(result) => result,
        Err(_) => Err(DesktopError::Screenshot(format!(
            "{op} timed out after {limit:?}"
        ))),
    }
}

/// macOS desktop backend. Only screenshots are implemented so far; every other
/// operation reports [`DesktopError::Unsupported`].
#[derive(Debug, Default)]
pub struct MacosBackend;

impl MacosBackend {
    /// Create the backend. Infallible: capture errors surface per call, so a
    /// machine without Screen Recording permission still boots the daemon.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DesktopBackend for MacosBackend {
    async fn snapshot(&self, _scope: Option<u64>) -> Result<Snapshot, DesktopError> {
        Err(DesktopError::Unsupported(
            "the macOS backend cannot read the accessibility tree yet".into(),
        ))
    }

    async fn act(&self, _action: Action) -> Result<ActionResult, DesktopError> {
        Err(DesktopError::Unsupported(
            "the macOS backend cannot send keyboard or mouse input yet".into(),
        ))
    }

    async fn screenshot(&self, region: Region, quality: Quality) -> Result<Image, DesktopError> {
        // `Region::FullScreen` and `Region::Rect` cover the main display in
        // this slice; capturing every monitor stitched into one image is a
        // follow-up (see the issue linked from the PR).
        bounded("screenshot", CAPTURE_LIMIT, async move {
            match tokio::task::spawn_blocking(move || capture::capture(&region, quality)).await {
                Ok(result) => result,
                Err(e) => Err(DesktopError::Internal(format!("screenshot task: {e}"))),
            }
        })
        .await
    }

    async fn windows(&self) -> Result<Vec<WindowInfo>, DesktopError> {
        Err(DesktopError::Unsupported(
            "the macOS backend cannot list windows yet".into(),
        ))
    }

    async fn launch(&self, _app: AppSpec) -> Result<Pid, DesktopError> {
        Err(DesktopError::Unsupported(
            "the macOS backend cannot launch applications yet".into(),
        ))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            platform: "macos".into(),
            accessibility_tree: false,
            screenshot: true,
            app_launch: false,
            has_display: true,
        }
    }
}
