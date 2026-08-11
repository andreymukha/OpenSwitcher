pub mod user_services;

use crate::daemon::debug_log::{try_debug_line, DebugLogKind};
use crate::error::SystemContextError;
use crate::model::{DesktopEnvironment, DistroKind, SessionType, SystemContext};
use std::env;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct SystemContextDetector;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SessionEnvironment {
    xdg_session_type: Option<String>,
    xdg_current_desktop: Option<String>,
    xdg_session_desktop: Option<String>,
    desktop_session: Option<String>,
    wayland_display: Option<String>,
    display: Option<String>,
    xdg_runtime_dir: Option<String>,
}

impl SessionEnvironment {
    fn set(&mut self, key: &str, value: String) {
        match key {
            "XDG_SESSION_TYPE" => self.xdg_session_type = Some(value),
            "XDG_CURRENT_DESKTOP" => self.xdg_current_desktop = Some(value),
            "XDG_SESSION_DESKTOP" => self.xdg_session_desktop = Some(value),
            "DESKTOP_SESSION" => self.desktop_session = Some(value),
            "WAYLAND_DISPLAY" => self.wayland_display = Some(value),
            "DISPLAY" => self.display = Some(value),
            "XDG_RUNTIME_DIR" => self.xdg_runtime_dir = Some(value),
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionTypeEvidence {
    XdgSessionTypeWayland,
    XdgSessionTypeX11,
    StaleX11OverriddenByLiveWaylandSocket,
    X11WithStaleWaylandDisplay,
    LiveWaylandSocket,
    DisplayOnly,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionDetectionSummary {
    process_env: SessionEnvironment,
    systemd_env: SessionEnvironment,
    merged_env: SessionEnvironment,
    chosen_session_type: SessionType,
    chosen_desktop_environment: DesktopEnvironment,
    wayland_socket_path: Option<PathBuf>,
    wayland_socket_live: bool,
    has_conflict: bool,
    has_suspicious_stale_env: bool,
    session_evidence: SessionTypeEvidence,
}

impl SessionDetectionSummary {
    fn log_fields(&self) -> String {
        format!(
            "process_env={:?} systemd_env={:?} merged_env={:?} chosen_session_type={:?} chosen_desktop_environment={:?} wayland_socket_path={:?} wayland_socket_live={} conflict={} suspicious={} evidence={:?}",
            self.process_env,
            self.systemd_env,
            self.merged_env,
            self.chosen_session_type,
            self.chosen_desktop_environment,
            self.wayland_socket_path,
            self.wayland_socket_live,
            self.has_conflict,
            self.has_suspicious_stale_env,
            self.session_evidence
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SystemContextDetection {
    context: SystemContext,
    summary: SessionDetectionSummary,
}

pub use user_services::{UserServiceController, DAEMON_UNIT, TRAY_UNIT};

impl SystemContextDetector {
    pub fn detect_current() -> Result<SystemContext, SystemContextError> {
        let distro = detect_distro(Path::new("/etc/os-release"))?;
        let detection = detect_system_context_from_sources(
            distro,
            process_session_environment(),
            systemd_user_session_environment().unwrap_or_default(),
        );
        log_session_detection_summary(&detection.summary);
        Ok(detection.context)
    }
}

#[cfg(test)]
fn build_system_context(distro: DistroKind, env: SessionEnvironment) -> SystemContext {
    detect_system_context_from_sources(distro, env, SessionEnvironment::default()).context
}

fn detect_system_context_from_sources(
    distro: DistroKind,
    process_env: SessionEnvironment,
    systemd_env: SessionEnvironment,
) -> SystemContextDetection {
    let merged_env =
        current_session_environment_from_sources(process_env.clone(), systemd_env.clone());
    let wayland_socket_path = wayland_socket_path(
        merged_env.xdg_runtime_dir.as_deref(),
        merged_env.wayland_display.as_deref(),
    );
    let wayland_socket_live = wayland_socket_path
        .as_deref()
        .is_some_and(wayland_socket_is_live);
    let process_session_type = detect_session_type(process_env.xdg_session_type.as_deref());
    let systemd_session_type = detect_session_type(systemd_env.xdg_session_type.as_deref());
    let session_conflict = process_session_type != SessionType::Unknown
        && systemd_session_type != SessionType::Unknown
        && process_session_type != systemd_session_type;
    let (session_type, evidence, suspicious) =
        reconcile_session_type(&merged_env, wayland_socket_live, session_conflict);
    let desktop_environment = detect_desktop_environment(
        merged_env.xdg_current_desktop.as_deref(),
        merged_env.xdg_session_desktop.as_deref(),
        merged_env.desktop_session.as_deref(),
    );

    let context = SystemContext {
        session_type,
        desktop_environment,
        distro,
    };
    let summary = SessionDetectionSummary {
        process_env,
        systemd_env,
        merged_env,
        chosen_session_type: session_type,
        chosen_desktop_environment: desktop_environment,
        wayland_socket_path,
        wayland_socket_live,
        has_conflict: session_conflict
            || matches!(
                evidence,
                SessionTypeEvidence::StaleX11OverriddenByLiveWaylandSocket
            ),
        has_suspicious_stale_env: suspicious,
        session_evidence: evidence,
    };

    SystemContextDetection { context, summary }
}

fn reconcile_session_type(
    env: &SessionEnvironment,
    wayland_socket_live: bool,
    session_conflict: bool,
) -> (SessionType, SessionTypeEvidence, bool) {
    let xdg_session_type = detect_session_type(env.xdg_session_type.as_deref());
    let has_wayland_display = env
        .wayland_display
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_display = env
        .display
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());

    match xdg_session_type {
        SessionType::Wayland => (
            SessionType::Wayland,
            SessionTypeEvidence::XdgSessionTypeWayland,
            has_wayland_display && !wayland_socket_live,
        ),
        SessionType::X11 if wayland_socket_live => (
            SessionType::Wayland,
            SessionTypeEvidence::StaleX11OverriddenByLiveWaylandSocket,
            true,
        ),
        SessionType::X11 if has_wayland_display => (
            SessionType::X11,
            SessionTypeEvidence::X11WithStaleWaylandDisplay,
            true,
        ),
        SessionType::X11 => (
            SessionType::X11,
            SessionTypeEvidence::XdgSessionTypeX11,
            session_conflict,
        ),
        SessionType::Unknown if wayland_socket_live => (
            SessionType::Wayland,
            SessionTypeEvidence::LiveWaylandSocket,
            false,
        ),
        SessionType::Unknown if has_display && !has_wayland_display => {
            (SessionType::X11, SessionTypeEvidence::DisplayOnly, false)
        }
        SessionType::Unknown if has_wayland_display => {
            (SessionType::Unknown, SessionTypeEvidence::Unknown, true)
        }
        SessionType::Unknown => (SessionType::Unknown, SessionTypeEvidence::Unknown, false),
    }
}

fn wayland_socket_path(
    runtime_dir: Option<&str>,
    wayland_display: Option<&str>,
) -> Option<PathBuf> {
    let display = wayland_display?.trim();
    if display.is_empty() {
        return None;
    }

    let display_path = Path::new(display);
    if display_path.is_absolute() {
        return Some(display_path.to_path_buf());
    }

    Some(PathBuf::from(runtime_dir?).join(display))
}

fn wayland_socket_is_live(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

fn log_session_detection_summary(summary: &SessionDetectionSummary) {
    let _ = try_debug_line(DebugLogKind::Layout, || {
        format!("[session-detect] {}", summary.log_fields())
    });
}

fn current_session_environment_from_sources(
    process_env: SessionEnvironment,
    systemd_env: SessionEnvironment,
) -> SessionEnvironment {
    SessionEnvironment {
        xdg_session_type: process_env
            .xdg_session_type
            .or(systemd_env.xdg_session_type),
        xdg_current_desktop: process_env
            .xdg_current_desktop
            .or(systemd_env.xdg_current_desktop),
        xdg_session_desktop: process_env
            .xdg_session_desktop
            .or(systemd_env.xdg_session_desktop),
        desktop_session: process_env.desktop_session.or(systemd_env.desktop_session),
        wayland_display: process_env.wayland_display.or(systemd_env.wayland_display),
        display: process_env.display.or(systemd_env.display),
        xdg_runtime_dir: process_env.xdg_runtime_dir.or(systemd_env.xdg_runtime_dir),
    }
}

fn process_session_environment() -> SessionEnvironment {
    SessionEnvironment {
        xdg_session_type: env::var("XDG_SESSION_TYPE").ok(),
        xdg_current_desktop: env::var("XDG_CURRENT_DESKTOP").ok(),
        xdg_session_desktop: env::var("XDG_SESSION_DESKTOP").ok(),
        desktop_session: env::var("DESKTOP_SESSION").ok(),
        wayland_display: env::var("WAYLAND_DISPLAY").ok(),
        display: env::var("DISPLAY").ok(),
        xdg_runtime_dir: env::var("XDG_RUNTIME_DIR").ok(),
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

    Some(parse_systemd_user_environment(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_systemd_user_environment(output: &str) -> SessionEnvironment {
    let mut env = SessionEnvironment::default();

    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        env.set(key, value.to_string());
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
    use std::os::unix::net::UnixListener;

    fn env_with(values: &[(&str, &str)]) -> SessionEnvironment {
        let mut env = SessionEnvironment::default();
        for (key, value) in values {
            env.set(key, (*value).to_string());
        }
        env
    }

    fn bind_wayland_socket(dir: &Path, name: &str) -> (UnixListener, std::path::PathBuf) {
        let path = dir.join(name);
        let listener = UnixListener::bind(&path).unwrap();
        (listener, path)
    }

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
            "XDG_SESSION_TYPE=wayland\nXDG_CURRENT_DESKTOP=ubuntu:GNOME\nDESKTOP_SESSION=ubuntu\nWAYLAND_DISPLAY=wayland-0\nDISPLAY=:0\nXDG_RUNTIME_DIR=/tmp/runtime-test\nIGNORED=value\n",
        );

        assert_eq!(parsed.xdg_session_type.as_deref(), Some("wayland"));
        assert_eq!(parsed.xdg_current_desktop.as_deref(), Some("ubuntu:GNOME"));
        assert_eq!(parsed.desktop_session.as_deref(), Some("ubuntu"));
        assert_eq!(parsed.xdg_session_desktop, None);
        assert_eq!(parsed.wayland_display.as_deref(), Some("wayland-0"));
        assert_eq!(parsed.display.as_deref(), Some(":0"));
        assert_eq!(parsed.xdg_runtime_dir.as_deref(), Some("/tmp/runtime-test"));
    }

    #[test]
    fn builds_wayland_socket_path_from_absolute_wayland_display() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("custom-wayland");

        assert_eq!(
            wayland_socket_path(None, Some(socket_path.to_str().unwrap())),
            Some(socket_path)
        );
    }

    #[test]
    fn builds_wayland_socket_path_from_runtime_dir_and_relative_wayland_display() {
        let temp_dir = tempfile::tempdir().unwrap();

        assert_eq!(
            wayland_socket_path(
                Some(temp_dir.path().to_str().unwrap()),
                Some("wayland-test")
            ),
            Some(temp_dir.path().join("wayland-test"))
        );
    }

    #[test]
    fn does_not_guess_wayland_zero_when_wayland_display_is_missing() {
        let temp_dir = tempfile::tempdir().unwrap();

        assert_eq!(
            wayland_socket_path(Some(temp_dir.path().to_str().unwrap()), None),
            None
        );
    }

    #[test]
    fn detects_live_wayland_socket() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (_listener, socket_path) = bind_wayland_socket(temp_dir.path(), "wayland-test");

        assert!(wayland_socket_is_live(&socket_path));
        assert!(!wayland_socket_is_live(&temp_dir.path().join("missing")));
    }

    #[test]
    fn xdg_wayland_with_display_stays_wayland_without_conflict() {
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            env_with(&[("XDG_SESSION_TYPE", "wayland"), ("DISPLAY", ":0")]),
            SessionEnvironment::default(),
        );

        assert_eq!(detection.context.session_type, SessionType::Wayland);
        assert!(!detection.summary.has_conflict);
        assert!(!detection.summary.has_suspicious_stale_env);
    }

    #[test]
    fn stale_x11_env_with_live_wayland_socket_prefers_wayland() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (_listener, _socket_path) = bind_wayland_socket(temp_dir.path(), "wayland-test");
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            env_with(&[
                ("XDG_SESSION_TYPE", "x11"),
                ("WAYLAND_DISPLAY", "wayland-test"),
                ("XDG_RUNTIME_DIR", temp_dir.path().to_str().unwrap()),
            ]),
            SessionEnvironment::default(),
        );

        assert_eq!(detection.context.session_type, SessionType::Wayland);
        assert!(detection.summary.wayland_socket_live);
        assert!(detection.summary.has_conflict);
        assert!(detection.summary.has_suspicious_stale_env);
        assert_eq!(
            detection.summary.session_evidence,
            SessionTypeEvidence::StaleX11OverriddenByLiveWaylandSocket
        );
    }

    #[test]
    fn x11_env_with_stale_wayland_display_remains_x11_and_is_suspicious() {
        let temp_dir = tempfile::tempdir().unwrap();
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            env_with(&[
                ("XDG_SESSION_TYPE", "x11"),
                ("WAYLAND_DISPLAY", "missing-wayland"),
                ("XDG_RUNTIME_DIR", temp_dir.path().to_str().unwrap()),
            ]),
            SessionEnvironment::default(),
        );

        assert_eq!(detection.context.session_type, SessionType::X11);
        assert!(!detection.summary.wayland_socket_live);
        assert!(!detection.summary.has_conflict);
        assert!(detection.summary.has_suspicious_stale_env);
        assert_eq!(
            detection.summary.session_evidence,
            SessionTypeEvidence::X11WithStaleWaylandDisplay
        );
    }

    #[test]
    fn missing_session_type_with_live_wayland_socket_prefers_wayland() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (_listener, _socket_path) = bind_wayland_socket(temp_dir.path(), "wayland-test");
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            env_with(&[
                ("WAYLAND_DISPLAY", "wayland-test"),
                ("XDG_RUNTIME_DIR", temp_dir.path().to_str().unwrap()),
            ]),
            SessionEnvironment::default(),
        );

        assert_eq!(detection.context.session_type, SessionType::Wayland);
        assert_eq!(
            detection.summary.session_evidence,
            SessionTypeEvidence::LiveWaylandSocket
        );
    }

    #[test]
    fn missing_session_type_with_display_only_prefers_x11() {
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            env_with(&[("DISPLAY", ":0")]),
            SessionEnvironment::default(),
        );

        assert_eq!(detection.context.session_type, SessionType::X11);
        assert_eq!(
            detection.summary.session_evidence,
            SessionTypeEvidence::DisplayOnly
        );
    }

