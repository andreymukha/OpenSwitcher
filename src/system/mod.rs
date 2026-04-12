pub mod user_services;

use crate::error::SystemContextError;
use crate::model::{DesktopEnvironment, DistroKind, SessionType, SystemContext};
use std::env;
use std::fs;
use std::path::Path;

pub struct SystemContextDetector;

const RUNTIME_MODE_ENV: &str = "OPEN_SWITCHER_RUNTIME_MODE";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMode {
    Dev,
    Managed,
}

fn parse_runtime_mode(value: Option<&str>) -> RuntimeMode {
    match value
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("dev") => RuntimeMode::Dev,
        _ => RuntimeMode::Managed,
    }
}

pub fn current_runtime_mode() -> RuntimeMode {
    parse_runtime_mode(env::var(RUNTIME_MODE_ENV).ok().as_deref())
}

pub fn is_dev_runtime_mode() -> bool {
    current_runtime_mode() == RuntimeMode::Dev
}

pub use user_services::{UserServiceController, DAEMON_UNIT, TRAY_UNIT};

impl SystemContextDetector {
    pub fn detect_current() -> Result<SystemContext, SystemContextError> {
        let distro = detect_distro(Path::new("/etc/os-release"))?;
        Ok(SystemContext {
            session_type: detect_session_type(env::var("XDG_SESSION_TYPE").ok().as_deref()),
            desktop_environment: detect_desktop_environment(
                env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
                env::var("XDG_SESSION_DESKTOP").ok().as_deref(),
                env::var("DESKTOP_SESSION").ok().as_deref(),
            ),
            distro,
        })
    }
}

fn detect_session_type(value: Option<&str>) -> SessionType {
    match value
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("x11") => SessionType::X11,
        Some("wayland") => SessionType::Wayland,
        _ => SessionType::Unknown,
    }
}

fn detect_desktop_environment(
    current_desktop: Option<&str>,
    session_desktop: Option<&str>,
    desktop_session: Option<&str>,
) -> DesktopEnvironment {
    [current_desktop, session_desktop, desktop_session]
        .into_iter()
        .flatten()
        .find_map(|candidate| match candidate.to_ascii_lowercase() {
            value if value.contains("cinnamon") => Some(DesktopEnvironment::Cinnamon),
            value if value.contains("gnome") => Some(DesktopEnvironment::Gnome),
            value if value.contains("xfce") => Some(DesktopEnvironment::Xfce),
            value if value.contains("kde") || value.contains("plasma") => {
                Some(DesktopEnvironment::Kde)
            }
            _ => None,
        })
        .unwrap_or(DesktopEnvironment::Unknown)
}

fn detect_distro(os_release_path: &Path) -> Result<DistroKind, SystemContextError> {
    let content = fs::read_to_string(os_release_path).map_err(SystemContextError::OsReleaseIo)?;
    let id = content
        .lines()
        .find_map(|line| line.strip_prefix("ID="))
        .map(|value| value.trim_matches('"').to_ascii_lowercase());

    Ok(match id.as_deref() {
        Some("linuxmint") => DistroKind::LinuxMint,
        Some("ubuntu") => DistroKind::Ubuntu,
        Some("debian") => DistroKind::Debian,
        _ => DistroKind::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_desktop_environment_from_any_known_variable() {
        let detected = detect_desktop_environment(Some("X-Cinnamon"), None, None);
        assert_eq!(detected, DesktopEnvironment::Cinnamon);

        let detected = detect_desktop_environment(None, Some("gnome"), None);
        assert_eq!(detected, DesktopEnvironment::Gnome);

        let detected = detect_desktop_environment(None, None, Some("xfce"));
        assert_eq!(detected, DesktopEnvironment::Xfce);
    }

    #[test]
    fn detects_session_type_case_insensitively() {
        assert_eq!(detect_session_type(Some("X11")), SessionType::X11);
        assert_eq!(detect_session_type(Some("wayland")), SessionType::Wayland);
    }

    #[test]
    fn detects_distro_from_os_release() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("os-release");
        fs::write(&path, "ID=linuxmint\n").unwrap();

        let distro = detect_distro(&path).unwrap();
        assert_eq!(distro, DistroKind::LinuxMint);
    }

    #[test]
    fn runtime_mode_defaults_to_managed() {
        assert_eq!(parse_runtime_mode(None), RuntimeMode::Managed);
        assert_eq!(parse_runtime_mode(Some("managed")), RuntimeMode::Managed);
    }

    #[test]
    fn runtime_mode_detects_dev_from_environment() {
        assert_eq!(parse_runtime_mode(Some("dev")), RuntimeMode::Dev);
        assert_eq!(parse_runtime_mode(Some("DEV")), RuntimeMode::Dev);
    }
}
