//! Click and clip repair for movie soundtracks (was tools/movie_declick.py).
//!
//! The soundtracks are mastered hot: `prod1` has samples the ADPCM decoder
//! clamps, and on loud hits the original encoder fell back to coarse frames
//! that click. Each channel is modelled block by block with an
//! autoregressive (AR) model; samples whose prediction error is far above
//! the block's typical error, and samples at full scale, are marked, and
//! each marked run is rebuilt by least-squares AR interpolation from the
//! samples around it (Vaseghi and Godsill). A second pass catches what the
//! first pass's model hid. Ordinary music passes through untouched: only
//! runs that stand out from their own block are rebuilt.

const ORDER: usize = 24;
const BLOCK: usize = 2048;
/// Prediction error, in robust standard deviations, that marks a click.
const THRESHOLD: f64 = 5.0;
/// Extra samples each side of a detection.
const MARGIN: usize = 2;
/// Longer runs are left alone: that much can't be guessed.
const MAX_GAP: usize = 160;
const CLIP: f64 = 32700.0;

/// Repair the channels in place; returns how many samples were rebuilt.
/// Rebuilt peaks may go past 16 bits (they were clipped there): turn the
/// result down before storing it as 16-bit again.
pub fn declick(channels: &mut [Vec<f64>]) -> usize {
    let mut rebuilt = 0;
    for x in channels {
        for _ in 0..2 {
            rebuilt += repair(x);
        }
    }
    rebuilt
}

/// AR coefficients `a` (x[n] ~ sum a[k] x[n-1-k]) by Levinson-Durbin.
fn ar(x: &[f64], p: usize) -> Vec<f64> {
    let mut r: Vec<f64> = (0..=p).map(|k| x[..x.len() - k].iter().zip(&x[k..]).map(|(a, b)| a * b).sum()).collect();
    r[0] *= 1.0 + 1e-4;
    let mut a = vec![0.0; p];
    if r[0] <= 0.0 {
        return a;
    }
    let mut e = r[0];
    for i in 0..p {
        let k = (r[i + 1] - (0..i).map(|j| a[j] * r[i - j]).sum::<f64>()) / e;
        let old = a.clone();
        for j in 0..i {
            a[j] = old[j] - k * old[i - 1 - j];
        }
        a[i] = k;
        e *= 1.0 - k * k;
        if e <= 0.0 {
            break;
        }
    }
    a
}

fn predict(x: &[f64], a: &[f64], n: usize) -> f64 {
    a.iter().enumerate().map(|(k, c)| c * x[n - 1 - k]).sum()
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 }
}

fn repair(x: &mut [f64]) -> usize {
    let len = x.len();
    let mut fixed = 0;
    for b0 in (0..len).step_by(BLOCK) {
        let (lo, hi) = (b0.saturating_sub(ORDER * 4), (b0 + BLOCK + ORDER * 4).min(len));
        let blk = x[lo..hi].to_vec();
        if hi - lo <= ORDER + 1 || blk.iter().all(|s| s.abs() < 64.0) {
            continue;
        }
        let n = blk.len();
        let windowed: Vec<f64> = blk.iter().enumerate().map(|(i, s)| s * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos())).collect();
        let a = ar(&windowed, ORDER);
        let e: Vec<f64> = (0..n).map(|i| if i < ORDER { 0.0 } else { blk[i] - predict(&blk, &a, i) }).collect();
        let scale = median(e[ORDER..].iter().map(|v| v.abs()).collect()) / 0.6745 + 1.0;
        let bad: Vec<usize> = (ORDER..n)
            .filter(|&i| e[i].abs() > THRESHOLD * scale || blk[i].abs() >= CLIP)
            .filter(|&i| i + lo >= b0 && i + lo < b0 + BLOCK)
            .collect();
        let Some(&first) = bad.first() else { continue };
        // Merge into runs, with a margin.
        let (mut start, mut end) = (first as isize - MARGIN as isize, first + MARGIN + 1);
        let mut spans = Vec::new();
        for &i in &bad[1..] {
            if i as isize - MARGIN as isize <= (end + ORDER / 2) as isize {
                end = i + MARGIN + 1;
            } else {
                spans.push((start, end));
                (start, end) = (i as isize - MARGIN as isize, i + MARGIN + 1);
            }
        }
        spans.push((start, end));
        for (s, t) in spans {
            let s = ((s + lo as isize).max(ORDER as isize)) as usize;
            let t = (t + lo).min(len - ORDER);
            if t > s && t - s <= MAX_GAP {
                interpolate(x, &a, s, t);
                fixed += t - s;
            }
        }
    }
    fixed
}

