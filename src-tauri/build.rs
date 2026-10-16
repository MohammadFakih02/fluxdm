fn main() {
    // The Mozilla-signed extension is embedded when it's there; without it FluxDM packs the unsigned one,
    // which only Waterfox / Firefox Developer / Nightly / ESR accept (with signatures switched off).
    println!("cargo::rustc-check-cfg=cfg(signed_xpi)");
    println!("cargo::rerun-if-changed=extension/signed");
    if std::path::Path::new("extension/signed/FluxDM-extension.xpi").is_file() {
        println!("cargo::rustc-cfg=signed_xpi");
    }
    tauri_build::build()
}
