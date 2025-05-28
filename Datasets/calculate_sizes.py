#!/usr/bin/env python3
from __future__ import annotations
import os
from pathlib import Path
from collections import namedtuple
import math

Stats = namedtuple("Stats", ["size", "count", "sum_sq"])     # total size in bytes, number of files

def collect(path: Path) -> tuple[Stats, list[str]]:
    """
    Post-order DFS.
    Returns the aggregated Stats for *path* and a list of pretty-printed lines that
    represent the subtree rooted at *path* (already indented).
    """
    indent = "│   "          # what a normal `tree` uses between levels
    corner = "└── "          # last entry
    tee    = "├── "          # not-last entry

    total_size = total_count = total_sum_sq = 0
    lines: list[str] = []

    # separate children so we can decide which is the “last” one for pretty printing
    dirs, files = [], []
    with os.scandir(path) as it:
        for entry in it:
            if entry.is_symlink():
                continue                       # skip symlinks (optional)
            if entry.is_dir(follow_symlinks=False):
                dirs.append(entry)
            elif entry.is_file(follow_symlinks=False):
                files.append(entry)

    # Process sub-directories first (depth-first)
    for idx, d in enumerate(sorted(dirs, key=lambda e: e.name)):
        child_stats, child_lines = collect(Path(d.path))
        total_size  += child_stats.size
        total_count += child_stats.count
        total_sum_sq += child_stats.sum_sq

        # adapt indentation of child lines
        branch = tee if idx < len(dirs)-1 or files else corner
        prefix = branch
        lines.append(f"{prefix}{d.name}")      # first line for the directory itself
        last_prefix = indent if idx < len(dirs)-1 or files else "    "
        lines.extend(last_prefix + line for line in child_lines)

    # Now account for files in *this* directory
    for idx, f in enumerate(sorted(files, key=lambda e: e.name)):
        size = f.stat().st_size
        #print(f"  {f.name} ({size:,} B)")   # print file size
        total_size  += size
        total_count += 1
        total_sum_sq += size * size
        branch = tee if idx < len(files)-1 else corner
        lines.append(f"{branch}{f.name} ({size:,} B)")   # file lines won’t appear
                                                        # in final printing – we’ll
                                                        # strip them later.

    return Stats(total_size, total_count, total_sum_sq), lines


def strip_file_lines(lines: list[str]) -> list[str]:
    """Remove the lines that correspond to individual files."""
    return [ln for ln in lines if not ln.strip().endswith("B)")]


def annotate(lines: list[str], stats_map: dict[str, Stats]) -> list[str]:
    """
    Replace each directory line with:
        ‹dirname›  ——  total: … B, files: …, avg: … B
    """
    out = []
    for ln in lines:
        stripped = ln.lstrip("│ ").lstrip("└── ").lstrip("├── ").rstrip()
        if stripped in stats_map:            # a directory name
            st = stats_map[stripped]
            avg = st.size / st.count if st.count else 0
            var  = st.sum_sq / st.count - avg * avg if st.count else 0
            std  = math.sqrt(max(var, 0))
            avg_kB = avg / 1000
            std_kB = std / 1000
            ln = ln.replace(
                stripped,
                f"{stripped}  (total: {st.size:,} B, files: {st.count}, "
                f"avg: {avg:,.1f} B, std: {std:,.1f} B) -> per file: \${avg_kB:,.2f} \\pm {std_kB:,.2f}\$ "
            )
        out.append(ln)
    return out


def main(root_dir: str = "."):
    root = Path(root_dir).resolve()
    stats_map: dict[str, Stats] = {}

    root_stats, subtree_lines = collect(root)
    # save the aggregate for every directory name encountered once
    # (names are unique inside the parent, which is all we need here)
    for path, _, files in os.walk(root):
        here = Path(path)
        sizes = [f.stat().st_size for f in here.glob("**/*") if f.is_file()]
        stats_map[here.name] = Stats(
            sum(sizes), len(sizes), sum(s * s for s in sizes)
        )

    # build the textual tree without file entries, then annotate
    dir_lines = strip_file_lines(subtree_lines)
    final_lines = annotate(dir_lines, stats_map)

    print(f"{root.name}/")                    # root line
    for l in final_lines:
        print(l)

    # Top-level summary
    avg_root = root_stats.size / root_stats.count if root_stats.count else 0
    var_root = root_stats.sum_sq / root_stats.count - avg_root * avg_root if root_stats.count else 0
    std_root = math.sqrt(max(var_root, 0))

    print("\nSummary for '.', including all sub-folders:")
    print(f"  total size : {root_stats.size:,} bytes")
    print(f"  file count : {root_stats.count}")
    print(f"  avg. file  : {avg_root:,.1f} bytes")
    print(f"  std. file  : {std_root:,.1f} bytes")


if __name__ == "__main__":
    main()
