#!/bin/bash

# Move to the directory of this script
cd "$(dirname "$0")"

# File path to Prometheus targets.json
TARGETS_FILE="./prometheus_data/targets.json"

# Ensure the targets file exists
if [ ! -f "$TARGETS_FILE" ]; then
  echo "[]" > "$TARGETS_FILE"
fi

case "$1" in
  set)
    if [ -z "$2" ]; then
      echo "Error: Please provide a comma-separated list of targets."
      echo "Usage: $0 set 'target1:port,target2:port,...'"
      exit 1
    fi

    # Split the comma-separated targets into an array
    IFS=',' read -ra TARGETS <<< "$2"

    # Convert the array into the required JSON format
    TARGETS_ARRAY=()
    for TARGET in "${TARGETS[@]}"; do
      TARGETS_ARRAY+=("{
        \"targets\": [\"$TARGET\"],
        \"labels\": {
          \"job\": \"node\"
        }
      }")
    done

    # Join the array into a JSON array
    TARGETS_JSON=$(echo "${TARGETS_ARRAY[@]}" | jq -s .)

    # Write the JSON to the targets file
    echo "$TARGETS_JSON" > "$TARGETS_FILE"
    echo "Targets set to:"
    echo "$TARGETS_JSON"
    ;;

  clear)
    # Clear all targets
    echo "[]" > "$TARGETS_FILE"
    echo "All targets cleared."
    ;;

  *)
    echo "Usage: $0 [set 'target1:port,target2:port,...' | clear]"
    exit 1
    ;;
esac