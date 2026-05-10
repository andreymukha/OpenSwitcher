#!/usr/bin/env bash

openswitcher_wayland_doctor_systemd_environment() {
    if [[ -n "${OPEN_SWITCHER_WAYLAND_DOCTOR_SYSTEMD_ENV_FILE:-}" ]]; then
        [[ -r "$OPEN_SWITCHER_WAYLAND_DOCTOR_SYSTEMD_ENV_FILE" ]] || return 1
        cat "$OPEN_SWITCHER_WAYLAND_DOCTOR_SYSTEMD_ENV_FILE"
        return 0
    fi

    command -v systemctl >/dev/null 2>&1 || return 1
    systemctl --user show-environment 2>/dev/null
}

openswitcher_wayland_doctor_systemd_value() {
    local key="$1"
    local env_output="$2"
    local line=""

    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in
            "$key="*)
                printf '%s\n' "${line#*=}"
                return 0
                ;;
        esac
    done <<<"$env_output"

    return 1
}

openswitcher_wayland_doctor_effective_env_value() {
    local key="$1"
    local systemd_env="$2"
    local process_value="${!key-}"

    if [[ -n "$process_value" ]]; then
        printf '%s\n' "$process_value"
        return 0
    fi

    openswitcher_wayland_doctor_systemd_value "$key" "$systemd_env" || true
}

openswitcher_wayland_doctor_print_env_row() {
    local key="$1"
    local systemd_env="$2"
    local process_value="${!key-}"
    local systemd_value
    systemd_value="$(openswitcher_wayland_doctor_systemd_value "$key" "$systemd_env" || true)"
    local chosen="$process_value"
    [[ -n "$chosen" ]] || chosen="$systemd_value"

    printf '  %s: process=%s systemd=%s chosen=%s\n' \
        "$key" \
        "${process_value:-<unset>}" \
        "${systemd_value:-<unset>}" \
        "${chosen:-<unset>}"
}

