fn main() {
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").expect("TARGET")
    );
    tauri_build::build();
}
