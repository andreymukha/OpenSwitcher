#!/usr/bin/env bash

openswitcher_linux_input_reject_production_overrides() {
    local override_name=""
    local -a override_names=(
        OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT
        OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES
        OPEN_SWITCHER_LINUX_INPUT_RULES_DIR
    )

    for override_name in "${override_names[@]}"; do
        if [[ -v "$override_name" ]] && [[ -n "${!override_name}" ]]; then
            echo "Refusing Linux input production bootstrap: test-only override $override_name is not allowed for production bootstrap." >&2
            return 1
        fi
    done
}

openswitcher_linux_input_dev_root() {
    printf '%s\n' "${OPEN_SWITCHER_LINUX_INPUT_DEV_ROOT:-/dev}"
}

openswitcher_linux_input_proc_devices_path() {
    printf '%s\n' "${OPEN_SWITCHER_LINUX_INPUT_PROC_INPUT_DEVICES:-/proc/bus/input/devices}"
}

openswitcher_linux_input_rule_name() {
    printf '%s\n' "80-openswitcher-input.rules"
}

openswitcher_linux_input_rule_install_path() {
    local rules_dir="${OPEN_SWITCHER_LINUX_INPUT_RULES_DIR:-/etc/udev/rules.d}"
    _openswitcher_linux_input_rule_install_path_for_dir "$rules_dir"
}

_openswitcher_linux_input_rule_install_path_for_dir() {
    local rules_dir="$1"
    printf '%s/%s\n' "$rules_dir" "$(openswitcher_linux_input_rule_name)"
}

_openswitcher_linux_input_add_unique_path() {
    local path="$1"
    local -n target_array_ref="$2"
    [[ -n "$path" ]] || return 0
    local existing
    local candidate_key="$path"
    if [[ -e "$path" ]]; then
        candidate_key="$(_openswitcher_linux_input_realpath "$path")"
    fi
    for existing in "${target_array_ref[@]}"; do
        local existing_key="$existing"
        if [[ -e "$existing" ]]; then
            existing_key="$(_openswitcher_linux_input_realpath "$existing")"
        fi
        [[ "$existing_key" == "$candidate_key" ]] && return 0
    done
    target_array_ref+=("$path")
}

_openswitcher_linux_input_collect_glob_paths() {
    local glob_pattern="$1"
    local target_name="$2"
    local -n target_array_ref="$2"
    local path
    shopt -s nullglob
    for path in $glob_pattern; do
        _openswitcher_linux_input_add_unique_path "$path" "$target_name"
    done
    shopt -u nullglob
}

_openswitcher_linux_input_emit_proc_paths() {
    local mode="$1"
    local dev_root
    dev_root="$(openswitcher_linux_input_dev_root)"
    local proc_devices
    proc_devices="$(openswitcher_linux_input_proc_devices_path)"

    _openswitcher_linux_input_emit_proc_paths_with_paths \
        "$mode" "$dev_root" "$proc_devices"
}

_openswitcher_linux_input_emit_proc_paths_with_paths() {
    local mode="$1"
    local dev_root="$2"
    local proc_devices="$3"

    [[ -r "$proc_devices" ]] || return 0

    local current_name=""
    local current_handlers=""
    local line=""

    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ -z "$line" ]]; then
            _openswitcher_linux_input_emit_proc_paths_from_block \
                "$mode" "$dev_root" "$current_name" "$current_handlers"
            current_name=""
            current_handlers=""
            continue
        fi

        case "$line" in
            N:\ Name=*)
                current_name="${line#N: Name=\"}"
                current_name="${current_name%\"}"
                ;;
            H:\ Handlers=*)
                current_handlers="${line#H: Handlers=}"
                ;;
        esac
    done <"$proc_devices"

    _openswitcher_linux_input_emit_proc_paths_from_block \
        "$mode" "$dev_root" "$current_name" "$current_handlers"
}

