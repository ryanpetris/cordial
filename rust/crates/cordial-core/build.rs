use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CORDIAL_CONFIG");
    let frames = if let Some(path) = env::var_os("CORDIAL_CONFIG") {
        let path = PathBuf::from(path);
        println!("cargo:rerun-if-changed={}", path.display());
        let board: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read board configuration"))
                .expect("parse board configuration");
        board["output_frames"]
            .as_u64()
            .filter(|n| (4..=u32::MAX as u64).contains(n))
            .expect("output_frames must be an integer from 4 to 4294967295")
    } else {
        // Host tests without a board use the smallest queue.
        // tools/check_memory.py explicitly selects the Pico W board.
        4
    };
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("output_frames.rs"),
        format!("pub const OUTPUT_FRAMES: usize = {frames};\n"),
    )
    .unwrap();
}
