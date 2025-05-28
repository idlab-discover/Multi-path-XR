#!/bin/bash

# Usage: ./monitoring.sh [start|stop|pause|resume|reload|status]

# Move to the directory of this script
cd "$(dirname "$0")"

COMPOSE_FILE="docker-compose.yml"
PROMETHEUS_CONTAINER="prometheus"

case "$1" in
  start)
    echo "Starting Prometheus and Grafana..."
    docker compose -f $COMPOSE_FILE up -d
    ;;
  stop)
    echo "Stopping Prometheus and Grafana..."
    docker compose -f $COMPOSE_FILE down
    ;;
  pause)
    echo "Pausing Prometheus..."
    docker compose -f $COMPOSE_FILE stop prometheus
    ;;
  resume)
    echo "Resuming Prometheus..."
    docker compose -f $COMPOSE_FILE start prometheus
    ;;
  reload)
    echo "Reloading Prometheus configuration..."
    docker exec $PROMETHEUS_CONTAINER kill -HUP 1
    ;;
  status)
    echo "Checking status of Prometheus and Grafana..."
    docker compose -f $COMPOSE_FILE ps
    ;;
  *)
    echo "Usage: $0 [start|stop|pause|resume|reload|status]"
    exit 1
    ;;
esac