_openswitcher_linux_input_emit_proc_paths_from_block() {
    local mode="$1"
    local dev_root="$2"
    local current_name="$3"
    local current_handlers="$4"

    [[ -n "$current_handlers" ]] || return 0

    local event_name=""
    local handler=""
    for handler in $current_handlers; do
        if [[ "$handler" == event* ]]; then
            event_name="$handler"
            break
        fi
    done

    [[ -n "$event_name" ]] || return 0

    local lower_name="${current_name,,}"
    case "$mode" in
        keyboard)
            if [[ "$current_handlers" == *"sysrq"* ]] || [[ "$lower_name" == *"keyboard"* ]]; then
                printf '%s/input/%s\n' "$dev_root" "$event_name"
            fi
            ;;
        pointer)
            if [[ "$current_handlers" == *"mouse"* ]]; then
                printf '%s/input/%s\n' "$dev_root" "$event_name"
            fi
            ;;
    esac
}

openswitcher_linux_input_collect_keyboard_candidates() {
    local dev_root
    dev_root="$(openswitcher_linux_input_dev_root)"
    local proc_devices
    proc_devices="$(openswitcher_linux_input_proc_devices_path)"

    _openswitcher_linux_input_collect_keyboard_candidates_with_paths \
        "$dev_root" "$proc_devices"
}

_openswitcher_linux_input_collect_keyboard_candidates_with_paths() {
    local dev_root="$1"
    local proc_devices="$2"

    local candidates=()
    _openswitcher_linux_input_collect_glob_paths \
        "$dev_root/input/by-path/*-event-kbd" candidates
    _openswitcher_linux_input_collect_glob_paths \
        "$dev_root/input/by-id/*-event-kbd" candidates

    local proc_path=""
    while IFS= read -r proc_path; do
        _openswitcher_linux_input_add_unique_path "$proc_path" candidates
    done < <(_openswitcher_linux_input_emit_proc_paths_with_paths \
        keyboard "$dev_root" "$proc_devices")

    printf '%s\n' "${candidates[@]}"
}

openswitcher_linux_input_collect_pointer_candidates() {
    local dev_root
    dev_root="$(openswitcher_linux_input_dev_root)"
    local proc_devices
    proc_devices="$(openswitcher_linux_input_proc_devices_path)"

    _openswitcher_linux_input_collect_pointer_candidates_with_paths \
        "$dev_root" "$proc_devices"
}

_openswitcher_linux_input_collect_pointer_candidates_with_paths() {
    local dev_root="$1"
    local proc_devices="$2"

    local candidates=()
    _openswitcher_linux_input_collect_glob_paths \
        "$dev_root/input/by-path/*-event-mouse" candidates
    _openswitcher_linux_input_collect_glob_paths \
        "$dev_root/input/by-id/*-event-mouse" candidates

    local proc_path=""
    while IFS= read -r proc_path; do
        _openswitcher_linux_input_add_unique_path "$proc_path" candidates
    done < <(_openswitcher_linux_input_emit_proc_paths_with_paths \
        pointer "$dev_root" "$proc_devices")

    printf '%s\n' "${candidates[@]}"
}

openswitcher_linux_input_find_uinput_path() {
    local dev_root
    dev_root="$(openswitcher_linux_input_dev_root)"

    _openswitcher_linux_input_find_uinput_path_with_dev_root "$dev_root"
}

_openswitcher_linux_input_find_uinput_path_with_dev_root() {
    local dev_root="$1"

    local path
    for path in "$dev_root/uinput" "$dev_root/input/uinput"; do
        if [[ -e "$path" ]]; then
            printf '%s\n' "$path"
            return 0
        fi
    done

    printf '%s\n' "$dev_root/uinput"
}

_openswitcher_linux_input_can_open_read() {
    local path="$1"
    [[ -e "$path" ]] || return 1

    dd if="$path" of=/dev/null bs=1 count=0 status=none 2>/dev/null
}

