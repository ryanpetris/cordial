use std::{fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|s| s != "--check") || args.len() > 1 {
        return Err("usage: cordial-schema [--check]".into());
    }
    let check = !args.is_empty();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../schema");
    let artifacts = cordial_schema::generate();
    if !check {
        fs::create_dir_all(&root)?;
    }
    for (name, value) in [
        ("wire.schema.json", artifacts.wire),
        ("commands.json", artifacts.catalog),
    ] {
        let text = serde_json::to_string_pretty(&value)? + "\n";
        let path = root.join(name);
        if check {
            if fs::read_to_string(&path).ok().as_deref() != Some(&text) {
                return Err(format!(
                    "{} is missing or stale; run cargo run -p cordial-schema",
                    path.display()
                )
                .into());
            }
        } else {
            fs::write(path, text)?;
        }
    }
    Ok(())
}
