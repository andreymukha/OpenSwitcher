pub mod user_services;

use crate::error::SystemContextError;
use crate::model::{DesktopEnvironment, DistroKind, SessionType, SystemContext};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct SystemContextDetector;

const RUNTIME_MODE_ENV: &str = "OPEN_SWITCHER_RUNTIME_MODE";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SessionEnvironment {
    xdg_session_type: Option<String>,
    xdg_current_desktop: Option<String>,
    xdg_session_desktop: Option<String>,
    desktop_session: Option<String>,
}

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
        let session_environment = current_session_environment();
        Ok(build_system_context(distro, session_environment))
    }
}

fn build_system_context(distro: DistroKind, env: SessionEnvironment) -> SystemContext {
    SystemContext {
        session_type: detect_session_type(env.xdg_session_type.as_deref()),
        desktop_environment: detect_desktop_environment(
            env.xdg_current_desktop.as_deref(),
            env.xdg_session_desktop.as_deref(),
            env.desktop_session.as_deref(),
        ),
        distro,
    }
}

fn current_session_environment() -> SessionEnvironment {
    current_session_environment_from_sources(
        process_session_environment(),
        systemd_user_session_environment().unwrap_or_default(),
    )
}

fn current_session_environment_from_sources(
    process_env: SessionEnvironment,
    systemd_env: SessionEnvironment,
) -> SessionEnvironment {
    SessionEnvironment {
        xdg_session_type: process_env.xdg_session_type.or(systemd_env.xdg_session_type),
        xdg_current_desktop: process_env
            .xdg_current_desktop
            .or(systemd_env.xdg_current_desktop),
        xdg_session_desktop: process_env
            .xdg_session_desktop
            .or(systemd_env.xdg_session_desktop),
        desktop_session: process_env.desktop_session.or(systemd_env.desktop_session),
    }
}

fn process_session_environment() -> SessionEnvironment {
    SessionEnvironment {
        xdg_session_type: env::var("XDG_SESSION_TYPE").ok(),
        xdg_current_desktop: env::var("XDG_CURRENT_DESKTOP").ok(),
        xdg_session_desktop: env::var("XDG_SESSION_DESKTOP").ok(),
        desktop_session: env::var("DESKTOP_SESSION").ok(),
    }
}

fn systemd_user_session_environment() -> Option<SessionEnvironment> {
    let output = Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(parse_systemd_user_environment(
        &String::from_utf8_lossy(&output.stdout),
    ))
}

fn parse_systemd_user_environment(output: &str) -> SessionEnvironment {
    let mut env = SessionEnvironment::default();

    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key {
            "XDG_SESSION_TYPE" => env.xdg_session_type = Some(value.to_string()),
            "XDG_CURRENT_DESKTOP" => env.xdg_current_desktop = Some(value.to_string()),
            "XDG_SESSION_DESKTOP" => env.xdg_session_desktop = Some(value.to_string()),
            "DESKTOP_SESSION" => env.desktop_session = Some(value.to_string()),
            _ => {}
        }
    }

    env
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
    fn parses_systemd_user_environment_for_session_context_keys() {
        let parsed = parse_systemd_user_environment(
            "XDG_SESSION_TYPE=wayland\nXDG_CURRENT_DESKTOP=ubuntu:GNOME\nDESKTOP_SESSION=ubuntu\nIGNORED=value\n",
        );

        assert_eq!(parsed.xdg_session_type.as_deref(), Some("wayland"));
        assert_eq!(
            parsed.xdg_current_desktop.as_deref(),
            Some("ubuntu:GNOME")
        );
        assert_eq!(parsed.desktop_session.as_deref(), Some("ubuntu"));
        assert_eq!(parsed.xdg_session_desktop, None);
    }

    #[test]
    fn merged_session_environment_fills_missing_process_values_from_systemd_environment() {
        let merged = current_session_environment_from_sources(
            SessionEnvironment {
                xdg_session_type: None,
                xdg_current_desktop: None,
                xdg_session_desktop: None,
                desktop_session: Some("ubuntu".to_string()),
            },
            SessionEnvironment {
                xdg_session_type: Some("wayland".to_string()),
                xdg_current_desktop: Some("ubuntu:GNOME".to_string()),
                xdg_session_desktop: Some("ubuntu".to_string()),
                desktop_session: None,
            },
        );

        assert_eq!(merged.xdg_session_type.as_deref(), Some("wayland"));
        assert_eq!(
            merged.xdg_current_desktop.as_deref(),
            Some("ubuntu:GNOME")
        );
        assert_eq!(merged.xdg_session_desktop.as_deref(), Some("ubuntu"));
        assert_eq!(merged.desktop_session.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn builds_system_context_from_merged_environment() {
        let context = build_system_context(
            DistroKind::Ubuntu,
            SessionEnvironment {
                xdg_session_type: Some("wayland".to_string()),
                xdg_current_desktop: Some("ubuntu:GNOME".to_string()),
                xdg_session_desktop: None,
                desktop_session: Some("ubuntu".to_string()),
            },
        );

        assert_eq!(
            context,
            SystemContext {
                session_type: SessionType::Wayland,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            }
        );
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
