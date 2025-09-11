import os
import pandas as pd
import numpy as np
from collections import defaultdict

# Show all columns, no truncation
pd.set_option("display.max_columns", None)
pd.set_option("display.width", None)            # Let pandas choose the width dynamically
pd.set_option("display.max_colwidth", None)     # No truncation of cell contents

def load_measurements(measurements_dir="measurements"):
    """
    Reads all subfolders in `measurements_dir`, each subfolder representing one measurement run.
    
    Returns a nested dict:
      {
        "<measurement_folder>": {
          "<instance_name>": pandas.DataFrame,
          ...
        },
        ...
      }
    """
    # This dictionary will map measurement-folder-name -> { instance_name -> DataFrame }
    measurements_data = {}
    
    # List all items in the top-level measurements folder
    for folder_name in os.listdir(measurements_dir):
        folder_path = os.path.join(measurements_dir, folder_name)
        
        # Skip if not a directory
        if not os.path.isdir(folder_path):
            continue
        
        # Create a dict to hold { instance_name: DataFrame } for this measurement run
        instance_map = {}
        
        # Iterate over all CSV files in this subfolder
        for csv_file in os.listdir(folder_path):
            if not csv_file.endswith(".csv"):
                continue
            
            csv_path = os.path.join(folder_path, csv_file)
            
            # Example csv_file = "metrics_11.0.1.2_3001_server.csv"
            # Remove the "metrics_" prefix and the ".csv" suffix
            if csv_file.startswith("metrics_"):
                instance_name = csv_file[len("metrics_"):-4]  # strip off "metrics_" and ".csv"
            else:
                instance_name = csv_file[:-4]  # fallback if naming changed
                
            # Read the CSV into a DataFrame
            #   - index_col=0 assumes that the first column (often "Unnamed: 0") is the timestamp index
            #   - parse_dates=True attempts to convert that index to datetime
            df = pd.read_csv(csv_path, index_col=0, parse_dates=True)
            
            # Store it in the instance map
            instance_map[instance_name] = df
        
        # Add the instance map to the overall measurements dict
        measurements_data[folder_name] = instance_map
    
    return measurements_data

def filter_known_mode(measurements_data):
    """
    Returns a new dictionary that excludes any instance_name
    containing 'unknown_mode'.

    :param measurements_data: 
        A dictionary of the form:
        {
            "<measurement_folder>": {
                "<instance_name>": pandas.DataFrame,
                ...
            },
            ...
        }
    :return:
        A similarly structured dictionary, but without DataFrames
        whose instance_name has 'unknown_mode'.
    """
    filtered_data = {}

    for folder_name, instance_map in measurements_data.items():
        new_instance_map = {}
        for instance_name, df in instance_map.items():
            # Skip any instance that has "unknown_mode" in its name
            if "unknown_mode" in instance_name:
                continue
            # Otherwise keep it
            new_instance_map[instance_name] = df
        
        filtered_data[folder_name] = new_instance_map
    
    return filtered_data

def trim_dataframes_to_x_minutes(measurements_data, minutes):
    """
    Modifies each DataFrame in the nested measurement dictionary to keep only data 
    from the last `minutes` minutes, relative to the latest timestamp in that DataFrame.
    
    :param measurements_data: Dict of { folder_name: { instance_name: DataFrame } }
    :param minutes: int number of minutes to keep
    """
    for folder_name, instance_map in measurements_data.items():
        for instance_name, df in instance_map.items():
            if df.empty:
                # If the DataFrame is empty, there's nothing to trim
                continue
            
            latest_time = df.index.max()  # max timestamp in this DataFrame
            if pd.isnull(latest_time):
                # If somehow we can't get a valid timestamp (NaT), skip trimming
                continue
            
            cutoff = latest_time - pd.Timedelta(minutes=minutes)
            # Only keep rows whose timestamp index is >= cutoff
            df_limited = df.loc[df.index >= cutoff]
            
            # Replace the DataFrame in the dictionary
            instance_map[instance_name] = df_limited

def compute_bandwidth_for_byte_counters(all_runs):
    """
    For each measurement folder and instance in `all_runs`, 
    finds any columns ending with '_rx_bytes' or '_tx_bytes' and 
    adds a corresponding bandwidth column in Mbps (suffix '_mbps').

    :param all_runs: nested dict of the form
                     {
                       <folder_name>: {
                         <instance_name>: pd.DataFrame,
                         ...
                       },
                       ...
                     }
    :return: the same nested dict, modified in-place with new _mbps columns
    """
    for folder_name, instance_map in all_runs.items():
        for instance_name, df in instance_map.items():
            if df.empty or df.index.size < 2:
                # If the DataFrame is empty or has only 1 row, skip
                continue
            
            # Ensure the DataFrame is sorted by its datetime index
            df.sort_index(inplace=True)
            
            # Compute the time delta in seconds between consecutive rows
            dt_s = df.index.to_series().diff().dt.total_seconds()
            
            # For each column that ends with _rx_bytes or _tx_bytes, compute bandwidth
            for col in df.columns:
                if col.endswith("_rx_bytes") or col.endswith("_tx_bytes"):
                    # Derive the new column name, e.g. "r2_eth0_rx_bytes" -> "r2_eth0_rx_mbps"
                    mbps_col = col.replace("_bytes", "_mbps")
                    
                    # Compute the difference in bytes
                    bytes_diff = df[col].diff()
                    
                    # Convert to Mbps:
                    #   bytes_diff -> bits_diff = bytes_diff * 8
                    #   bits/sec   = bits_diff / dt_s
                    #   Mbps       = (bits_diff / dt_s) / 1e6
                    # We align dt_s by indexing .values, dropping the first row's NaN automatically
                    df[mbps_col] = (bytes_diff * 8 / dt_s) / 1e6

                    # Optionally, you might handle negative rollovers or resets, e.g.:
                    # df.loc[df[mbps_col] < 0, mbps_col] = np.nan
                    
    return all_runs