openswitcher_wayland_doctor_socket_path() {
    local runtime_dir="$1"
    local wayland_display="$2"

    [[ -n "$wayland_display" ]] || return 1

    case "$wayland_display" in
        /*)
            printf '%s\n' "$wayland_display"
            ;;
        *)
            [[ -n "$runtime_dir" ]] || return 1
            printf '%s/%s\n' "$runtime_dir" "$wayland_display"
            ;;
    esac
}

openswitcher_wayland_doctor_session_hint() {
    local xdg_session_type="$1"
    local display="$2"
    local wayland_socket_live="$3"

    case "${xdg_session_type,,}" in
        wayland)
            printf '%s\n' "Wayland"
            ;;
        x11)
            if [[ "$wayland_socket_live" == "yes" ]]; then
                printf '%s\n' "Wayland (stale X11 env suspected)"
            else
                printf '%s\n' "X11"
            fi
            ;;
        *)
            if [[ "$wayland_socket_live" == "yes" ]]; then
                printf '%s\n' "Wayland"
            elif [[ -n "$display" ]]; then
                printf '%s\n' "X11"
            else
                printf '%s\n' "Unknown"
            fi
            ;;
    esac
}

openswitcher_wayland_doctor_session_kind() {
    local xdg_session_type="$1"
    local display="$2"
    local wayland_socket_live="$3"

    case "${xdg_session_type,,}" in
        wayland)
            printf '%s\n' "wayland"
            ;;
        x11)
            if [[ "$wayland_socket_live" == "yes" ]]; then
                printf '%s\n' "wayland"
            else
                printf '%s\n' "x11"
            fi
            ;;
        *)
            if [[ "$wayland_socket_live" == "yes" ]]; then
                printf '%s\n' "wayland"
            elif [[ -n "$display" ]]; then
                printf '%s\n' "x11"
            else
                printf '%s\n' "unknown"
            fi
            ;;
    esac
}

openswitcher_wayland_doctor_desktop_hint() {
    local value="$1"

    case "${value,,}" in
        *gnome*)
            printf '%s\n' "GNOME"
            ;;
        *cinnamon*)
            printf '%s\n' "Cinnamon"
            ;;
        *kde*|*plasma*)
            printf '%s\n' "KDE"
            ;;
        "")
            printf '%s\n' "Unknown"
            ;;
        *)
            printf '%s\n' "$value"
            ;;
    esac
}

openswitcher_wayland_doctor_print_wayland_support_summary() {
    local session_kind="$1"
    local desktop_hint="$2"
    local desktop_normalized="${desktop_hint,,}"

    case "$session_kind" in
        wayland)
            if [[ "$desktop_normalized" == "gnome" ]]; then
                printf '%s\n' "Wayland support status: supported (GNOME Wayland confirmed target)"
                printf '%s\n' "Layout switch detection backend: GNOME gsettings"
                printf '%s\n' "Layout observation backend: GNOME input-sources"
                printf '%s\n' "Layout-dependent correction: supported"
            elif [[ "$desktop_normalized" == "kde" ]]; then
                printf '%s\n' "Wayland support status: degraded (best-effort; desktop not confirmed yet)"
                printf '%s\n' "Wayland warning: non-GNOME Wayland is diagnostics-first and needs manual smoke"
                printf '%s\n' "Layout switch detection backend: unavailable for this desktop"
                printf '%s\n' "Layout observation backend: unavailable for this desktop"
                printf '%s\n' "Layout-dependent correction: degraded"
            else
                printf '%s\n' "Wayland support status: unknown (best-effort; needs manual smoke)"
                printf '%s\n' "Wayland warning: non-GNOME Wayland is diagnostics-first and needs manual smoke"
                printf '%s\n' "Layout switch detection backend: unavailable for this desktop"
                printf '%s\n' "Layout observation backend: unavailable for this desktop"
                printf '%s\n' "Layout-dependent correction: unknown"
            fi
            ;;
        x11)
            printf '%s\n' "Wayland support status: not applicable (X11 session)"
            printf '%s\n' "Layout switch detection backend: not applicable for this session"
            printf '%s\n' "Layout observation backend: not applicable for this session"
            printf '%s\n' "Layout-dependent correction: not applicable for Wayland diagnostics"
            ;;
        *)
            printf '%s\n' "Wayland support status: unknown (session type unknown)"
            printf '%s\n' "Layout switch detection backend: unknown"
            printf '%s\n' "Layout observation backend: unknown"
            printf '%s\n' "Layout-dependent correction: unknown"
            ;;
    esac
}

openswitcher_wayland_doctor_print_tray_acceptance_summary() {
    echo "Tray acceptance:"
    printf '%s\n' "Tray visibility: required for supported environment acceptance"
    printf '%s\n' "Tray absence: does not by itself prove daemon/D-Bus/settings failure"
    printf '%s\n' "Tray missing checks: daemon status, D-Bus response, settings window, tray service status, tray service logs"
    printf '%s\n' "Supported environment smoke: daemon running; D-Bus responds; settings opens; tray visible; tray menu opens; settings opens from tray; user stop path works; tray systemd restart works"
}

openswitcher_wayland_doctor_gsettings_get() {
    local schema="$1"
    local key="$2"

    if [[ -n "${OPEN_SWITCHER_WAYLAND_DOCTOR_GSETTINGS_DIR:-}" ]]; then
        local path="$OPEN_SWITCHER_WAYLAND_DOCTOR_GSETTINGS_DIR/$schema.$key"
        [[ -r "$path" ]] || return 1
        cat "$path"
        return 0
    fi

    command -v gsettings >/dev/null 2>&1 || return 1
    gsettings get "$schema" "$key" 2>/dev/null
}

openswitcher_wayland_doctor_quoted_values() {
    local raw="$1"
    printf '%s\n' "$raw" | grep -o "'[^']*'" | sed "s/^'//;s/'$//" || true
}

openswitcher_wayland_doctor_keybinding_summary() {
    local label="$1"
    local raw="$2"
    local values=()
    mapfile -t values < <(openswitcher_wayland_doctor_quoted_values "$raw")

    if (( ${#values[@]} == 0 )); then
        printf 'GNOME keybinding %s summary: unavailable/disabled\n' "$label"
        return 0
    fi

    local value=""
    for value in "${values[@]}"; do
        case "$value" in
            "<Super>space")
                printf 'GNOME keybinding %s summary: supported Super+Space\n' "$label"
                return 0
                ;;
            "<Primary>space"|"<Control>space")
                printf 'GNOME keybinding %s summary: supported Ctrl+Space\n' "$label"
                return 0
                ;;
            "Caps_Lock")
                printf 'GNOME keybinding %s summary: supported CapsLock\n' "$label"
                return 0
                ;;
        esac
        if [[ "$value" != *space* ]]; then
            case "$value" in
                *Shift*Alt*|*Alt*Shift*)
                    printf 'GNOME keybinding %s summary: supported Alt+Shift\n' "$label"
                    return 0
                    ;;
                *Shift*Control*|*Control*Shift*|*Shift*Ctrl*|*Ctrl*Shift*|*Shift*Primary*|*Primary*Shift*)
                    printf 'GNOME keybinding %s summary: supported Ctrl+Shift\n' "$label"
                    return 0
                    ;;
            esac
        fi
    done

    printf 'GNOME keybinding %s summary: unsupported\n' "$label"
}

openswitcher_wayland_doctor_sources_summary() {
    local raw_sources="$1"
    local raw_mru_sources="$2"
    local source_values=()
    local mru_values=()

    mapfile -t source_values < <(openswitcher_wayland_doctor_quoted_values "$raw_sources")
    mapfile -t mru_values < <(openswitcher_wayland_doctor_quoted_values "$raw_mru_sources")

    if (( ${#source_values[@]} == 0 )); then
        printf '%s\n' "GNOME sources: unavailable"
    elif (( ${#source_values[@]} % 2 != 0 )); then
        printf '%s\n' "GNOME sources: malformed"
    else
        local pair_count=$(( ${#source_values[@]} / 2 ))
        local english_source=""
        local saw_ru=0
        local trusted=1
        local i=0
        while (( i < ${#source_values[@]} )); do
            local source_type="${source_values[$i]}"
            local source_id="${source_values[$((i + 1))]}"
            if [[ "$source_type" != "xkb" ]]; then
                trusted=0
            fi
            case "$source_id" in
                us|gb)
                    if [[ -n "$english_source" ]]; then
                        trusted=0
                    fi
                    english_source="$source_id"
                    ;;
                ru) saw_ru=1 ;;
                *) trusted=0 ;;
            esac
            i=$((i + 2))
        done

        if (( pair_count == 2 && saw_ru == 1 && trusted == 1 )) && [[ -n "$english_source" ]]; then
            printf '%s\n' "GNOME sources: trusted xkb/${english_source}+xkb/ru"
        else
            printf '%s\n' "GNOME sources: untrusted"
        fi
    fi

    if (( ${#mru_values[@]} < 2 )); then
        printf '%s\n' "Current GNOME layout: unknown"
    elif (( ${#mru_values[@]} % 2 != 0 )); then
        printf '%s\n' "Current GNOME layout: malformed"
    else
        local current_type="${mru_values[0]}"
        local current_id="${mru_values[1]}"
        case "$current_type:$current_id" in
            xkb:us|xkb:gb)
                printf '%s\n' "Current GNOME layout: English"
                ;;
            xkb:ru)
                printf '%s\n' "Current GNOME layout: Russian"
                ;;
            *)
                printf '%s\n' "Current GNOME layout: unsupported"
                ;;
        esac
    fi
}

openswitcher_wayland_doctor_uinput_path() {
    local dev_root="${OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT:-/dev}"
    local path=""

    for path in "$dev_root/uinput" "$dev_root/input/uinput"; do
        if [[ -e "$path" ]]; then
            printf '%s\n' "$path"
            return 0
        fi
    done

    printf '%s\n' "$dev_root/uinput"
}

openswitcher_wayland_doctor_uinput_summary() {
    local uinput_path
    uinput_path="$(openswitcher_wayland_doctor_uinput_path)"

    if [[ ! -e "$uinput_path" ]]; then
        printf 'uinput device: not found (%s)\n' "$uinput_path"
        printf '%s\n' "uinput access: unavailable"
        return 0
    fi

    printf 'uinput device: %s\n' "$uinput_path"
    if dd if=/dev/null of="$uinput_path" bs=1 count=0 status=none 2>/dev/null; then
        printf '%s\n' "uinput access: available"
    else
        printf '%s\n' "uinput access: denied"
    fi
}

openswitcher_wayland_doctor() {
    local systemd_env=""
    systemd_env="$(openswitcher_wayland_doctor_systemd_environment || true)"

    local xdg_session_type
    local xdg_current_desktop
    local xdg_session_desktop
    local desktop_session
    local wayland_display
    local display
    local runtime_dir

    xdg_session_type="$(openswitcher_wayland_doctor_effective_env_value XDG_SESSION_TYPE "$systemd_env")"
    xdg_current_desktop="$(openswitcher_wayland_doctor_effective_env_value XDG_CURRENT_DESKTOP "$systemd_env")"
    xdg_session_desktop="$(openswitcher_wayland_doctor_effective_env_value XDG_SESSION_DESKTOP "$systemd_env")"
    desktop_session="$(openswitcher_wayland_doctor_effective_env_value DESKTOP_SESSION "$systemd_env")"
    wayland_display="$(openswitcher_wayland_doctor_effective_env_value WAYLAND_DISPLAY "$systemd_env")"
    display="$(openswitcher_wayland_doctor_effective_env_value DISPLAY "$systemd_env")"
    runtime_dir="$(openswitcher_wayland_doctor_effective_env_value XDG_RUNTIME_DIR "$systemd_env")"

    local socket_path=""
    socket_path="$(openswitcher_wayland_doctor_socket_path "$runtime_dir" "$wayland_display" || true)"
    local socket_live="no"
    [[ -n "$socket_path" && -S "$socket_path" ]] && socket_live="yes"

    local desktop_hint_source="$xdg_current_desktop"
    [[ -n "$desktop_hint_source" ]] || desktop_hint_source="$xdg_session_desktop"
    [[ -n "$desktop_hint_source" ]] || desktop_hint_source="$desktop_session"

    local session_kind
    session_kind="$(openswitcher_wayland_doctor_session_kind "$xdg_session_type" "$display" "$socket_live")"
    local desktop_hint
    desktop_hint="$(openswitcher_wayland_doctor_desktop_hint "$desktop_hint_source")"

    echo "OpenSwitcher Wayland doctor"
    echo
    echo "Session environment:"
    for key in \
        XDG_SESSION_TYPE \
        XDG_CURRENT_DESKTOP \
        XDG_SESSION_DESKTOP \
        DESKTOP_SESSION \
        WAYLAND_DISPLAY \
        DISPLAY \
        XDG_RUNTIME_DIR; do
        openswitcher_wayland_doctor_print_env_row "$key" "$systemd_env"
    done

    echo
    printf 'Session hint: %s\n' \
        "$(openswitcher_wayland_doctor_session_hint "$xdg_session_type" "$display" "$socket_live")"
    printf 'Desktop hint: %s\n' "$desktop_hint"
    if [[ -n "$socket_path" && "$socket_live" == "yes" ]]; then
        printf 'Wayland socket: live (%s)\n' "$socket_path"
    elif [[ -n "$socket_path" ]]; then
        printf 'Wayland socket: missing/non-socket (%s)\n' "$socket_path"
    else
        printf '%s\n' "Wayland socket: unavailable (WAYLAND_DISPLAY unset or missing XDG_RUNTIME_DIR)"
    fi
    if [[ "${xdg_session_type,,}" == "wayland" && -n "$display" ]]; then
        printf '%s\n' "DISPLAY under Wayland: present (normal for XWayland)"
    fi

    echo
    openswitcher_wayland_doctor_print_wayland_support_summary "$session_kind" "$desktop_hint"

    echo
    openswitcher_wayland_doctor_print_tray_acceptance_summary

    if [[ "${desktop_hint,,}" != "gnome" ]]; then
        echo
        printf '%s\n' "GNOME diagnostics: skipped (desktop is not GNOME)"
        echo
        echo "uinput:"
        openswitcher_wayland_doctor_uinput_summary
        return 0
    fi

    echo
    echo "GNOME keybindings:"
    local primary_binding=""
    local backward_binding=""
    primary_binding="$(openswitcher_wayland_doctor_gsettings_get \
        org.gnome.desktop.wm.keybindings switch-input-source || true)"
    backward_binding="$(openswitcher_wayland_doctor_gsettings_get \
        org.gnome.desktop.wm.keybindings switch-input-source-backward || true)"
    printf 'GNOME keybinding primary: %s\n' "${primary_binding:-unavailable}"
    openswitcher_wayland_doctor_keybinding_summary primary "$primary_binding"
    printf 'GNOME keybinding backward: %s\n' "${backward_binding:-unavailable}"
    openswitcher_wayland_doctor_keybinding_summary backward "$backward_binding"

    echo
    echo "GNOME input sources:"
    local sources=""
    local mru_sources=""
    sources="$(openswitcher_wayland_doctor_gsettings_get \
        org.gnome.desktop.input-sources sources || true)"
    mru_sources="$(openswitcher_wayland_doctor_gsettings_get \
        org.gnome.desktop.input-sources mru-sources || true)"
    printf 'GNOME raw sources: %s\n' "${sources:-unavailable}"
    printf 'GNOME raw mru-sources: %s\n' "${mru_sources:-unavailable}"
    openswitcher_wayland_doctor_sources_summary "$sources" "$mru_sources"

    echo
    echo "uinput:"
    openswitcher_wayland_doctor_uinput_summary
}
