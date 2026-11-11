fn main() {
    println!("cargo:rerun-if-changed=src/windows_audio_capture.cpp");
    println!("cargo:rerun-if-changed=src/windows_native_drop.cpp");
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
}
