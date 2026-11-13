fn main() {
    println!("cargo:rerun-if-changed=src/windows_audio_capture.cpp");
    println!("cargo:rerun-if-changed=src/macos_system_audio.mm");
    println!("cargo:rerun-if-changed=src/windows_native_drop.cpp");
    println!("cargo:rerun-if-changed=src/windows_file_drop_stream.h");
    println!("cargo:rerun-if-changed=src/macos_native_drop.mm");
    if std::env::var_os("CARGO_CFG_TARGET_OS").is_some_and(|os| os == "windows")
        && std::env::var_os("CARGO_CFG_TARGET_ENV").is_some_and(|env| env == "msvc")
    {
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .file("src/windows_audio_capture.cpp")
            .file("src/windows_native_drop.cpp")
            .compile("linko_windows_native");
        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=shell32");
    }
    if std::env::var_os("CARGO_CFG_TARGET_OS").is_some_and(|os| os == "macos") {
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=13.0")
            .file("src/macos_system_audio.mm")
            .file("src/macos_native_drop.mm")
            .compile("linko_macos_native");
        for framework in [
            "AppKit",
            "AVFoundation",
            "Foundation",
            "CoreMedia",
            "ScreenCaptureKit",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
