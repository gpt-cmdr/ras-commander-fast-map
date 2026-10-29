//! Independent-pixel nodata conversion and product counts, with no arithmetic reassociation.
use rayon::prelude::*;

fn scalar(depth: &mut [f32], wse: &mut [f32]) -> (usize, usize) {
    let (mut finite, mut flooded) = (0, 0);
    for (d, w) in depth.iter_mut().zip(wse) {
        if *d == crate::sloping::NODATA {
            *d = f32::NAN;
            *w = f32::NAN;
        }
        finite += usize::from(d.is_finite());
        flooded += usize::from(*d > 0.0 && *d <= 10000.0);
    }
    (finite, flooded)
}

#[cfg(test)]
fn vector(depth: &mut [f32], wse: &mut [f32]) -> (usize, usize) {
    use wide::f32x8;
    let (mut finite, mut flooded) = (0, 0);
    let length = depth.len() / 8 * 8;
    for (d, w) in depth[..length]
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(wse[..length].as_chunks_mut::<8>().0.iter_mut())
    {
        let x = f32x8::from(*d);
        let y = f32x8::from(*w);
        let mask = x.simd_eq(f32x8::splat(crate::sloping::NODATA));
        let x = mask.bitselect(f32x8::splat(f32::NAN), x);
        d.copy_from_slice(&x.to_array());
        w.copy_from_slice(&mask.bitselect(f32x8::splat(f32::NAN), y).to_array());
        finite += x
            .abs()
            .simd_lt(f32x8::splat(f32::INFINITY))
            .to_bitmask()
            .count_ones() as usize;
        flooded += (x.simd_gt(f32x8::ZERO) & x.simd_le(f32x8::splat(10000.0)))
            .to_bitmask()
            .count_ones() as usize;
    }
    let tail = scalar(&mut depth[length..], &mut wse[length..]);
    (finite + tail.0, flooded + tail.1)
}

pub(crate) fn apply(
    depth: &mut [f32],
    wse: &mut [f32],
    pool: &rayon::ThreadPool,
) -> (usize, usize) {
    assert_eq!(depth.len(), wse.len());
    pool.install(|| {
        depth
            .par_chunks_mut(8192)
            .zip(wse.par_chunks_mut(8192))
            .map(|(d, w)| scalar(d, w))
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    })
}

#[cfg(test)]
fn apply_candidate(
    depth: &mut [f32],
    wse: &mut [f32],
    pool: &rayon::ThreadPool,
    simd: bool,
) -> (usize, usize) {
    if !simd {
        return apply(depth, wse, pool);
    }
    assert_eq!(depth.len(), wse.len());
    pool.install(|| {
        depth
            .par_chunks_mut(8192)
            .zip(wse.par_chunks_mut(8192))
            .map(|(d, w)| vector(d, w))
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_and_scalar_preserve_every_bit_and_counts() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        let special = [
            0.0,
            -0.0,
            f32::NAN,
            f32::from_bits(0x7fc01234),
            f32::INFINITY,
            f32::NEG_INFINITY,
            -9999.0,
            10000.0,
            0.001,
        ];
        let mut seed = 12_u32;
        let a: Vec<_> = (0..19999)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                if i % 3 == 0 {
                    special[i % special.len()]
                } else {
                    f32::from_bits(seed)
                }
            })
            .collect();
        for n in [0, 1, 7, 8, 9, 8193, a.len()] {
            let (mut d, mut w) = (a[..n].to_vec(), a[..n].to_vec());
            let (mut x, mut y) = (d.clone(), w.clone());
            assert_eq!(
                apply_candidate(&mut d, &mut w, &pool, false),
                apply_candidate(&mut x, &mut y, &pool, true)
            );
            assert_eq!(
                d.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                x.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(
                w.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                y.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    #[ignore = "explicit performance experiment; prints timings, no speed threshold"]
    fn benchmark_scalar_and_safe_simd() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let source: Vec<_> = (0..4_000_003)
            .map(|i| {
                if i % 5 == 0 {
                    -9999.0
                } else {
                    (i % 20000) as f32 / 2.0
                }
            })
            .collect();
        let (mut depth, mut wse) = (source.clone(), source.clone());
        for repeat in 0..10 {
            for simd in [repeat % 2 == 0, repeat % 2 != 0] {
                depth.copy_from_slice(&source);
                wse.copy_from_slice(&source);
                let tick = std::time::Instant::now();
                let result = apply_candidate(&mut depth, &mut wse, &pool, simd);
                let seconds = tick.elapsed().as_secs_f64();
                std::hint::black_box(result);
                println!("finalize_benchmark repeat={repeat} simd={simd} seconds={seconds}");
            }
        }
    }
}
