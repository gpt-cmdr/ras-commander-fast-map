//! Research probe for the velocity reconstruction kernel.
//! `velocity_probe <sloping-request.json> <inputs-dir> <out.f32>`
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    if a.len() != 3 {
        return Err("usage: velocity_probe <request> <inputs-dir> <out>".into());
    }
    ras_commander_fast_map::velocity::probe(&a[0], &a[1], &a[2])?;
    Ok(())
}
