#!/bin/bash

# Execute the target executable and pass all arguments
EXECUTABLE="src/app.py"

# Get the current process ID and command line arguments
CURRENT_PID=$$
CURRENT_CMDLINE=$(tr '\0' ' ' < /proc/$CURRENT_PID/cmdline)

# Kill duplicate processes
ps -eo pid,cmd --no-headers | while read -r PID CMDLINE; do
    if [[ "$PID" == "$CURRENT_PID" || -z "$CMDLINE" ]]; then
        continue
    fi
    if [[ "$CMDLINE" == *"python3 $EXECUTABLE"* ]]; then
        echo "Killing previous app process $PID"
        if ! kill -9 "$PID" 2>/dev/null; then
            echo "Failed to kill PID $PID. Process may have already exited."
        fi
    fi
done


# Default behavior is to clear the screen
CLEAR_SCREEN=true

# Parse arguments for --no-clear
for arg in "$@"; do
    if [[ "$arg" == "--no-clear" ]]; then
        CLEAR_SCREEN=false
        # Remove --no-clear from the arguments passed to the Python script
        set -- "${@/--no-clear/}"
        break
    fi
done

# Clear the current screen if CLEAR_SCREEN is true
if [[ "$CLEAR_SCREEN" == true ]]; then
    clear
fi

# Initialize a new array to hold non-empty arguments
new_args=()

for arg in "$@"; do
    # Add non-empty arguments to the new array
    if [[ -n "$arg" ]]; then
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


if [[ $# -gt 0 ]]; then
    python3 "$EXECUTABLE" "$@"
else
    python3 "$EXECUTABLE"
fi


# echo "Mininet has stopped."