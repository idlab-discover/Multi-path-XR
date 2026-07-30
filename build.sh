#!/bin/bash

# Clear the current screen
clear

# Exit immediately if a command exits with a non-zero status
set -e

# Default values
RUSTFLAGS="" # "-Z threads=8"
BUILD_MODE="debug"
LINKER="clang"  # Default linker
PACKAGES="all"    # List of packages to build, if empty this script will search for changed packages
# We will collect changed package names in an array
CHANGED_PACKAGES=()
# Enable bindings or not?
BINDINGS="true"
CODECS_BINDINGS="false"
TARGET="x86_64-unknown-linux-gnu"
WINDOWS_TARGET="false"
UNSTABLE="false"
NATIVE_OPT="false"
UBUNTU_2204="false"
TARGET_DIR_OVERRIDE=""
LOCKED="false"
BUILD_UNITY_PLAYER="${BUILD_UNITY_PLAYER:-false}"
UNITY_EDITOR_PATH="${UNITY_EDITOR_PATH:-\~/Unity/Hub/Editor/6000.1.5f1/Editor/Unity}"

# Parse command-line arguments
for arg in "$@"; do
    case $arg in
        --unstable)
            UNSTABLE="true"
            ;;
        --release)
            BUILD_MODE="release"
            export RELEASE_MODE_IS_SET=1
            ;;
        --not-all)
            PACKAGES=""
            ;;
        --no-bindings)
            BINDINGS="false"
            ;;
        --do-codecs-bindings)
            CODECS_BINDINGS="true"
            ;;
        --windows)
            TARGET="x86_64-pc-windows-gnu"
            WINDOWS_TARGET="true"
            ;;
        --native-opt)
            NATIVE_OPT="true"
            ;;
        --ubuntu2204)
            UBUNTU_2204="true"
            ;;
        --target-dir=*)
            TARGET_DIR_OVERRIDE="${arg#*=}"
            ;;
        --locked)
            LOCKED="true"
            ;;
        --unity-player|--build-unity|--build-unity-player)
            BUILD_UNITY_PLAYER="true"
            ;;
        --unity-editor=*)
            UNITY_EDITOR_PATH="${arg#*=}"
            ;;

    esac
done

lock_args=()
if [[ "$LOCKED" == "true" ]]; then
    lock_args+=(--locked)
fi

# -----------------------------------------
# Optional Ubuntu 22.04 Docker relaunch
# -----------------------------------------
if [[ "$UBUNTU_2204" == "true" && "${UBU2204_IN_CONTAINER:-0}" != "1" ]]; then
    set -euo pipefail

    if ! command -v docker >/dev/null 2>&1; then
        echo "ERROR: docker is not installed or not in PATH."
        exit 1
    fi

    SCRIPT_SELF="${BASH_SOURCE[0]}"
    SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_SELF")" && pwd)"
    REPO_DIR="$SCRIPT_DIR"

    IMAGE_TAG="${UBU2204_IMAGE_TAG:-pc-build-ubuntu2204:latest}"
    DOCKERFILE_NAME="${UBU2204_DOCKERFILE:-Dockerfile.ubuntu2204.build}"

    # Decide target dir (host-visible)
    if [[ -n "$TARGET_DIR_OVERRIDE" ]]; then
        export CARGO_TARGET_DIR="$TARGET_DIR_OVERRIDE"
    else
        # Keep container builds separate by default
        export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_DIR/target_ubuntu2204}"
    fi

    # Build image if missing
    if ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
        echo "Docker image '$IMAGE_TAG' not found; building it..."
        if [[ ! -f "$REPO_DIR/$DOCKERFILE_NAME" ]]; then
            echo "ERROR: Dockerfile not found: $REPO_DIR/$DOCKERFILE_NAME"
            exit 1
        fi
        docker build \
            -f "$REPO_DIR/$DOCKERFILE_NAME" \
            -t "$IMAGE_TAG" \
            "$REPO_DIR"
    fi

    # Prepare caches for speed
    CACHE_ROOT="$REPO_DIR/.docker-cache"
    mkdir -p "$CACHE_ROOT/cargo-registry" "$CACHE_ROOT/cargo-git"
    # add next to your other caches
    mkdir -p "$CACHE_ROOT/home" # for any $HOME-based caching

    # Ensure target dir exists and is mounted
    mkdir -p "$CARGO_TARGET_DIR"
    TARGET_DIR_ABS="$(cd "$REPO_DIR" && python3 - <<'PY'
