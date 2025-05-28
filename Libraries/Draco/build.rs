use std::env;
use std::fs;
use cmake::Config;

fn main() {
    // env::set_var("RUST_BACKTRACE", "full");
    let out_dir = "output";
    env::set_var("OUT_DIR", out_dir);

    // Create build dir if it does not exist yet
    let dir = format!("{}/build/{}", out_dir, out_dir);
    match fs::create_dir_all(dir.clone()) {
        Ok(_) => println!("Build directory created"),
        Err(e) => println!("Unable to create build directory: {}", e),
    }

    // Inside dir there should be another build folder that is actually a symlink to out_dir/build
    // This is because in certain cases, the out_dir is actually nested inside the actual build directory
    // This workaround prevents duplicate build directories
    let current_dir = env::current_dir().unwrap().into_os_string().into_string().unwrap();
    println!("Current directory: {}", current_dir.clone());
    let og_dir = format!("{}/{}/build", current_dir, out_dir);
    let new_dir = format!("{}/build/{}/build", out_dir, out_dir);
    #[cfg(unix)]
    match std::os::unix::fs::symlink(og_dir, new_dir) {
        Ok(_) => println!("Symlink created"),
        Err(e) => println!("Unable to create symlink: {}", e),
    }
    #[cfg(windows)]
    match std::os::windows::fs::symlink_dir(out_dir, symlink) {
        Ok(_) => println!("Symlink created"),
        Err(e) => println!("Unable to create symlink: {}", e),
    }

    let mut config = Config::new("draco_wrapper_cpp");

    // Check our target platform
    if cfg!(target_os = "windows") {
        // Example if using MinGW:
        config.define("CMAKE_SYSTEM_NAME", "Windows");
        config.define("CMAKE_SYSTEM_PROCESSOR", "x86_64");
        
        // Optionally specify compilers (usually set by $CC/$CXX, but you can do it here):
        config.define("CMAKE_C_COMPILER", env::var("CC").unwrap_or("x86_64-w64-mingw32-gcc".to_owned()));
        config.define("CMAKE_CXX_COMPILER", env::var("CXX").unwrap_or("x86_64-w64-mingw32-g++".to_owned()));

        // Force static-link to the GCC/MinGW C++ runtime.
        config.cflag("-static");
        config.cxxflag("-static");
        config.cflag("-static-libgcc");
        config.cxxflag("-static-libgcc");
        config.cflag("-static-libstdc++");
        config.cxxflag("-static-libstdc++");

        config.define("CMAKE_CXX_FLAGS", "-std=c++17 -O3 -static -static-libgcc -static-libstdc++");

        // If you want to use MinGW Makefiles:
        config.generator("MinGW Makefiles");
    }else {
        config.define("CMAKE_CXX_FLAGS", "-std=c++17 -O3");
    }


    let dst = config
    .define("draco_build", "${CMAKE_CURRENT_BINARY_DIR}")  // Override draco_build due to a bug in Draco CMakeLists.txt
    .always_configure(false)
    .build_target("draco_wrapper_cpp_static")  // Build the Draco static library and the wrapper
    .profile("Release")
    .build();
    
    // Expect the dst.display to be "output" directory
    // Othewise, panic
    if !dst.display().to_string().contains(out_dir) {
        panic!("Error: Draco library not found in output directory");
    }

    println!("cargo:rerun-if-changed=draco/CMakeLists.txt");
    println!("cargo:rerun-if-changed=draco/src");
    println!("cargo:rerun-if-changed=draco_wrapper_cpp/CMakeLists.txt");
    println!("cargo:rerun-if-changed=draco_wrapper_cpp/src");
    println!("cargo:rerun-if-env-changed=OUT_DIR");

    // Convert dst to an absolute path
    let absolute_dst = fs::canonicalize(dst.display().to_string())
        .expect("Unable to get absolute path of the build directory");


    // Add Draco library search path (it requires an absolute path)
     // Static linking is order dependent, so we need to link the wrapper first
    println!("cargo:rustc-link-search=native={}/build", absolute_dst.display());
    println!("cargo:rustc-link-lib=static=draco_wrapper_cpp_static");
    println!("cargo:rustc-link-search=native={}/build/draco", absolute_dst.display());
    println!("cargo:rustc-link-lib=static=draco");

    // Link the C++ standard library based on platform
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if cfg!(target_os = "windows") {
        // For the MinGW-w64 environment, you typically link with:
        println!("cargo:rustc-link-lib=stdc++"); 
        println!("cargo:rustc-link-lib=gcc_eh"); // Or other specifics
        // Forcing static-link to the GCC/MinGW C++ runtime.
        println!("cargo:rustc-link-arg=-static");
        println!("cargo:rustc-link-arg=-static-libgcc");
        println!("cargo:rustc-link-arg=-static-libstdc++");
    }

    // Copy the out_dir/build/draco/draco_features.h file to draco/src/draco
    std::fs::copy(
        format!("{}/build/draco/draco_features.h", dst.display()),
        "draco/src/draco/draco_features.h"
    ).expect("Unable to copy draco_features.h");


    // Use bindgen to generate Rust bindings for Draco C++ API
    let bindings = bindgen::Builder::default()
        .clang_args(&[
            "-x", "c++", "--std=c++17",
            "-I", "draco/src",
            "-I", "draco_wrapper_cpp/src",
            "-I", &format!("{}/build/draco", dst.display()),
            "-I", &format!("{}/build/draco_wrapper_cpp", dst.display()),
            "-fretain-comments-from-system-headers", // Retain comments from system headers
            "-fparse-all-comments", // Parse all comments

        ])
        .header("draco/src/draco/attributes/geometry_attribute.h")
        .header("draco/src/draco/attributes/point_attribute.h")
        .header("draco/src/draco/compression/config/compression_shared.h")
        .header("draco/src/draco/compression/encode.h")
        .header("draco/src/draco/compression/decode.h")
        .header("draco/src/draco/compression/point_cloud/point_cloud_encoder.h")
        .header("draco/src/draco/compression/point_cloud/point_cloud_decoder.h")
        .header("draco/src/draco/core/data_buffer.h")
        .header("draco/src/draco/core/encoder_buffer.h")
        .header("draco/src/draco/core/decoder_buffer.h")
        .header("draco/src/draco/point_cloud/point_cloud.h")
        .header("draco_wrapper_cpp/include/wrapper.h")
        .allowlist_type("draco::GeometryAttribute")
        .allowlist_type("draco::PointAttribute")
        .allowlist_type("draco::PointCloudEncodingMethod")
        .allowlist_type("draco::Encoder")
        .allowlist_type("draco::Decoder")
        .allowlist_type("draco::PointCloudEncoder")
        .allowlist_type("draco::PointCloudDecoder")
        .allowlist_type("draco::DataBuffer")
        .allowlist_type("draco::EncoderBuffer")
        .allowlist_type("draco::DecoderBuffer")
        .allowlist_type("draco::PointCloud")
        .allowlist_type("draco_wrapper::DracoWrapper")
        .opaque_type("std::.*") // Rust bindgen is not fully compatible with the C++ standard library
        .generate_comments(true)
        .generate_inline_functions(true) // Required for buffer.size() and buffer.data() functions
        .disable_name_namespacing() // Otherwise all our functions/structs/enumts/etc would start with draco_
        .derive_default(true)
        .raw_line("#![allow(clippy::missing_safety_doc)]")
        .raw_line("#![allow(clippy::too_many_arguments)]")
        .raw_line("#![allow(non_upper_case_globals)]")
        .raw_line("#![allow(non_camel_case_types)]")
        .raw_line("#![allow(non_snake_case)]")
        .raw_line("#![allow(improper_ctypes)]") // This is very unfortunate, but necessary
        .blocklist_type("ValueType")
        .raw_line("pub type ValueType = std::os::raw::c_uint;")
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new())) // Invalide the built crate whenever any of the included header files changed
        .generate()
        .expect("Unable to generate bindings");

    bindings
        .write_to_file("src/bindings.rs")
        .expect("Couldn't write bindings!");

    // Remove the draco_features.h file from draco/src/draco
    std::fs::remove_file("draco/src/draco/draco_features.h")
        .expect("Unable to remove draco_features.h");

}