def compute_statistics_for_columns(measurements_data, stats_to_compute=None):
    """
    Computes statistics (mean, median, p95, etc.) for each column in each measurement folder.
    Ignores columns that start with 'scrape_'.
    
    :param measurements_data: 
        {
          "<measurement_folder>": {
            "<instance_name>": pandas.DataFrame,
            ...
          },
          ...
        }
    :param stats_to_compute: list of (stat_name, stat_func)
        e.g. [
          ("mean", np.mean),
          ("median", np.median),
          ("p95", lambda x: np.percentile(x, 95)),
        ]
    :return: 
        A dict of { column_name: pd.DataFrame }
        where each pd.DataFrame has:
          - index = measurement folder
          - columns = [stat_name for stat_name in stats_to_compute]
    """
    if stats_to_compute is None:
        stats_to_compute = [
            ("mean", np.mean),
            ("median", np.median),
            ("p95", lambda x: np.percentile(x, 95)),
            ("p5",  lambda x: np.percentile(x, 5)),
            ("p99", lambda x: np.percentile(x, 99)),
            ("min", np.min),
            ("max", np.max),
            ("sum", np.sum),
            ("std", np.std),
            ("var", np.var),
            ("iqr", lambda x: np.percentile(x, 75) - np.percentile(x, 25)),
        ]
    
    # data_dict[column][folder] = list of values from all instances in that folder
    data_dict = defaultdict(lambda: defaultdict(list))
    
    # 1) Collect all data values per (folder, column), ignoring columns that start with "scrape_".
    for folder_name, instance_map in measurements_data.items():
        for instance_name, df in instance_map.items():
            # Filter out "scrape_*" columns
            valid_columns = [col for col in df.columns if not col.startswith("scrape_")]
            
            for col in valid_columns:
                # Extend the list with all non-NaN values from this column
                col_values = df[col].dropna().values
                data_dict[col][folder_name].extend(col_values)
    
    # 2) For each column, build a DataFrame of statistics (one row per folder).
    column_stats_map = {}
    
    for col_name, folder_map in data_dict.items():
        rows = []
        # folder_map is { folder_name: [list_of_values] }
        
        for folder_name, values in folder_map.items():
            if len(values) == 0:
                # No data for this folder
                continue
            
            # Compute all requested stats for the collected values
            row_data = {"folder": folder_name}
            for stat_name, stat_func in stats_to_compute:
                try:
                    row_data[stat_name] = float(stat_func(values))
                except Exception:
                    # If stat_func fails, put NaN
                    row_data[stat_name] = float("nan")
            
            rows.append(row_data)
        
        # Convert the list of dicts into a DataFrame
        # We'll set "folder" as the index
        df_stats = pd.DataFrame(rows)
        if not df_stats.empty:
            df_stats.set_index("folder", inplace=True)
            
            # Sort the index (measurement folders) if needed
            df_stats.sort_index(inplace=True)
        
        # Store the DataFrame in the output map
        column_stats_map[col_name] = df_stats
    
    return column_stats_map

def main():
    # 1) Load all measurements from disk
    all_runs = load_measurements("measurements")

    # 2) Filter out any DataFrames with 'unknown_mode' in their instance name
    all_runs = filter_known_mode(all_runs)
    
    # 3) Trim each DataFrame to only the last 4 minutes of data
    trim_dataframes_to_x_minutes(all_runs, 4)

    # 4) Compute bandwidth columns
    compute_bandwidth_for_byte_counters(all_runs)

    # 5) Replace any NaN values with 0
    for folder_name, instance_map in all_runs.items():
        for instance_name, df in instance_map.items():
            # Replace NaN with 0
            df.fillna(0, inplace=True)
    
    # 5) Compute stats, ignoring 'scrape_' columns
    column_stats_map = compute_statistics_for_columns(all_runs)
    
    # Now `all_runs` is structured like:
    # {
    #   "1679923456000": {
    #       "11.0.1.2_3001_server": <DataFrame>,
    #       "11.0.2.2_3380_client": <DataFrame>,
    #       ...
    #   },
    #   "1679927890000": {
    #       "11.0.1.2_3001_server": <DataFrame>,
    #       ...
    #   },
    #   ...
    # }
    
    # Example: print out the shape of each DataFrame
    for measurement_folder, instance_map in all_runs.items():
        print(f"\nMeasurement folder: {measurement_folder}")
        for instance, df in instance_map.items():
            print(f"  Instance: {instance}, DataFrame shape: {df.shape}")
            # print(df.head())  # Uncomment if you want to see the first few rows

    
    metrics_keywords_to_ignore = ["n3_", "r1_", "r2_", "r3_"]
    
    # Or loop over all columns and save them, etc.
    for col_name, df_stats in column_stats_map.items():
        if df_stats.empty:
            continue
        # Filter out columns that contain any of the keywords to ignore
        if any(keyword in col_name for keyword in metrics_keywords_to_ignore):
            continue
        print(f"\nMetric: {col_name}")
        print(df_stats)
        # Optionally save to CSV, etc.
        # df_stats.to_csv(f"stats_{col_name}.csv")

if __name__ == "__main__":
    main()
