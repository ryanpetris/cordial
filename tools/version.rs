// Shared by the CLI and firmware build scripts. Python owns version policy.
pub fn configure() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    // Tags, the index and untracked files can change without source mtimes
    // changing. A missing input deliberately reruns the build script.
    println!(
        "cargo:rerun-if-changed={}",
        root.join("target/always-recompute-version").display()
    );
    println!("cargo:rerun-if-env-changed=CORDIAL_PYTHON");
    println!("cargo:rerun-if-env-changed=CORDIAL_VERSION");
    let override_python = std::env::var_os("CORDIAL_PYTHON");
    let candidates: Vec<_> = override_python.map_or_else(
        || vec!["python3".into(), "python".into(), "py".into()],
        |python| vec![python],
    );
    for executable in candidates {
        let mut command = std::process::Command::new(&executable);
        if executable == "py" {
            command.arg("-3");
        }
        if !command
            .args(["-c", "import sys; sys.exit(sys.version_info < (3, 11))"])
            .output()
            .is_ok_and(|result| result.status.success())
        {
            continue;
        }
        let mut command = std::process::Command::new(&executable);
        if executable == "py" {
            command.arg("-3");
        }
        let result = command
            .arg(root.join("tools/version.py"))
            .output()
            .expect("run version resolver");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        println!(
            "cargo:rustc-env=CORDIAL_VERSION={}",
            String::from_utf8(result.stdout).unwrap().trim()
        );
        return;
    }
    panic!(
        "Cordial builds require Python 3.11+; install it or set CORDIAL_PYTHON to its executable"
    );
}
