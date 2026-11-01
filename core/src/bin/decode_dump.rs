use std::path::Path;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(args.get(1).map_or("test.mp4", String::as_str));
    let result = gyro_doctor::decode::decode(path, false)?;
    println!(
        "quaternions {} rate samples {} quaternion rate {:.12} measurement rate {:.12} period {:.12} fps {:.12}",
        result.quaternion_times_s.len(),
        result.recorded_rate_times_s.len(),
        result.quaternion_rate_hz,
        result.measurement_rate_hz,
        result.period_s,
        result.video_frame_rate
    );
    println!(
        "first time {:.12} last time {:.12} groups {}",
        result.quaternion_times_s[0],
        result.quaternion_times_s[result.quaternion_times_s.len() - 1],
        result.group_opening_quaternion_index.len()
    );
    for i in 0..5 {
        println!(
            "rate {} {:?} median {:?}",
            i, result.recorded_rate_deg_s[i], result.normalized_rate_deg_s[i]
        );
    }
    Ok(())
}
