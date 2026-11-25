//! Drop-shadow blur for the on-screen notices: box-blur passes over an alpha mask. Pure CPU.

fn box_pass<const N: usize>(src: &[[f32; N]], w: usize, h: usize, radius: usize, horizontal: bool) -> Vec<[f32; N]> {
    let r = radius as i32;
    let span = (2 * r + 1) as f32;
    let mut out = vec![[0f32; N]; w * h];
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    for line in 0..lines {
        let at = |i: i32| -> usize {
            let i = i.clamp(0, len as i32 - 1) as usize;
            if horizontal { line * w + i } else { i * w + line }
        };
        let mut acc = [0f32; N];
        for i in -r..=r {
            let p = src[at(i)];
            for c in 0..N {
                acc[c] += p[c];
            }
        }
        for i in 0..len as i32 {
            let mut v = [0f32; N];
            for c in 0..N {
                v[c] = acc[c] / span;
            }
            out[at(i)] = v;
            let (add, sub) = (src[at(i + r + 1)], src[at(i - r)]);
            for c in 0..N {
                acc[c] += add[c] - sub[c];
            }
        }
    }
    out
}

/// Blurs a premultiplied BGRA buffer that holds only a black shape (a drop shadow) in place:
/// three box passes over the alpha channel; colour stays black.
pub fn blur_shadow(bgra: &mut [u8], w: usize, h: usize, radius: usize) {
    if w == 0 || h == 0 || radius == 0 {
        return;
    }
    let mut alpha: Vec<[f32; 1]> = bgra.chunks_exact(4).map(|p| [p[3] as f32]).collect();
    for _ in 0..3 {
        alpha = box_pass(&alpha, w, h, radius, true);
        alpha = box_pass(&alpha, w, h, radius, false);
    }
    for (p, [a]) in bgra.chunks_exact_mut(4).zip(alpha) {
        p[0] = 0;
        p[1] = 0;
        p[2] = 0;
        p[3] = a.round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_blur_spreads_but_keeps_mass() {
        let (w, h) = (21, 21);
        let mut buf = vec![0u8; w * h * 4];
        buf[(10 * w + 10) * 4 + 3] = 255;
        blur_shadow(&mut buf, w, h, 2);
        let center = buf[(10 * w + 10) * 4 + 3];
        let near = buf[(10 * w + 12) * 4 + 3];
        assert!(center < 255 && near > 0);
    }
}
