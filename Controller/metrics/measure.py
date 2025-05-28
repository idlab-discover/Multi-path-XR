import time
import os
import requests
import pandas as pd
from datetime import datetime

PROMETHEUS_URL = "http://0.0.0.0:9090"

def fetch_all_metrics():
    """
    Fetch the list of all metric names from Prometheus.
    """
    url = f"{PROMETHEUS_URL}/api/v1/label/__name__/values"
    resp = requests.get(url)
    resp.raise_for_status()
    data = resp.json()
    # data["data"] is the list of metric names
    return data.get("data", [])

def query_metric(metric_name):
    """
    Query the current value of a single metric from Prometheus.
    Returns a list of results (each result has 'metric' and 'value' keys).
    """
    url = f"{PROMETHEUS_URL}/api/v1/query"
    params = {"query": metric_name}
    resp = requests.get(url, params=params)
    resp.raise_for_status()
    data = resp.json()
    # data["data"]["result"] is a list of { "metric": {...}, "value": [timestamp, value] }
    return data["data"].get("result", [])

def main():
    # Get current time in milliseconds since epoch
    start_time_ms = int(time.time() * 1000)
    
    # Create subfolder measurements/<start_time_ms>
    folder_path = os.path.join("measurements", str(start_time_ms))
    os.makedirs(folder_path, exist_ok=True)
    
    print(f"Saving CSV files to: {folder_path}")

    # Dictionary to hold DataFrames keyed by instance
    instance_dfs = {}
    
    # Get all metric names once at startup (you could refresh occasionally if needed)
    all_metrics = fetch_all_metrics()
    print(f"Found {len(all_metrics)} metrics total.")

    # If you only want certain metrics, filter them here
    # all_metrics = [m for m in all_metrics if m.startswith("cpu_") or m in ("up", "memory_usage")]

    while True:
        timestamp = datetime.now()

        # Prepare a dict to hold metric -> {instance -> value} for this timestep
        step_data = {}

        for metric_name in all_metrics:
            results = query_metric(metric_name)
            for res in results:
                # Each 'res' is like:
                # {
                #   "metric": {"__name__": "cpu_usage", "instance": "11.12.1.2:8080", ...},
                #   "value": [ <timestamp>, <value_string> ]
                # }
                metric_info = res["metric"]
                value_arr = res["value"]
                
                instance_name = metric_info.get("instance", "unknown_instance")
                mode_name = metric_info.get("mode", "unknown_mode")
                instance_name = f"{instance_name}_{mode_name}"  # Combine instance and mode for uniqueness
                # Value array is [unix_timestamp, value_as_string]
                value = float(value_arr[1])  # Convert to float

                if instance_name not in step_data:
                    step_data[instance_name] = {}
                
                # Assign this metric's value for this instance
                step_data[instance_name][metric_name] = value
        
        # Insert a row into each instance’s DataFrame
        for instance_name, metrics_dict in step_data.items():
            if instance_name not in instance_dfs:
                instance_dfs[instance_name] = pd.DataFrame()

            # Create a single-row DataFrame for the new data
            row_df = pd.DataFrame([metrics_dict], index=[timestamp])

            # Append the row to the instance DataFrame (in memory),
            # but only keep the last 5 rows
            instance_dfs[instance_name] = pd.concat(
                [instance_dfs[instance_name], row_df]
            ).tail(5)

            # Write this single new row to CSV (append mode)
            csv_name = f"metrics_{instance_name.replace(':','_')}.csv"
            csv_path = os.path.join(folder_path, csv_name)
            file_exists = os.path.isfile(csv_path)
            
            row_df.to_csv(
                csv_path,
                mode='a',
                header=not file_exists
            )

        print(f"[{timestamp}] Updated {len(step_data)} instances.")

        # Sleep for 1 second before next iteration
        time.sleep(1)

if __name__ == "__main__":
    main()
