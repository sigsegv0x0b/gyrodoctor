use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(args.get(1).map_or("test.mp4", String::as_str));
    let out = Path::new(args.get(2).map_or("/tmp/opencode/rust-decode.json", String::as_str));
    let force = args.iter().any(|argument| argument == "--force");
    let result = gyro_doctor::decode::decode(path, force)?;
    let file = std::fs::File::create(out)?;
    serde_json::to_writer(file, &result)?;
    eprintln!("wrote {}", out.display());
    Ok(())
}