/// Least-squares AR fill of x[lo..hi] from the samples around it: the
/// values that make the prediction error over the stretch smallest.
fn interpolate(x: &mut [f64], a: &[f64], lo: usize, hi: usize) {
    let p = a.len();
    let (s, t) = (lo.saturating_sub(p), (hi + p).min(x.len()));
    let n = t - s;
    if n <= p {
        return;
    }
    let rows = n - p;
    let m = hi - lo;
    let (u0, u1) = (lo - s, hi - s);
    // Error row i: e_i = seg[i+p] - sum_k a[k] seg[i+p-1-k].
    let coef = |i: usize, c: usize| -> f64 {
        if c == i + p {
            1.0
        } else if c >= i && c < i + p {
            -a[i + p - 1 - c]
        } else {
            0.0
        }
    };
    // What the known samples contribute to each error.
    let b: Vec<f64> = (0..rows).map(|i| (i..=i + p).filter(|&c| c < u0 || c >= u1).map(|c| coef(i, c) * x[s + c]).sum()).collect();
    // Normal equations: (Au^T Au) xu = -Au^T b.
    let mut mat = vec![0.0; m * m];
    let mut rhs = vec![0.0; m];
    for i in 0..rows {
        let cols: Vec<(usize, f64)> = (u0.max(i)..u1.min(i + p + 1)).map(|c| (c - u0, coef(i, c))).filter(|&(_, v)| v != 0.0).collect();
        for &(j, vj) in &cols {
            rhs[j] -= vj * b[i];
            for &(k, vk) in &cols {
                mat[j * m + k] += vj * vk;
            }
        }
    }
    let ridge = 1e-9 * (0..m).map(|j| mat[j * m + j]).sum::<f64>() / m as f64;
    for j in 0..m {
        mat[j * m + j] += ridge;
    }
    if let Some(sol) = cholesky_solve(&mut mat, &rhs, m) {
        x[lo..hi].copy_from_slice(&sol);
    }
}

/// Solve the symmetric positive definite system `mat` y = `rhs`.
fn cholesky_solve(mat: &mut [f64], rhs: &[f64], m: usize) -> Option<Vec<f64>> {
    for j in 0..m {
        let mut d = mat[j * m + j];
        for k in 0..j {
            d -= mat[j * m + k] * mat[j * m + k];
        }
        if d <= 0.0 {
            return None;
        }
        let d = d.sqrt();
        mat[j * m + j] = d;
        for i in j + 1..m {
            let mut v = mat[i * m + j];
            for k in 0..j {
                v -= mat[i * m + k] * mat[j * m + k];
            }
            mat[i * m + j] = v / d;
        }
    }
    let mut y = rhs.to_vec();
    for i in 0..m {
        for k in 0..i {
            y[i] -= mat[i * m + k] * y[k];
        }
        y[i] /= mat[i * m + i];
    }
    for i in (0..m).rev() {
        for k in i + 1..m {
            y[i] -= mat[k * m + i] * y[k];
        }
        y[i] /= mat[i * m + i];
    }
    Some(y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilds_a_click_in_a_sine() {
        let clean: Vec<f64> = (0..20_000).map(|i| (8000.0 * (i as f64 * 0.05).sin()).round()).collect();
        let mut x = clean.clone();
        x[10_000] = 30_000.0;
        x[10_001] = -30_000.0;
        let mut ch = vec![x];
        let n = declick(&mut ch);
        assert!(n > 0 && n < 100, "{n} samples rebuilt");
        let err = ch[0].iter().zip(&clean).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(err < 200.0, "largest difference {err}");
    }
}
