use std::path::Path;

use gyro_doctor::attenuate::RepairParameters;
use gyro_doctor::detect::DetectionParameters;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let video = Path::new(args.get(1).map_or("test.mp4", String::as_str));
    let output = Path::new(args.get(2).map_or("/tmp/opencode/rust-repaired.mp4", String::as_str));
    let export = args.get(3).map(Path::new);

    let started = std::time::Instant::now();
    let decode = gyro_doctor::decode::decode(video, false)?;
    eprintln!("decode {:.3} s", started.elapsed().as_secs_f64());

    let started = std::time::Instant::now();
    let axes: Vec<String> = vec!["pitch".into(), "yaw".into(), "roll".into()];
    let detection = gyro_doctor::detect::detect(
        &decode,
        &axes,
        &DetectionParameters::default(),
    )?;
    eprintln!("detect {:.3} s", started.elapsed().as_secs_f64());

    let started = std::time::Instant::now();
    let repair = gyro_doctor::repair::repair(
        &decode,
        &detection,
        &axes,
        &RepairParameters::default(),
    )?;
    eprintln!("repair {:.3} s", started.elapsed().as_secs_f64());

    if let Some(export) = export {
        let file = std::fs::File::create(export)?;
        serde_json::to_writer(file, &repair)?;
        eprintln!("wrote {}", export.display());
    }

    let started = std::time::Instant::now();
    let report = gyro_doctor::write::write(video, &repair, output, false)?;
    eprintln!("write {:.3} s: {:?}", started.elapsed().as_secs_f64(), report);
    eprintln!("total {:.3} s", started.elapsed().as_secs_f64());
    Ok(())
}
