fn main() {
    println!("cargo:rerun-if-changed=src/windows_audio_capture.cpp");
    println!("cargo:rerun-if-changed=src/macos_system_audio.mm");
    println!("cargo:rerun-if-changed=src/macos_native_drop.mm");
    if std::env::var_os("CARGO_CFG_TARGET_OS").is_some_and(|os| os == "windows")
        && std::env::var_os("CARGO_CFG_TARGET_ENV").is_some_and(|env| env == "msvc")
    {
        cc::Build::new()
            .cpp(true)
            .std("c++17")
            .file("src/windows_audio_capture.cpp")
            .compile("linko_wasapi");
        println!("cargo:rustc-link-lib=ole32");
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
        for framework in ["AppKit", "Foundation", "CoreMedia", "ScreenCaptureKit"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
