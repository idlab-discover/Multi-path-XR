import os
import platform
import difflib
import argparse


def clear_screen():
    """Clear the terminal screen."""
    if platform.system() == "Windows":
        os.system("cls")
    else:
        os.system("clear")


def suggest_directories(base_dir):
    """
    Suggest similar directories if the provided directory is not found.

    :param base_dir: The directory that was not found.
    :return: A list of suggested directories.
    """
    parent_dir = os.path.dirname(base_dir) or "."
    if os.path.exists(parent_dir):
        all_dirs = [d for d in os.listdir(parent_dir) if os.path.isdir(os.path.join(parent_dir, d))]
        suggestions = difflib.get_close_matches(os.path.basename(base_dir), all_dirs, n=3)
        return [os.path.join(parent_dir, s) for s in suggestions]
    return []


def list_files_with_content(base_dir, file_extension=None, max_content_length=1024):
    """
    List all files in a directory and its subdirectories, sorted and grouped by directory.
    Prints the content of each file.

    :param base_dir: The base directory to search for files.
    :param file_extension: File extension to filter (e.g., '.txt'), or None to include all files.
    :param max_content_length: Maximum length of content to print per file (to prevent excessive output).
    """
    clear_screen()  # Clear the screen at the start

    if not os.path.exists(base_dir):
        print(f"Error: Directory '{base_dir}' not found.")
        suggestions = suggest_directories(base_dir)
        if suggestions:
            print("\nDid you mean:")
            for suggestion in suggestions:
                print(f"  - {suggestion}")
        else:
            print("\nNo similar directories found.")
        return

    for root, _, files in os.walk(base_dir):
        # Filter files by extension if provided
        if file_extension:
            files = [f for f in files if f.endswith(file_extension)]

        # Sort files
        files.sort()

        if files:  # Only print directories containing files
            print(f"\nDirectory: {root}")
            print("-" * (len(root) + 11))

            for file in files:
                file_path = os.path.join(root, file)
                print(f"File: {file}")
                print("-" * (len(file) + 6))
                try:
                    with open(file_path, 'r', encoding='utf-8', errors='ignore') as f:
                        content = f.read(max_content_length)
                        print(content)
                        if len(content) == max_content_length:
                            print("\n[Content truncated...]\n")
                except Exception as e:
                    print(f"Error reading file: {e}")
                print("-" * (len(file) + 6) + "\n")


if __name__ == "__main__":
    # Set up command-line argument parsing
    parser = argparse.ArgumentParser(description="List files in a directory and print their content.")
    parser.add_argument("base_directory", help="The base directory to search for files.")
    parser.add_argument(
        "--extension",
        default=None,
        help="File extension to filter (e.g., '.txt'). If not provided, includes all files."
    )
    parser.add_argument(
        "--max_content_length",
        type=int,
        default=131072,
        help="Maximum number of characters to display from each file."
    )

    args = parser.parse_args()

    # Call the function with the provided arguments
    list_files_with_content(args.base_directory, args.extension, args.max_content_length)

