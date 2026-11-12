fn main() {
    if std::env::var_os("CARGO_CFG_TARGET_OS").is_some_and(|os| os == "macos") {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
        println!("cargo:rerun-if-changed=macos-worker-info.plist");
        println!(
            "cargo:rustc-link-arg-bin=peerglide=-Wl,-sectcreate,__TEXT,__info_plist,{manifest}/macos-worker-info.plist"
        );
    }
}
