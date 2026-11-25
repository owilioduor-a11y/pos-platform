fn main() {
    // Proc-macro UniFFI scaffolding is generated at compile time via
    // `uniffi::setup_scaffolding!()` in src/lib.rs; this script only
    // declares which inputs should trigger rebuilds.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=migrations");
}