    #[test]
    fn systemd_environment_fills_missing_process_wayland_values() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (_listener, _socket_path) = bind_wayland_socket(temp_dir.path(), "wayland-test");
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            SessionEnvironment::default(),
            env_with(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
                ("WAYLAND_DISPLAY", "wayland-test"),
                ("XDG_RUNTIME_DIR", temp_dir.path().to_str().unwrap()),
            ]),
        );

        assert_eq!(
            detection.context,
            SystemContext {
                session_type: SessionType::Wayland,
                desktop_environment: DesktopEnvironment::Gnome,
                distro: DistroKind::Ubuntu,
            }
        );
    }

    #[test]
    fn process_x11_and_systemd_wayland_with_live_socket_prefers_wayland() {
        let temp_dir = tempfile::tempdir().unwrap();
        let (_listener, _socket_path) = bind_wayland_socket(temp_dir.path(), "wayland-test");
        let detection = detect_system_context_from_sources(
            DistroKind::Ubuntu,
            env_with(&[("XDG_SESSION_TYPE", "x11")]),
            env_with(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
                ("WAYLAND_DISPLAY", "wayland-test"),
                ("XDG_RUNTIME_DIR", temp_dir.path().to_str().unwrap()),
            ]),
        );

        assert_eq!(detection.context.session_type, SessionType::Wayland);
        assert_eq!(
            detection.context.desktop_environment,
            DesktopEnvironment::Gnome
        );
        assert!(detection.summary.has_conflict);
    }

    #[test]
    fn merged_session_environment_fills_missing_process_values_from_systemd_environment() {
        let merged = current_session_environment_from_sources(
            SessionEnvironment {
                xdg_session_type: None,
                xdg_current_desktop: None,
                xdg_session_desktop: None,
                desktop_session: Some("ubuntu".to_string()),
                ..SessionEnvironment::default()
            },
            SessionEnvironment {
                xdg_session_type: Some("wayland".to_string()),
                xdg_current_desktop: Some("ubuntu:GNOME".to_string()),
                xdg_session_desktop: Some("ubuntu".to_string()),
                desktop_session: None,
                ..SessionEnvironment::default()
            },
        );

        assert_eq!(merged.xdg_session_type.as_deref(), Some("wayland"));
        assert_eq!(merged.xdg_current_desktop.as_deref(), Some("ubuntu:GNOME"));
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
                ..SessionEnvironment::default()
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
}