import os
print(os.path.abspath(os.environ["CARGO_TARGET_DIR"]))
PY
    )"

    UID_GID="$(id -u):$(id -g)"

    echo "Relaunching inside Ubuntu 22.04 container..."
    echo "  Repo:   $REPO_DIR"
    echo "  Target: $CARGO_TARGET_DIR"

    exec docker run --rm -it \
        --user "$UID_GID" \
        -e UBU2204_IN_CONTAINER=1 \
        -e CARGO_TARGET_DIR="$TARGET_DIR_ABS" \
        -e RUSTFLAGS="${RUSTFLAGS:-}" \
        -e CARGO_NET_GIT_FETCH_WITH_CLI=true \
        -v "$REPO_DIR:/work" \
        -v "$CACHE_ROOT/cargo-registry:/opt/cargo/registry" \
        -v "$CACHE_ROOT/cargo-git:/opt/cargo/git" \
        -e HOME=/home/container-user \
        -v "$CACHE_ROOT/home:/home/container-user" \
        -v "$TARGET_DIR_ABS:$TARGET_DIR_ABS" \
        -w /work \
        "$IMAGE_TAG" \
        bash "$SCRIPT_SELF" "$@"
fi

# -----------------------------------------
# Function to find MinGW DLLs
# -----------------------------------------
find_mingw_dll() {
  local cc="$1"
  local dll="$2"

  # GCC knows where its runtime lives; prefer that.
  local p
  p="$("$cc" -print-file-name="$dll" 2>/dev/null || true)"
  if [[ -n "$p" && "$p" != "$dll" && -f "$p" ]]; then
    echo "$p"
    return 0
  fi

  # Fall back to sysroot search.
  local sysroot
  sysroot="$("$cc" --print-sysroot 2>/dev/null || true)"
  if [[ -n "$sysroot" && -d "$sysroot" ]]; then
    p="$(find "$sysroot" -type f -name "$dll" 2>/dev/null | head -n 1 || true)"
    if [[ -n "$p" && -f "$p" ]]; then
      echo "$p"
      return 0
    fi
  fi

  # Last-resort: common locations.
  p="$(find /usr -type f -name "$dll" 2>/dev/null | head -n 1 || true)"
  if [[ -n "$p" && -f "$p" ]]; then
    echo "$p"
    return 0
  fi

  return 1
}

