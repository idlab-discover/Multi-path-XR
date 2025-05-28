import open3d as o3d
import numpy as np
import os
import pathlib
from tqdm import tqdm
import argparse

# Function to downsample a ply file and save it
def downsample_ply_file(input_file, output_file, percentage=100.0, target_num_pts=None, method="farthest"):
    # Load the point cloud
    pcd = o3d.io.read_point_cloud(input_file)

    current_num_pts = np.asarray(pcd.points).shape[0]
    
    # Calculate the target number of points based on the percentage if target_num_pts is not provided
    if target_num_pts is None:
        target_num_pts = int(percentage / 100.0 * current_num_pts)
    else:
        # Ensure we only downsample, not upsample
        target_num_pts = min(int(target_num_pts), current_num_pts)

    # Downsample using the selected method
    if method == "farthest":
        downpcd = pcd.farthest_point_down_sample(target_num_pts)
    elif method == "random":
        # Calculate the sampling ratio for random downsampling
        sampling_ratio = (target_num_pts * 1.0) / current_num_pts
        downpcd = pcd.random_down_sample(sampling_ratio)
    else:
        raise ValueError(f"Unsupported downsampling method: {method}")

    # Write the downsampled point cloud to file
    o3d.io.write_point_cloud(output_file, downpcd)

# Function to process a directory and downsample ply files
def process_directory(base_dir, percentage=100.0, target_num_pts=None, method="farthest"):
    if percentage is None and target_num_pts is None:
        raise ValueError("Either percentage or target number of points must be provided.")

    # Check if the percentage is valid
    if percentage < 0.0 or percentage > 100.0:
        raise ValueError("Percentage must be between 0 and 100.")

    if target_num_pts is not None and target_num_pts <= 0:
        raise ValueError("Target number of points must be positive.")

    datasets = []

    # Create a list to store the input-output file pairs
    file_pairs = []

    # First collect all the datasets in the base directory that have a 'Ply' folder
    for root, dirs, files in os.walk(base_dir):
        if 'Ply' in dirs:
            datasets.append(root)

    print(f"Found {len(datasets)} datasets with 'Ply' folders.")
        
    # Sort the datasets and iterate over them
    # Here we already know that each dataset has a 'Ply' folder
    # Search for all the ply files and add them to the list
    for dataset in sorted(datasets):
            ply_dir = os.path.join(dataset, 'Ply')

            # Make the output directory path
            output_dir = os.path.join(dataset, f'Ply_pct_{int(percentage)}') if target_num_pts is None else os.path.join(dataset, f'Ply_pts_{int(target_num_pts)}') 

            # Create the output directory if it doesn't exist
            if not os.path.exists(output_dir):
                os.makedirs(output_dir)
            else:
                # Clear the output directory if it already exists
                for file_name in os.listdir(output_dir):
                    file_path = os.path.join(output_dir, file_name)
                    if os.path.isfile(file_path):
                        os.remove(file_path)

            # Sort the ply files in the directory
            # Iterate over each ply file in the Ply folder and add it to the list
            for file_name in sorted(os.listdir(ply_dir)):
                if file_name.endswith(".ply"):
                    input_file = os.path.join(ply_dir, file_name)
                    output_file = os.path.join(output_dir, file_name)
                    file_pairs.append((input_file, output_file))

    # Now process the file pairs with a progress bar
    for input_file, output_file in tqdm(file_pairs, desc="Downsampling PLY files", unit="file"):
        try:
            downsample_ply_file(input_file, output_file, percentage, target_num_pts, method)
        except Exception as e:
            print(f"Error processing {input_file}")
            # Throw the exception to stop the processing
            raise e


def main():
    # Set up argument parser
    parser = argparse.ArgumentParser(description="Downsample PLY files either by percentage or target number of points.")
    
    # Add argument for the directory to process
    parser.add_argument('-d', '--directory', type=str, default=pathlib.Path(__file__).parent.resolve(), help="Root directory to scan for 'Ply' folders.")

    # Add argument for percentage-based downsampling
    parser.add_argument('-p', '--percentage', type=float, default=100.0, help="Percentage of points to keep (default: 100%).")

    # Add argument for downsampling to a target number of points
    parser.add_argument('-n', '--num_points', type=int, default=None, help="Target number of points to downsample to. Overrides percentage if provided.")

    # Add argument for downsampling method
    parser.add_argument('-m', '--method', type=str, choices=["farthest", "random"], default="farthest", help="Downsampling method: 'farthest' (default) or 'random'.")

    # Parse the arguments
    args = parser.parse_args()

    # Determine the root directory (from argument)
    root_directory = pathlib.Path(args.directory).resolve()

    process_directory(root_directory, percentage=args.percentage, target_num_pts=args.num_points, method=args.method)

if __name__ == "__main__":
    main()
