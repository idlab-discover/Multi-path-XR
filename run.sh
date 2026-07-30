#!/bin/bash

# Check if at least one argument is provided
if [[ $# -lt 1 ]]; then
    echo "Error: You must specify the node the first argument."
    exit 1
fi

# Extract the first argument to check if it's --client or --server
MODE=$1
shift  # Remove the first argument so the rest can be passed along to the respective script


# Get the directory of the script
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Move to the directory of the script
cd "$SCRIPT_DIR"

# Source the virtual environment
VENV_DIR="$SCRIPT_DIR/.venv" # Adjust this to point to your virtual environment directory
# If the venv_dir exists and has the activate script, source it
if [[ -f "$VENV_DIR/bin/activate" ]]; then
    source "$VENV_DIR/bin/activate"
fi

# Enable Rust backtrace
export RUST_BACKTRACE=full

# Determine the script to execute based on the first argument
if [[ "$MODE" == "--client" ]]; then
    # Check if the client script exists
    CLIENT_SCRIPT="./Client/run.sh"
    if [[ -f "$CLIENT_SCRIPT" && -x "$CLIENT_SCRIPT" ]]; then
        exec "$CLIENT_SCRIPT" "$@"
    else
        echo "Error: Client script not found or not executable at $CLIENT_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--server" ]]; then
    # Check if the server script exists
    SERVER_SCRIPT="./Server/run.sh"
    if [[ -f "$SERVER_SCRIPT" && -x "$SERVER_SCRIPT" ]]; then
        exec "$SERVER_SCRIPT" "$@"
    else
        echo "Error: Server script not found or not executable at $SERVER_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--proxy" ]]; then
    # Check if the proxy script exists
    PROXY_SCRIPT="./Proxy/CDN/run.sh"
    if [[ -f "$PROXY_SCRIPT" && -x "$PROXY_SCRIPT" ]]; then
        exec "$PROXY_SCRIPT" "$@"
    else
        echo "Error: Proxy script not found or not executable at $PROXY_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--metrics" ]]; then
    # Check if the metrics script exists
    METRICS_SCRIPT="./Libraries/Metrics/run.sh"
    if [[ -f "$METRICS_SCRIPT" && -x "$METRICS_SCRIPT" ]]; then
        exec "$METRICS_SCRIPT" "$@"
    else
        echo "Error: Metrics script not found or not executable at $METRICS_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--draco" ]]; then
    # Check if the draco script exists
    DRACO_SCRIPT="./Libraries/Draco/run.sh"
    if [[ -f "$DRACO_SCRIPT" && -x "$DRACO_SCRIPT" ]]; then
        exec "$DRACO_SCRIPT" "$@"
    else
        echo "Error: Draco script not found or not executable at $DRACO_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--controller" ]]; then
    # Check if the controller script exists
    CONTROLLER_SCRIPT="./Controller/run.sh"
    if [[ -f "$CONTROLLER_SCRIPT" && -x "$CONTROLLER_SCRIPT" ]]; then
        exec "$CONTROLLER_SCRIPT" "$@"
    else
        echo "Error: Controller script not found or not executable at $CONTROLLER_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--agent" ]]; then
    # Check if the agent script exists
    AGENT_SCRIPT="./Agent/run.sh"
    if [[ -f "$AGENT_SCRIPT" && -x "$AGENT_SCRIPT" ]]; then
        exec "$AGENT_SCRIPT" "$@"
    else
        echo "Error: Agent script not found or not executable at $AGENT_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--mininet" ]]; then
    # Check if the mininet script exists
    MININET_SCRIPT="./Environments/Mininet/run.sh"
    if [[ -f "$MININET_SCRIPT" && -x "$MININET_SCRIPT" ]]; then
        exec "$MININET_SCRIPT" "$@"
    else
        echo "Error: Mininet script not found or not executable at $MININET_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--vw" ]]; then
    # Check if the vw script exists
    VW_SCRIPT="./Environments/VirtualWall/scripts/vw.sh"
    if [[ -f "$VW_SCRIPT" && -x "$VW_SCRIPT" ]]; then
        exec "$VW_SCRIPT" "$@"
    else
        echo "Error: Virtual Wall script not found or not executable at $VW_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--monitoring" ]]; then
    # Check if the monitoring script exists
    MONITORING_SCRIPT="./Controller/metrics/monitoring.sh"
    if [[ -f "$MONITORING_SCRIPT" && -x "$MONITORING_SCRIPT" ]]; then
        exec "$MONITORING_SCRIPT" "$@"
    else
        echo "Error: Monitoring script not found or not executable at $MONITORING_SCRIPT."
        exit 1
    fi
elif [[ "$MODE" == "--update-targets" ]]; then
    # Check if the update targets script exists
    UPDATE_TARGETS_SCRIPT="./Controller/metrics/update_targets.sh"
    if [[ -f "$UPDATE_TARGETS_SCRIPT" && -x "$UPDATE_TARGETS_SCRIPT" ]]; then
        exec "$UPDATE_TARGETS_SCRIPT" "$@"
    else
        echo "Error: Update targets script not found or not executable at $UPDATE_TARGETS_SCRIPT."
        exit 1
    fi
else
    echo "Error: Invalid argument."
    exit 1
fi
