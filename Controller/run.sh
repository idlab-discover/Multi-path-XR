#!/bin/bash

# Clear the current screen
clear

# Initialize a new array to hold non-empty arguments
new_args=()
RELEASE_MODE="false"

for arg in "$@"; do
    # Check if this argument is --release
    if [[ "$arg" == "--release" ]]; then
        RELEASE_MODE="true"
        # Skip pushing this argument into new_args,
        # so it won't be passed to the final executable.
    elif [[ -n "$arg" ]]; then # Add non-empty arguments to the new array
        # Trim trailing spaces and add to the new array
        trimmed_arg=$(echo "$arg" | sed 's/[[:space:]]*$//')
        new_args+=("$trimmed_arg")
    fi
done

# Overwrite positional parameters with the new arguments
set -- "${new_args[@]}"

# Get the directory of the script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Move to the directory of the script
cd "$SCRIPT_DIR"


# Choose the newest matching debug or release binary. This keeps manual
# `cargo build --release` and the repo build scripts compatible.
if [[ "$RELEASE_MODE" == "true" ]]; then
    candidates=(
        "../target/x86_64-unknown-linux-gnu/release/pc-controller"
        "../target/release/pc-controller"
    )
else
    candidates=(
        "../target/x86_64-unknown-linux-gnu/debug/pc-controller"
        "../target/debug/pc-controller"
    )
fi

EXECUTABLE=""
EXECUTABLE_MTIME=-1

for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate" ]]; then
        mtime=$(stat -c %Y "$candidate")
        if (( mtime > EXECUTABLE_MTIME )); then
            EXECUTABLE="$candidate"
            EXECUTABLE_MTIME=$mtime
        fi
    fi
done

# Check if a matching executable exists
if [[ -z "$EXECUTABLE" ]]; then
    echo "Error: Executable not found. Checked: ${candidates[*]}"
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

echo "The controller has stopped."
