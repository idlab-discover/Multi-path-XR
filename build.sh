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
# Enable tests or not?
TESTS="true"
TARGET="x86_64-unknown-linux-gnu"
WINDOWS_TARGET="false"
UNSTABLE="false"

# Parse command-line arguments
for arg in "$@"; do
    case $arg in
        --unstable)
            UNSTABLE="true"
            ;;
        --release)
            BUILD_MODE="release"
            ;;
        --not-all)
            PACKAGES=""
            ;;
        --no-tests)
            TESTS="false"
            ;;
        --windows)
            TARGET="x86_64-pc-windows-gnu"
            WINDOWS_TARGET="true"
            ;;
    esac
done

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

# Export RUSTFLAGS
export RUSTFLAGS

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
fi

if [ "$PACKAGES" == "all" ]; then
    # Build all packages
    if [ "$BUILD_MODE" == "release" ]; then
        cargo build --release --target "$TARGET"
    else
        cargo build --target "$TARGET"
    fi
else
    # Check which packages have changed:
    # Base commit or branch to compare against
    BASE_REF="HEAD~1"   # e.g. 1 commit behind HEAD
    # Or if you're in CI, maybe "origin/main" or something else.

    # Retrieve a list of (path, name) for each workspace member:
    PACKAGES="$(cargo metadata --format-version 1 --no-deps | \
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
    exit 0
    fi

    echo "Changed crates: ${CHANGED_PACKAGES[@]}"

    for crate in "${CHANGED_PACKAGES[@]}"; do
    # build just the crate
    if [ "$BUILD_MODE" == "release" ]; then
        cargo build -p "$crate" --release --target "$TARGET"
    else
        cargo build -p "$crate" --target "$TARGET"
    fi
    done
fi

# Should we run tests?
if [ "$TESTS" == "false" ]; then
    echo -e "\a"
    exit 0
fi

# If changed_packages contains pc_reciever or packages is set to all
if [[ "$PACKAGES" == "all" || " ${CHANGED_PACKAGES[@]} " =~ " pc-receiver " ]]; then
    # The tests also generates ffi bindings for the receiver library in C#
    cargo test -p pc-receiver --target "$TARGET"

    # Get the script's directory
    DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

    # Path to Rust build output (+ DIR)
    if [ "$WINDOWS_TARGET" = "true" ]; then
        LIB_RECEIVER_PATH="$DIR/target/$TARGET/$BUILD_MODE/pc-receiver.dll"
    else
        LIB_RECEIVER_PATH="$DIR/target/$TARGET/$BUILD_MODE/libpc_receiver.so"
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

    RECEIVER_BINDER_PATH="$DIR/Client/receiver/bindings/csharp/ReceiverInterop.cs"
    UNITY_SCRIPT_PATH="$UNITY_PATH/Assets/Scripts/"
    cp "$RECEIVER_BINDER_PATH" "$UNITY_SCRIPT_PATH"
fi


echo -e "\a"