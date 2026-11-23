//! Index arithmetic for the delay lines' circular buffers, without the integer division a `%`
//! by a runtime length costs on every sample (tens of cycles each on x86, and the delay lines
//! did up to five per frame). Each returns exactly what the `%` expression it replaces does,
//! over the documented input range.

/// `(i + 1) % len`, for `i < len`.
#[inline(always)]
pub fn next(i: usize, len: usize) -> usize {
    let j = i + 1;
    if j >= len {
        j - len
    } else {
        j
    }
}

/// `(i + len - d) % len` — the slot `d` behind `i` — for `i < len` and `d <= len`.
#[inline(always)]
pub fn back(i: usize, d: usize, len: usize) -> usize {
    let j = i + len - d;
    if j >= len {
        j - len
    } else {
        j
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_modulo_it_replaces() {
        for len in [1usize, 2, 3, 7, 96_000] {
            let is: Vec<usize> = if len < 64 {
                (0..len).collect()
            } else {
                vec![0, 1, 2, len / 2, len - 2, len - 1]
            };
            for &i in &is {
                assert_eq!(next(i, len), (i + 1) % len, "next({i}, {len})");
                let ds: Vec<usize> = if len < 64 {
                    (0..=len).collect()
                } else {
                    vec![0, 1, 2, i, len / 3, len - 2, len - 1, len]
                };
                for d in ds {
                    assert_eq!(
                        back(i, d, len),
                        (i + len - d) % len,
                        "back({i}, {d}, {len})"
                    );
                }
            }
        }
    }
}
