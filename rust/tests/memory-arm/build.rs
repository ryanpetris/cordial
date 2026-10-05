//! The board under test comes from `check_memory.py`: its name, the heap span of its linked
//! development firmware and its generated profile memory budget.
fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        out.join("memory.x"),
        "MEMORY { FLASH : ORIGIN = 0, LENGTH = 4M\n RAM : ORIGIN = 0x20000000, LENGTH = 4M }",
    )
    .unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg=-Tlink.x");
    // LittleFS uses C string functions from the nano C library, as on the board.
    println!("cargo:rerun-if-env-changed=CC_thumbv6m_none_eabi");
    let compiler = std::env::var("CC_thumbv6m_none_eabi").expect("the ARM C compiler");
    let output = std::process::Command::new(compiler)
        .args([
            "-mcpu=cortex-m0plus",
            "-mthumb",
            "-print-file-name=libc_nano.a",
        ])
        .output()
        .expect("locate nano libc");
    assert!(output.status.success());
    let libc = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(libc.is_file(), "ARM nano C library missing");
    println!(
        "cargo:rustc-link-search=native={}",
        libc.parent().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static=c_nano");
    let var = |name: &str| {
        println!("cargo:rerun-if-env-changed={name}");
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{name} is set by rust/tools/check_memory.py"))
    };
    let board = var("CORDIAL_TEST_BOARD");
    let heap: usize = var("CORDIAL_TEST_HEAP_BYTES").parse().unwrap();
    let budget = var("CORDIAL_TEST_PROFILE_MEMORY_BUDGET");
    assert!(
        budget == "None"
            || budget
                .strip_prefix("Some(")
                .and_then(|b| b.strip_suffix(')'))
                .is_some_and(|b| b.parse::<u32>().is_ok())
    );
    std::fs::write(
        out.join("board.rs"),
        format!(
            "pub const BOARD: &str = {board:?};\n\
             pub const HEAP_BYTES: usize = {heap};\n\
             pub const PROFILE_MEMORY_BUDGET: Option<u32> = {budget};\n"
        ),
    )
    .unwrap();
}
