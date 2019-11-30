use std::fs::File;
use std::io::{Read, Write};

fn main() {
    let out_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let include_flag_string = format!("-I{}", out_path.to_string_lossy());

    if cfg!(windows) {
        cc::Build::new()
            .cpp(true)
            .file("src/bindings.cpp")
            .flag("-Isrc")
            .flag("-Iinclude")
            .flag(&include_flag_string)
            .compile("bindings");
    } else {
        cc::Build::new()
            .flag("-Wno-unused-parameter")
            .cpp(true)
            .file("src/bindings.cpp")
            .flag("-Isrc")
            .flag("-Iinclude")
            .flag(&include_flag_string)
            .compile("bindings");
    }

    bindgen::builder()
        .clang_arg("-xc++")
        .header("src/openvr_driver_capi.h")
        .clang_arg("-Isrc")
        .clang_arg("-Iinclude")
        .clang_arg(&include_flag_string)
        .layout_tests(false)
        .enable_cxx_namespaces()
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .derive_default(true)
        // .rustified_enum("vr::ETrackedPropertyError")
        // .rustified_enum("vr::EHDCPError")
        // .rustified_enum("vr::EVRInputError")
        // .rustified_enum("vr::EVRSpatialAnchorError")
        // .rustified_enum("vr::EVRSettingsError")
        // .rustified_enum("vr::EIOBufferError")
        .generate_inline_functions(true)
        .blacklist_function("vr::.*")
        .blacklist_item("std")
        .blacklist_type("vr::IVRSettings")
        .blacklist_type("vr::CVRSettingHelper")
        .blacklist_type("vr::ITrackedDeviceServerDriver")
        .blacklist_type("vr::IVRDisplayComponent")
        .blacklist_type("vr::IVRDriverDirectModeComponent")
        .opaque_type("vr::ICameraVideoSinkCallback")
        .blacklist_type("vr::IVRCameraComponent")
        .opaque_type("vr::IVRDriverContext")
        .blacklist_type("vr::IServerTrackedDeviceProvider")
        .blacklist_type("vr::IVRWatchdogProvider")
        .blacklist_type("vr::IVRCompositorPluginProvider")
        .blacklist_type("vr::IVRProperties")
        .blacklist_type("vr::CVRPropertyHelpers")
        .blacklist_type("vr::IVRDriverInput")
        .blacklist_type("vr::IVRDriverLog")
        .blacklist_type("vr::IVRServerDriverHost")
        .blacklist_type("vr::IVRCompositorDriverHost")
        .blacklist_type("vr::CVRHiddenAreaHelpers")
        .blacklist_type("vr::IVRWatchdogHost")
        .blacklist_type("vr::IVRVirtualDisplay")
        .blacklist_type("vr::IVRResources")
        .blacklist_type("vr::IVRIOBuffer")
        .blacklist_type("vr::IVRDriverManager")
        .blacklist_type("vr::IVRDriverSpatialAnchors")
        .blacklist_type("vr::COpenVRDriverContext")
        .generate()
        .expect("bindings")
        .write_to_file(out_path.join("bindings.rs"))
        .expect("bindings.rs");
}