#!/bin/bash

# Clear the current screen
clear

# Initialize the headless flag to false
HEADLESS=false
ENABLE_MOQ="false"

# Remember where the project lives
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Initialize a new array to hold non-empty arguments
new_args=()
RELEASE_MODE="false"

for arg in "$@"; do
    case "$arg" in
        --headless)
            HEADLESS=true
            ;;
        --release)
            RELEASE_MODE="true"
            ;;
        --enable-moq)
            ENABLE_MOQ="true"
            ;;
        *)
            if [[ -n "$arg" ]]; then
                trimmed_arg=$(echo "$arg" | sed 's/[[:space:]]*$//')
                new_args+=("$trimmed_arg")
            fi
            ;;
    esac
done

# Overwrite positional parameters with the new arguments
set -- "${new_args[@]}"
ARGS=("$@")

if [[ "$ENABLE_MOQ" == "true" ]]; then
    DEFAULT_URL="moqt://127.0.0.1:4443/multipathxr"
    DEFAULT_NAMESPACE="multipathxr"
    DEFAULT_BIND="[::]:0"

    has_flag() {
        local flag="$1"
        for candidate in "${ARGS[@]}"; do
            if [[ "$candidate" == "$flag" ]]; then
                return 0
            fi
        done
        return 1
    }

    add_flag() {
        local flag="$1"
        local value="$2"
        if [[ -z "$value" ]] || has_flag "$flag"; then
            return
        fi
        ARGS+=("$flag" "$value")
    }

    add_flag "--moq-url" "${DEFAULT_URL}"
    add_flag "--moq-namespace" "${DEFAULT_NAMESPACE}"
    add_flag "--moq-bind" "${DEFAULT_BIND}"

    set -- "${ARGS[@]}"
fi

# Move to the directory of the script
cd "$SCRIPT_DIR"

# Execute pc-receiver with or without the headless flag
if [[ "$HEADLESS" == true ]]; then
    # Choose debug or release binary
    if [[ "$RELEASE_MODE" == "true" ]]; then
        EXECUTABLE="../target/x86_64-unknown-linux-gnu/release/pc-receiver"
    else
        EXECUTABLE="../target/x86_64-unknown-linux-gnu/debug/pc-receiver"
    fi
    # Check if the file exists
    if [[ ! -f "$EXECUTABLE" ]]; then
        echo "Error: Executable $EXECUTABLE not found."
        exit 1
    fi

    # Make it executable if it is not
    if [[ ! -x "$EXECUTABLE" ]]; then
        chmod +x "$EXECUTABLE"
    fi

    # Execute the target executable and pass all arguments
    if [[ $# -gt 0 ]]; then
        echo "$EXECUTABLE" "$@"
        exec "$EXECUTABLE" "$@"
    else
        exec "$EXECUTABLE"
    fi

else
    # Choose debug or release binary
    if [[ "$RELEASE_MODE" == "true" ]]; then
        EXECUTABLE="./pc_renderer_unity/Build/release.x86_64"
    else
        EXECUTABLE="./pc_renderer_unity/Build/debug.x86_64"
    fi
    # Check if the executable exists
    if [[ ! -f "$EXECUTABLE" ]]; then
        echo "Error: Executable $EXECUTABLE not found."
        exit 1
    fi

    # Make it executable if it is not
    if [[ ! -x "$EXECUTABLE" ]]; then
        chmod +x "$EXECUTABLE"
    fi

    # Execute the target executable and pass all arguments
    if [[ $# -gt 0 ]]; then
        echo "$EXECUTABLE" "$@" -force-vulkan -logfile -
        exec "$EXECUTABLE" "$@" -force-vulkan -logfile -
    else
        exec "$EXECUTABLE" -force-vulkan
    fi
fi


echo "The client has stopped."
