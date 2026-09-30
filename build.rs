use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");

    if !cfg!(unix) {
        return;
    }

    let rustc = env::var("RUSTC").expect("cargo should provide RUSTC");
    let sysroot = Command::new(rustc)
        .arg("--print=sysroot")
        .output()
        .expect("failed to query rustc sysroot");
    if !sysroot.status.success() {
        panic!("rustc --print=sysroot failed");
    }

    let sysroot = String::from_utf8(sysroot.stdout)
        .expect("rustc sysroot output should be UTF-8")
        .trim()
        .to_owned();
    let target = env::var("TARGET").expect("cargo should provide TARGET");

    let mut rpaths = vec![PathBuf::from(&sysroot).join("lib")];
    rpaths.push(
        PathBuf::from(&sysroot)
            .join("lib")
            .join("rustlib")
            .join(target)
            .join("lib"),
    );

    for rpath in rpaths {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", rpath.display());
    }
}