_openswitcher_linux_input_can_open_write() {
    local path="$1"
    [[ -e "$path" ]] || return 1

    dd if=/dev/null of="$path" bs=1 count=0 status=none 2>/dev/null
}

_openswitcher_linux_input_print_path_list() {
    local -n paths_ref="$1"
    local path=""
    local first=1
    for path in "${paths_ref[@]}"; do
        if [[ "$first" -eq 1 ]]; then
            printf '%s' "$path"
            first=0
        else
            printf ', %s' "$path"
        fi
    done
}

openswitcher_linux_input_doctor() {
    local keyboard_candidates=()
    local keyboard_path=""
    while IFS= read -r keyboard_path; do
        [[ -n "$keyboard_path" ]] || continue
        keyboard_candidates+=("$keyboard_path")
    done < <(openswitcher_linux_input_collect_keyboard_candidates)

    local pointer_candidates=()
    local pointer_path=""
    while IFS= read -r pointer_path; do
        [[ -n "$pointer_path" ]] || continue
        pointer_candidates+=("$pointer_path")
    done < <(openswitcher_linux_input_collect_pointer_candidates)

    local uinput_path
    uinput_path="$(openswitcher_linux_input_find_uinput_path)"

    local ready=0
    local keyboard_status=""
    local pointer_status=""
    local uinput_status=""
    local pointer_denied_paths=()
    local pointer_available_count=0
    local pointer_total="${#pointer_candidates[@]}"
    local keyboard_detected_path=""

    echo "OpenSwitcher Linux input doctor"

    if [[ "${#keyboard_candidates[@]}" -eq 0 ]]; then
        keyboard_status="not-found"
        echo "Keyboard device: not found"
        echo "Keyboard access: unavailable"
    else
        keyboard_detected_path="${keyboard_candidates[0]}"
        echo "Keyboard device detected: $keyboard_detected_path"
        if _openswitcher_linux_input_can_open_read "$keyboard_detected_path"; then
            keyboard_status="available"
            echo "Keyboard access: available"
        else
            keyboard_status="denied"
            echo "Keyboard access: denied"
        fi
    fi

    if [[ "$pointer_total" -eq 0 ]]; then
        pointer_status="not-detected"
        echo "Pointer devices: none detected"
        echo "Pointer access: not required"
    else
        echo -n "Pointer devices detected: "
        _openswitcher_linux_input_print_path_list pointer_candidates
        printf '\n'

        for pointer_path in "${pointer_candidates[@]}"; do
            if _openswitcher_linux_input_can_open_read "$pointer_path"; then
                pointer_available_count=$((pointer_available_count + 1))
            else
                pointer_denied_paths+=("$pointer_path")
            fi
        done

        if [[ "${#pointer_denied_paths[@]}" -eq 0 ]]; then
            pointer_status="available"
            echo "Pointer access: available"
        else
            pointer_status="denied"
            echo -n "Pointer access: denied for "
            _openswitcher_linux_input_print_path_list pointer_denied_paths
            printf '\n'
        fi
    fi

    if [[ ! -e "$uinput_path" ]]; then
        uinput_status="not-found"
        echo "uinput device: not found ($uinput_path)"
        echo "uinput access: unavailable"
    else
        echo "uinput device: $uinput_path"
        if _openswitcher_linux_input_can_open_write "$uinput_path"; then
            uinput_status="available"
            echo "uinput access: available"
        else
            uinput_status="denied"
            echo "uinput access: denied"
        fi
    fi

    if [[ "$keyboard_status" == "available" ]] &&
        [[ "$uinput_status" == "available" ]] &&
        [[ "$pointer_status" != "denied" ]]; then
        ready=1
    fi

    if [[ "$ready" -eq 1 ]]; then
        echo "Result: Linux input setup is ready."
        return 0
    fi

    echo "Result: Linux input setup is not ready."

    if [[ "$keyboard_status" == "not-found" ]]; then
        echo 'Run `./manage.sh doctor` after connecting the keyboard device or fixing the Linux input setup.'
    else
        echo 'Run `./manage.sh bootstrap linux-input` to install the required Linux input setup.'
    fi

    return 1
}

