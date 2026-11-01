use std::io::Write;
use std::path::Path;

use gyro_doctor::attenuate::RepairParameters;
use gyro_doctor::detect::DetectionParameters;

fn write_f64<W: Write>(out: &mut W, values: &[f64]) -> std::io::Result<()> {
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    out.write_all(&bytes)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let video = Path::new(args.get(1).map_or("test.mp4", String::as_str));
    let out_path = Path::new(args.get(2).map_or("/tmp/opencode/rust-repair-dump.bin", String::as_str));

    let decode = gyro_doctor::decode::decode(video, false)?;
    let axes: Vec<String> = vec!["pitch".into(), "yaw".into(), "roll".into()];
    let detection = gyro_doctor::detect::detect(&decode, &axes, &DetectionParameters::default())?;
    let repair = gyro_doctor::repair::repair(&decode, &detection, &axes, &RepairParameters::default())?;

    let mut out = std::io::BufWriter::new(std::fs::File::create(out_path)?);
    let times = &repair.quaternion_times_s;
    write_f64(&mut out, times)?;
    let quaternions: Vec<f64> = repair
        .quaternions_wxyz
        .iter()
        .flat_map(|quaternion| quaternion.iter().copied())
        .collect();
    write_f64(&mut out, &quaternions)?;
    let kept: Vec<u8> = repair
        .kept_after_time_collisions
        .iter()
        .map(|kept| u8::from(*kept))
        .collect();
    out.write_all(&kept)?;
    let rates: Vec<f64> = repair
        .normalized_rate_deg_s
        .iter()
        .flat_map(|rate| rate.iter().copied())
        .collect();
    write_f64(&mut out, &rates)?;
    out.flush()?;
    eprintln!(
        "wrote {} quaternions {} rate samples to {}",
        repair.quaternions_wxyz.len(),
        repair.normalized_rate_deg_s.len(),
        out_path.display()
    );
    Ok(())
}
