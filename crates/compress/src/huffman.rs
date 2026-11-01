//! Huffman code construction shared by the DEFLATE, bzip2 and Zstandard encoders.

/// Compute length-limited Huffman code lengths for `freqs` (0 = unused symbol).
/// If fewer than `min_codes` symbols are used, the lowest unused symbols get a nonzero
/// frequency so the code has at least that many entries (DEFLATE wants >= 2 codes).
pub fn code_lengths(freqs: &[u32], max_len: u32, min_codes: usize) -> Vec<u8> {
    let n = freqs.len();
    let mut lens = vec![0u8; n];
    let mut syms: Vec<(u32, u32)> = freqs
        .iter()
        .enumerate()
        .filter(|(_, f)| **f > 0)
        .map(|(i, &f)| (f, i as u32))
        .collect();
    if syms.len() < min_codes {
        let mut i = 0;
        while syms.len() < min_codes && i < n {
            if freqs[i] == 0 {
                syms.push((1, i as u32));
            }
            i += 1;
        }
    }
    match syms.len() {
        0 => return lens,
        1 => {
            lens[syms[0].1 as usize] = 1;
            return lens;
        }
        _ => {}
    }
    syms.sort_unstable();
    let m = syms.len();
    // Two-queue Huffman construction: leaves 0..m, internal nodes m..2m-1.
    let mut weight: Vec<u64> = Vec::with_capacity(2 * m);
    weight.extend(syms.iter().map(|s| s.0 as u64));
    let mut parent = vec![0usize; 2 * m - 1];
    let (mut li, mut ni) = (0usize, m);
    let pick = |li: &mut usize, ni: &mut usize, weight: &Vec<u64>| -> usize {
        if *li < m && (*ni >= weight.len() || weight[*li] <= weight[*ni]) {
            *li += 1;
            *li - 1
        } else {
            *ni += 1;
            *ni - 1
        }
    };
    for _ in 0..m - 1 {
        let a = pick(&mut li, &mut ni, &weight);
        let b = pick(&mut li, &mut ni, &weight);
        let idx = weight.len();
        weight.push(weight[a] + weight[b]);
        parent[a] = idx;
        parent[b] = idx;
    }
    // Depths: root is the last node.
    let total = 2 * m - 1;
    let mut depth = vec![0u32; total];
    for i in (0..total - 1).rev() {
        depth[i] = depth[parent[i]] + 1;
    }
    let maxd = depth[..m].iter().copied().max().unwrap();
    let lim = max_len.max(1);
    let mut count = vec![0u32; (maxd.max(lim) + 1) as usize];
    for &d in &depth[..m] {
        count[d as usize] += 1;
    }
    if maxd > lim {
        for l in (lim + 1)..=maxd {
            count[lim as usize] += count[l as usize];
            count[l as usize] = 0;
        }
        let mut total: u64 = 0;
        for l in 1..=lim {
            total += (count[l as usize] as u64) << (lim - l);
        }
        let target = 1u64 << lim;
        while total > target {
            count[lim as usize] -= 1;
            for l in (1..lim).rev() {
                if count[l as usize] > 0 {
                    count[l as usize] -= 1;
                    count[l as usize + 1] += 2;
                    break;
                }
            }
            total -= 1;
        }
    }
    // Most frequent symbols get the shortest codes.
    let mut it = syms.iter().rev();
    for l in 1..=lim.max(maxd) {
        for _ in 0..count.get(l as usize).copied().unwrap_or(0) {
            let s = it.next().unwrap();
            lens[s.1 as usize] = l as u8;
        }
    }
    lens
}

/// Canonical (MSB-first) codes for the given lengths.
pub fn canonical_codes(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 33];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 33];
    let mut code = 0u32;
    for l in 1..33 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[l as usize];
                next[l as usize] += 1;
                c
            }
        })
        .collect()
}

#[inline]
pub fn reverse_bits(code: u32, len: u32) -> u32 {
    if len == 0 { 0 } else { code.reverse_bits() >> (32 - len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kraft() {
        let mut freqs = vec![0u32; 300];
        let mut x = 1u32;
        for (i, f) in freqs.iter_mut().enumerate() {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            *f = if i % 7 == 0 { 0 } else { 1 + (x >> 16) % (1 + (i as u32 * i as u32)) };
        }
        freqs[3] = 1_000_000_000;
        for lim in [9u32, 15] {
            let l = code_lengths(&freqs, lim, 2);
            let s: f64 = l.iter().filter(|&&x| x > 0).map(|&x| 0.5f64.powi(x as i32)).sum();
            assert!(s <= 1.0 + 1e-12);
            assert!(l.iter().all(|&x| x as u32 <= lim));
            assert!(l.iter().zip(&freqs).all(|(&a, &f)| (a > 0) == (f > 0)));
        }
    }
}