_openswitcher_linux_input_realpath() {
    local path="$1"
    if command -v readlink >/dev/null 2>&1; then
        readlink -f "$path" 2>/dev/null || printf '%s\n' "$path"
    else
        printf '%s\n' "$path"
    fi
}

openswitcher_linux_input_apply_session_acl() {
    local target_user="$1"
    shift

    local path=""
    local resolved_path=""
    local seen=()
    for path in "$@"; do
        [[ -n "$path" ]] || continue
        [[ -e "$path" ]] || continue
        resolved_path="$(_openswitcher_linux_input_realpath "$path")"
        _openswitcher_linux_input_add_unique_path "$resolved_path" seen
    done

    if [[ "${#seen[@]}" -eq 0 ]]; then
        return 0
    fi

    local acl_path=""
    for acl_path in "${seen[@]}"; do
        setfacl -m "u:${target_user}:rw" "$acl_path"
    done
}

_openswitcher_linux_input_bootstrap_with_paths() {
    local repo_root="$1"
    local target_user="$2"
    local dev_root="$3"
    local proc_devices="$4"
    local rules_dir="$5"

    local rule_source="$repo_root/dist/udev/$(openswitcher_linux_input_rule_name)"
    local rule_target
    rule_target="$(_openswitcher_linux_input_rule_install_path_for_dir "$rules_dir")"

    if [[ ! -f "$rule_source" ]]; then
        echo "Linux input bootstrap asset not found: $rule_source" >&2
        return 1
    fi

    mkdir -p "$(dirname "$rule_target")"
    install -m 0644 "$rule_source" "$rule_target"
    echo "Installed udev rule: $rule_target"

    if command -v udevadm >/dev/null 2>&1; then
        udevadm control --reload-rules
        udevadm trigger --subsystem-match=input --action=change || true
        udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change || true
        echo "Reloaded udev rules and triggered input devices."
    else
        echo "udevadm not found; permanent rule was installed but live udev reload was skipped."
    fi

    if command -v setfacl >/dev/null 2>&1; then
        local acl_paths=()
        local path=""
        while IFS= read -r path; do
            [[ -n "$path" ]] || continue
            acl_paths+=("$path")
        done < <(_openswitcher_linux_input_collect_keyboard_candidates_with_paths \
            "$dev_root" "$proc_devices")
        while IFS= read -r path; do
            [[ -n "$path" ]] || continue
            acl_paths+=("$path")
        done < <(_openswitcher_linux_input_collect_pointer_candidates_with_paths \
            "$dev_root" "$proc_devices")
        acl_paths+=("$(_openswitcher_linux_input_find_uinput_path_with_dev_root \
            "$dev_root")")

        openswitcher_linux_input_apply_session_acl "$target_user" "${acl_paths[@]}"
        echo "Applied same-session ACL bridge for user: $target_user"
    else
        echo "setfacl not found; same-session ACL bridge was skipped."
    fi
}

openswitcher_linux_input_bootstrap_test() {
    local repo_root="$1"
    local target_user="$2"
    local dev_root="$3"
    local proc_devices="$4"
    local rules_dir="$5"

    _openswitcher_linux_input_bootstrap_with_paths \
        "$repo_root" "$target_user" "$dev_root" "$proc_devices" "$rules_dir"
}

openswitcher_linux_input_bootstrap_root() {
    local repo_root="$1"
    local target_user="$2"

    openswitcher_linux_input_reject_production_overrides || return 1

    _openswitcher_linux_input_bootstrap_with_paths \
        "$repo_root" \
        "$target_user" \
        /dev \
        /proc/bus/input/devices \
        /etc/udev/rules.d
}
