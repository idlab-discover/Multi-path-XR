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


# Choose debug or release binary
if [[ "$RELEASE_MODE" == "true" ]]; then
    EXECUTABLE="../target/x86_64-unknown-linux-gnu/release/pc-agent"
else
    EXECUTABLE="../target/x86_64-unknown-linux-gnu/debug/pc-agent"
fi
# Check if the file exists
if [[ ! -x "$EXECUTABLE" ]]; then
    echo "Error: Executable $EXECUTABLE not found or is not executable."
    exit 1
fi

# Execute the target executable and pass all arguments
if [[ $# -gt 0 ]]; then
    # Echo all arguments
    echo "Running $EXECUTABLE with arguments: $@"
    # And log all output to a file
    "$EXECUTABLE" "$@" #> agent.log 2>&1
else
    "$EXECUTABLE" #> agent.log 2>&1
fi

echo "The agent has stopped."