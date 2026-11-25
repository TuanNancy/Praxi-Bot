//! macOS OS backend: Accessibility API, `CGEvent` input, and screenshot capture.
//!
//! Implements the screenshot part of the [`desktop_api::DesktopBackend`] trait
//! with CoreGraphics display and window capture. The accessibility tree
//! (`AXUIElement`) and `CGEvent` input are not implemented yet: those methods
//! return [`desktop_api::DesktopError::Unsupported`].
//!
//! On non-macOS platforms this crate only exposes the platform-independent
//! helpers in [`pure`], so the workspace still builds on Windows and Linux.

pub mod pure;

#[cfg(target_os = "macos")]
mod backend;
#[cfg(target_os = "macos")]
pub mod capture;
#[cfg(target_os = "macos")]
pub mod displays;

#[cfg(target_os = "macos")]
pub use backend::MacosBackend;
