fn main() {
    println!("cargo:rerun-if-env-changed=CODEXTRAY_DIAGNOSTIC_VARIANT");
    tauri_build::build()
}
