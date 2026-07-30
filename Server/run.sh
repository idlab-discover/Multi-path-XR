#!/bin/bash

# Clear the current screen
clear

# Get the directory of the script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Move to the directory of the script
cd "$SCRIPT_DIR"

# Initialize a new array to hold non-empty arguments
new_args=()
RELEASE_MODE="false"
ENABLE_MOQ="false"

for arg in "$@"; do
    if [[ "$arg" == "--release" ]]; then
        RELEASE_MODE="true"
    elif [[ "$arg" == "--enable-moq" ]]; then
        ENABLE_MOQ="true"
    elif [[ -n "$arg" ]]; then
        trimmed_arg=$(echo "$arg" | sed 's/[[:space:]]*$//')
        new_args+=("$trimmed_arg")
    fi
done

# Overwrite positional parameters with the new arguments
set -- "${new_args[@]}"
ARGS=("$@")

if [[ "$ENABLE_MOQ" == "true" ]]; then
    #echo "Enabling MoQ support..."
    DEFAULT_URL="moqt://127.0.0.1:4443/multipathxr"
    DEFAULT_NAMESPACE="/multipathxr"
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
        if [[ -z "$value" ]]; then
            return
        fi
        if has_flag "$flag"; then
            return
        fi
        ARGS+=("$flag" "$value")
    }

    add_flag "--moq-url" "${DEFAULT_URL}"
    add_flag "--moq-namespace" "${DEFAULT_NAMESPACE}"
    add_flag "--moq-bind" "${DEFAULT_BIND}"

    set -- "${ARGS[@]}"
fi

# Choose debug or release binary
if [[ "$RELEASE_MODE" == "true" ]]; then
    EXECUTABLE="../target/x86_64-unknown-linux-gnu/release/pc-server"
else
    EXECUTABLE="../target/x86_64-unknown-linux-gnu/debug/pc-server"
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
    exec "$EXECUTABLE" "$@"
else
    exec "$EXECUTABLE"
fi

echo "The server has stopped."
