#!/usr/bin/env bash
# Source from disposable-container checks before producing evidence or release files.
check_outputs=()
check_output_directories=()
check_output_root=$PWD
check_temporary_home=$(mktemp -d)
export HOME="$check_temporary_home"

check_output() {
    local output parent
    for output in "$@"; do
        if [[ "$output" != /* ]]; then output="$check_output_root/$output"; fi
        check_outputs+=("$output")
        parent=$(dirname "$output")
        while [ "$parent" != "$check_output_root" ] && [ "$parent" != / ]; do
            check_output_directories+=("$parent")
            if [ -d "$parent" ]; then break; fi
            parent=$(dirname "$parent")
        done
    done
}

check_output_cleanup() {
    local result=$? path
    rm -rf "$check_temporary_home"
    if [ "$(id -u)" = 0 ] && [ -n "${CHECK_UID:-}" ]; then
        for path in "${check_outputs[@]}"; do
            if [ -e "$path" ]; then chown -R "$CHECK_UID:$CHECK_GID" "$path"; fi
        done
        for path in "${check_output_directories[@]}"; do
            if [ -d "$path" ]; then chown "$CHECK_UID:$CHECK_GID" "$path"; fi
        done
    fi
    return "$result"
}
trap check_output_cleanup EXIT
