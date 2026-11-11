fn main() {
    println!("cargo:rerun-if-changed=src/windows_audio_capture.cpp");
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
}
