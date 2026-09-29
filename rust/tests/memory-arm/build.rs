fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        out.join("memory.x"),
        "MEMORY { FLASH : ORIGIN = 0, LENGTH = 4M\n RAM : ORIGIN = 0x20000000, LENGTH = 4M }",
    )
    .unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rerun-if-env-changed=CORDIAL_TEST_HEAP_BYTES");
    let heap: usize = std::env::var("CORDIAL_TEST_HEAP_BYTES")
        .unwrap_or_else(|_| "117316".into())
        .parse()
        .unwrap();
    std::fs::write(out.join("budget.rs"), heap.to_string()).unwrap();
}
