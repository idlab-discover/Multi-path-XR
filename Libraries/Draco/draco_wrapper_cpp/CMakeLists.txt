cmake_minimum_required(VERSION 3.12 FATAL_ERROR)

# Set the project name
project(draco_wrapper_cpp)

# Set C and C++ standard to C++17
set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED True)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(CMAKE_C_FLAGS_DEBUG_INIT "-Wall -Wextra -Werror -g3 -O0")
set(CMAKE_CXX_FLAGS_DEBUG_INIT "-Wall -Wextra -Werror -g3 -O0")
set(CMAKE_C_FLAGS_RELEASE_INIT "-Wall -O3")
set(CMAKE_CXX_FLAGS_RELEASE_INIT "-Wall -O3")

# Only enable Clang-Tidy if we are doing a native Clang build (not cross).
if (CMAKE_CXX_COMPILER_ID STREQUAL "Clang" AND NOT WIN32)
    set(CMAKE_CXX_CLANG_TIDY clang-tidy)
endif()

set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Draco-specific settings
set(DRACO_FAST ON)
set(DRACO_JS_GLUE OFF)
set(DRACO_IE_COMPATIBLE OFF)
set(DRACO_MESH_COMPRESSION OFF)
set(DRACO_POINT_CLOUD_COMPRESSION ON)
set(DRACO_TESTS OFF)
set(DRACO_WASM OFF)
set(DRACO_UNITY_PLUGIN OFF)
set(DRACO_ANIMATION_ENCODING OFF)
set(DRACO_GLTF_BITSTREAM OFF)
set(DRACO_MAYA_PLUGIN OFF)
set(DRACO_INSTALL OFF)
set(DRACO_ENABLE_OPTIMIZATIONS ON)
set(DRACO_ENABLE_NEON ON)
set(DRACO_ENABLE_SSE4_1 ON)

# Add dependencies
add_subdirectory(../draco ${CMAKE_BINARY_DIR}/draco) # This adds the Draco library, both the source and the build (binary) directories

message(STATUS "CMAKE_SOURCE_DIR = ${CMAKE_SOURCE_DIR}")
message(STATUS "Including path = ${CMAKE_SOURCE_DIR}/../draco/src")
message(STATUS "CMAKE_BINARY_DIR = ${CMAKE_BINARY_DIR}")
message(STATUS "Including path = ${CMAKE_BINARY_DIR}/draco")

# Include Draco's source and build directories
include_directories(${CMAKE_SOURCE_DIR}/../draco/src)
include_directories(${CMAKE_BINARY_DIR}/draco)

# Include the directory where the wrapper library's source code is located
include_directories(
    "${PROJECT_BINARY_DIR}"
    ${CMAKE_SOURCE_DIR}/include)

# Add the wrapper library
add_library(draco_wrapper_cpp_static STATIC)
target_sources(draco_wrapper_cpp_static
    PRIVATE
    ${CMAKE_SOURCE_DIR}/src/wrapper.cpp
    PUBLIC
    ${CMAKE_SOURCE_DIR}/include/wrapper.h
)
target_include_directories(draco_wrapper_cpp_static
    PUBLIC
    ${CMAKE_CURRENT_LIST_DIR}/include/
  )

# Link the Draco static library to the wrapper library
target_link_libraries(draco_wrapper_cpp_static
    PUBLIC 
    draco_static
)

add_dependencies(draco_wrapper_cpp_static draco_static)

target_compile_options(draco_wrapper_cpp_static PRIVATE -fPIC)
target_compile_options(draco_static PRIVATE -fPIC)