//! Embed the committed ICO with the existing Windows SDK; no build dependency.
use std::{env, fs, path::PathBuf, process::Command};

fn resource_compiler() -> PathBuf {
    if let Some(path) = env::var_os("RC") {
        return path.into();
    }
    if let Some(paths) = env::var_os("PATH") {
        if let Some(path) = env::split_paths(&paths)
            .map(|path| path.join("rc.exe"))
            .find(|path| path.is_file())
        {
            return path;
        }
    }
    let mut roots = Vec::new();
    for name in ["WindowsSdkVerBinPath", "WindowsSdkBinPath"] {
        if let Some(path) = env::var_os(name) {
            roots.push(PathBuf::from(path));
        }
    }
    if let Some(path) = env::var_os("WindowsSdkDir") {
        roots.push(PathBuf::from(path).join("bin"));
    }
    if let Some(path) = env::var_os("ProgramFiles(x86)") {
        roots.push(PathBuf::from(path).join("Windows Kits/10/bin"));
    }
    for root in roots {
        let mut versions: Vec<_> = fs::read_dir(&root)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        versions.sort();
        versions.reverse();
        versions.insert(0, root);
        for directory in versions {
            for candidate in [directory.join("x64/rc.exe"), directory.join("rc.exe")] {
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    panic!("Windows SDK rc.exe is required to embed the Linko EXE icon");
}

fn main() {
    for path in ["assets/brand/Linko.ico", "assets/brand/linko.rc"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=LINKO_VERSION");
    for name in [
        "RC",
        "WindowsSdkDir",
        "WindowsSdkBinPath",
        "WindowsSdkVerBinPath",
        "ProgramFiles(x86)",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!(
            "cargo:warning=Windows PE icon embedding requires MSVC/Windows SDK; runtime icon only"
        );
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
        .join("linko-icon.res");
    let status = Command::new(resource_compiler())
        .current_dir(&root)
        .args(["/nologo", "/fo"])
        .arg(&output)
        .arg(root.join("assets/brand/linko.rc"))
        .status()
        .expect("Windows resource compiler could not start");
    assert!(
        status.success(),
        "Windows resource compiler rejected the Linko icon"
    );
    assert!(output.is_file(), "Windows icon resource was not produced");
    println!(
        "cargo:rustc-link-arg-bin=peerglide-gui={}",
        output.display()
    );
}
