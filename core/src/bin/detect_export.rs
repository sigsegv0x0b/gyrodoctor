use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(args.get(1).map_or("test.mp4", String::as_str));
    let out = Path::new(args.get(2).map_or("/tmp/opencode/rust-detection.json", String::as_str));
    let decode = gyro_doctor::decode::decode(path, false)?;
    let axes: Vec<String> = vec!["pitch".into(), "yaw".into(), "roll".into()];
    let result = gyro_doctor::detect::detect(
        &decode,
        &axes,
        &gyro_doctor::detect::DetectionParameters::default(),
    )?;
    let file = std::fs::File::create(out)?;
    serde_json::to_writer(file, &result)?;
    eprintln!("wrote {}", out.display());
    Ok(())
}
