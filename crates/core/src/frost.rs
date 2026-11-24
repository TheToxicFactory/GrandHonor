//! Frosted glass from a copy of the screen: shrink, blur and slightly saturate a BGRA image.
//! Used by the panel (System → Style → Glass) and by the on-screen notices. Pure CPU, no GPU.

/// A top-down, opaque BGRA image.
pub struct Frosted {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Frosted {
    /// Mean Rec. 709 luminance, 0.0 (black) to 1.0 (white): picks dark or light glass ink.
    pub fn luminance(&self) -> f32 {
        let n = (self.bgra.len() / 4).max(1) as f32;
        let sum: f32 = self
            .bgra
            .chunks_exact(4)
            .map(|p| 0.0722 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.2126 * p[2] as f32)
            .sum();
        sum / n / 255.0
    }
}

/// `src` is `w`×`h` BGRA. Each `shrink`×`shrink` block is averaged, then three box-blur passes
/// of `radius` (at the reduced size) approximate a Gaussian, and colour is pushed by
/// `saturation` (1.0 keeps it), as vibrancy does on macOS and iOS.
pub fn frost(src: &[u8], w: usize, h: usize, shrink: usize, radius: usize, saturation: f32) -> Frosted {
    let s = shrink.max(1);
    let (sw, sh) = (w.div_ceil(s).max(1), h.div_ceil(s).max(1));
    let mut small = vec![[0f32; 3]; sw * sh];
    for by in 0..sh {
        for bx in 0..sw {
            let mut acc = [0f32; 3];
            let mut n = 0f32;
            for yy in by * s..((by + 1) * s).min(h) {
                let row = yy * w * 4;
                for xx in bx * s..((bx + 1) * s).min(w) {
                    let p = row + xx * 4;
                    acc[0] += src[p] as f32;
                    acc[1] += src[p + 1] as f32;
                    acc[2] += src[p + 2] as f32;
                    n += 1.0;
                }
            }
            let n = n.max(1.0);
            small[by * sw + bx] = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    }
    for _ in 0..3 {
        small = box_pass(&small, sw, sh, radius, true);
        small = box_pass(&small, sw, sh, radius, false);
    }
    let mut bgra = Vec::with_capacity(sw * sh * 4);
    for [b, g, r] in small {
        let luma = 0.0722 * b + 0.7152 * g + 0.2126 * r;
        let sat = |c: f32| (luma + (c - luma) * saturation).clamp(0.0, 255.0) as u8;
        bgra.extend_from_slice(&[sat(b), sat(g), sat(r), 255]);
    }
    Frosted { width: sw as u32, height: sh as u32, bgra }
}

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
    fn flat_colour_stays_flat_and_shrinks() {
        let (w, h) = (40, 24);
        let src: Vec<u8> = (0..w * h).flat_map(|_| [10u8, 120, 200, 255]).collect();
        let out = frost(&src, w, h, 8, 3, 1.45);
        assert_eq!((out.width, out.height), (5, 3));
        let first = &out.bgra[..4];
        assert!(out.bgra.chunks(4).all(|p| p == first));
        assert_eq!(first[3], 255);
    }

    #[test]
    fn luminance_of_white_and_black() {
        let white = Frosted { width: 1, height: 1, bgra: vec![255, 255, 255, 255] };
        let black = Frosted { width: 1, height: 1, bgra: vec![0, 0, 0, 255] };
        assert!(white.luminance() > 0.99 && black.luminance() < 0.01);
    }

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