# -----------------------------------------
# Function to bundle Windows GNU dist
# -----------------------------------------
bundle_windows_gnu_dist() {
  local repo_dir="$1"
  local target_base="$2"
  local target_triple="$3"
  local profile="$4"
  local cc="$5"


  echo "============================="
  echo "Creating Windows GNU distribution bundle..."


  local bin_dir="$target_base/$target_triple/$profile"
  local dist_dir="$repo_dir/dist/$target_triple/$profile"

  echo "  Bin dir: $bin_dir"
  echo "  Dist dir: $dist_dir"


  mkdir -p "$dist_dir"

  # Copy produced executables and DLLs (adjust patterns if you want to be stricter).
  shopt -s nullglob
  cp -f "$bin_dir"/*.exe "$dist_dir/" 2>/dev/null || true
  cp -f "$bin_dir"/*.dll "$dist_dir/" 2>/dev/null || true
  shopt -u nullglob

  # Required MinGW runtime DLLs (copy libgcc too for safety).
  local dlls=("libwinpthread-1.dll" "libstdc++-6.dll" "libgcc_s_seh-1.dll")

  for d in "${dlls[@]}"; do
    local src
    if ! src="$(find_mingw_dll "$cc" "$d")"; then
      echo "ERROR: Could not locate required runtime DLL: $d"
      exit 1
    fi
    echo "  Copying runtime DLL: $src"
    cp -f "$src" "$dist_dir/"
  done

  echo "Windows GNU dist created at: $dist_dir"
}


# -----------------------------------------
# Helper: run binding generators on host when cross-compiling to Windows
# -----------------------------------------
run_binding_generator() {
  local package="$1"
  local bin="$2"

  if [ "$WINDOWS_TARGET" = "true" ]; then
    echo "  Windows target detected → skip running $package/$bin; building it instead."
        cargo build "${lock_args[@]}" -p "$package" --bin "$bin" --target "$TARGET"
  else
        cargo run "${lock_args[@]}" -p "$package" --bin "$bin" --target "$TARGET"
  fi
}


# If windows windows
if [ "$WINDOWS_TARGET" = "true" ]; then
    RUSTFLAGS="$RUSTFLAGS -C link-self-contained=yes"
    # For windows, we need to link the static versions of the C++ and C libraries
    RUSTFLAGS="$RUSTFLAGS -C link-arg=-static -C link-arg=-static-libgcc -C link-arg=-static-libstdc++"
    LDFLAGS+=" -static-libgcc"
#else
    # If not windows
    # Check for mold and add linker flag if available
    #if command -v mold &> /dev/null; then
        #RUSTFLAGS="$RUSTFLAGS -Clink-arg=-fuse-ld=/usr/bin/mold -Clink-arg=-Wl,--no-rosegment"
    #fi
fi

# Check if the unstable flag is set
if [ "$UNSTABLE" = "true" ]; then
    RUSTFLAGS="$RUSTFLAGS --cfg tokio_unstable --cfg tracing_unstable"
fi

# Check if the native optimization flag is set
if [ "$NATIVE_OPT" = "true" ]; then
    # Enable native optimizations
    RUSTFLAGS="$RUSTFLAGS -C target-cpu=native"
    export ENABLE_NATIVE_OPTIMIZATIONS=1
fi

# Export RUSTFLAGS
export RUSTFLAGS

echo "Building with RUSTFLAGS: $RUSTFLAGS"

# -------------------------------------
# Cross-Compile: If --windows was given
# -------------------------------------
if [ "$WINDOWS_TARGET" = "true" ]; then
    
    # Set up the linker (the MinGW-w64 cross version).
    # Using GCC from mingw-w64:
    export CC="x86_64-w64-mingw32-gcc"
    export CXX="x86_64-w64-mingw32-g++"
    
    # Tell Cargo what linker to use for that target.
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$CC"
else
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=$LINKER
    
    if [ "$LINKER" == "clang" ]; then
        # ------------------------------------------------------------
        # Ensure the correct libstdc++ dev package exists for clang
        # ------------------------------------------------------------
        if command -v clang++ >/dev/null 2>&1; then
            echo "Detecting GCC toolchain used by clang..."

            # Extract GCC installation directory detected by clang
            GCC_TOOLCHAIN=$(clang++ -v 2>&1 | grep "Selected GCC installation" | awk '{print $4}')

            if [ -z "$GCC_TOOLCHAIN" ]; then
                echo "WARNING: Could not detect GCC toolchain used by clang."
            else
                GCC_VERSION=$(basename "$GCC_TOOLCHAIN")

                echo "Clang will use GCC toolchain version: $GCC_VERSION"

                # Check if matching libstdc++ dev package is installed
                if ! dpkg -s "libstdc++-${GCC_VERSION}-dev" >/dev/null 2>&1; then
                    echo "Installing required package: libstdc++-${GCC_VERSION}-dev"
                    sudo apt-get update -y
                    sudo apt-get install -y "libstdc++-${GCC_VERSION}-dev"
                else
                    echo "libstdc++-${GCC_VERSION}-dev already installed"
                fi
            fi
        else
            echo "clang++ not found; skipping libstdc++ compatibility check"
        fi
    fi
fi

if [ "$PACKAGES" == "all" ]; then
    echo "Building all packages..."
    echo "============================="
    # Build all packages
    if [ "$BUILD_MODE" == "release" ]; then
        cargo build "${lock_args[@]}" --release --target "$TARGET"
    else
        cargo build "${lock_args[@]}" --target "$TARGET"
    fi
else
    # Check which packages have changed:
    # Base commit or branch to compare against
    BASE_REF="HEAD~1"   # e.g. 1 commit behind HEAD
    # Or if you're in CI, maybe "origin/main" or something else.

    # Retrieve a list of (path, name) for each workspace member:
    PACKAGES="$(cargo metadata "${lock_args[@]}" --format-version 1 --no-deps | \
    jq -r '.packages[] | "\(.manifest_path) \(.name)"')"



    while IFS= read -r line; do
    MANIFEST_PATH=$(echo "$line" | awk '{print $1}')
    PACKAGE_NAME=$(echo "$line"   | awk '{print $2}')
    
    # The directory of the crate is the directory of the manifest path
    PACKAGE_DIR=$(dirname "$MANIFEST_PATH")

    # Check if there's any change in this directory (including subdirectories)
    # If `git diff` finds changes, it returns exit code 1
    if ! git diff --quiet "$BASE_REF" -- "$PACKAGE_DIR"; then
        CHANGED_PACKAGES+=("$PACKAGE_NAME")
    fi
    done <<< "$PACKAGES"

    if [ ${#CHANGED_PACKAGES[@]} -eq 0 ]; then
        echo "No crates changed. Skipping build."
        if [ "$BUILD_UNITY_PLAYER" != "true" ]; then
            exit 0
        fi
    fi

    echo "Changed crates: ${CHANGED_PACKAGES[@]}"
    echo "Building changed packages..."
    echo "============================="

    for crate in "${CHANGED_PACKAGES[@]}"; do
    # build just the crate
    if [ "$BUILD_MODE" == "release" ]; then
        cargo build "${lock_args[@]}" -p "$crate" --release --target "$TARGET"
    else
        cargo build "${lock_args[@]}" -p "$crate" --target "$TARGET"
    fi
    done
fi
echo "============================="
echo "Build completed."


if [ "$WINDOWS_TARGET" = "true" ] && [[ "$TARGET" == *gnu* ]]; then
    DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
    TARGET_BASE="${CARGO_TARGET_DIR:-$DIR/target}"
    bundle_windows_gnu_dist "$DIR" "$TARGET_BASE" "$TARGET" "$BUILD_MODE" "$CC"
fi

if [ "$CODECS_BINDINGS" == "true" ]; then
    # If change_packages contains spatial_codecs or packages is set to all
    if [[ "$PACKAGES" == "all" || " ${CHANGED_PACKAGES[@]} " =~ " spatial_codecs " ]]; then
        echo "============================="
        echo "Generating spatial_codecs bindings for Unity..."
        echo "============================="
        # Generate FFI bindings for the spatial_codecs library in C# (runs on host, no test harness).
        run_binding_generator "spatial_codecs" "pc_generate_bindings"

        # Get the script's directory
        DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

        # Path to Rust build output (+ DIR)
        TARGET_BASE="${CARGO_TARGET_DIR:-$DIR/target}"

        if [ "$WINDOWS_TARGET" = "true" ]; then
            LIB_RECEIVER_PATH="$TARGET_BASE/$TARGET/$BUILD_MODE/pc-receiver.dll"
        else
            LIB_RECEIVER_PATH="$TARGET_BASE/$TARGET/$BUILD_MODE/libpc_receiver.so"
        fi

        echo "A binding generation was triggered for spatial_codecs, with the following library path: $LIB_RECEIVER_PATH"
        LIBRARY_BINDER_PATH="$DIR/bindings/csharp/PointcloudCodecsInterop.cs"
        echo "Binding source path: $LIBRARY_BINDER_PATH"
    fi
fi

# Should we run bindings?
if [ "$BINDINGS" == "false" ]; then
    echo "============================="
    echo "Skipping bindings as per user request."
    echo "============================="
else

    echo "============================="
    echo "Generating receiver bindings for Unity..."
    echo "============================="

    # If changed_packages contains pc_reciever or packages is set to all
    if [[ "$PACKAGES" == "all" || " ${CHANGED_PACKAGES[@]} " =~ " pc-receiver " ]]; then
        # Generate FFI bindings for the receiver library in C# (runs on host, no test harness).
        run_binding_generator "pc-receiver" "generate_bindings"

        # Get the script's directory
        DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

        # Path to Rust build output (+ DIR)
        TARGET_BASE="${CARGO_TARGET_DIR:-$DIR/target}"

        if [ "$WINDOWS_TARGET" = "true" ]; then
            LIB_RECEIVER_PATH="$TARGET_BASE/$TARGET/$BUILD_MODE/pc-receiver.dll"
        else
            LIB_RECEIVER_PATH="$TARGET_BASE/$TARGET/$BUILD_MODE/libpc_receiver.so"
        fi

        UNITY_PATH="$DIR/Client/pc_renderer_unity"
        UNITY_PLUGIN_PATH="$UNITY_PATH/Assets/Plugins/"
        cp "$LIB_RECEIVER_PATH" "$UNITY_PLUGIN_PATH"
    
        # Check if $UNITY_PATH/Build/debug_Data/Plugins/ exists
        # If it does, copy the library there too
        if [ -d "$UNITY_PATH/Build/debug_Data/Plugins/" ]; then
            cp "$LIB_RECEIVER_PATH" "$UNITY_PATH/Build/debug_Data/Plugins/"
        fi
        # Check if $UNITY_PATH/Build/release_Data/Plugins/ exists
        # If it does, copy the library there too
        if [ -d "$UNITY_PATH/Build/release_Data/Plugins/" ]; then
            cp "$LIB_RECEIVER_PATH" "$UNITY_PATH/Build/release_Data/Plugins/"
        fi

        RECEIVER_BINDER_PATH="$DIR/bindings/csharp/ReceiverInterop.cs"
        UNITY_SCRIPT_PATH="$UNITY_PATH/Assets/Scripts/"
        cp "$RECEIVER_BINDER_PATH" "$UNITY_SCRIPT_PATH"
    fi

    echo "============================="
    echo "All bindings generated."
    echo "============================="
fi

if [ "$BUILD_UNITY_PLAYER" == "true" ]; then
    DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
    UNITY_PROJECT_PATH="$DIR/Client/pc_renderer_unity"

    if [ ! -x "$UNITY_EDITOR_PATH" ]; then
        echo "ERROR: Unity editor not found or not executable: $UNITY_EDITOR_PATH"
        echo "Set UNITY_EDITOR_PATH=/path/to/Unity or pass --unity-editor=/path/to/Unity."
        exit 1
    fi

    echo "============================="
    echo "Building Unity Linux release player..."
    echo "  Editor:  $UNITY_EDITOR_PATH"
    echo "  Project: $UNITY_PROJECT_PATH"
    echo "============================="

    "$UNITY_EDITOR_PATH" \
        -batchmode -quit \
        -projectPath "$UNITY_PROJECT_PATH" \
        -executeMethod CommandLineBuild.BuildLinuxRelease \
        -logFile -

    echo "============================="
    echo "Unity Linux release player build completed."
    echo "============================="
fi


echo -e "\a" # Alert sound
