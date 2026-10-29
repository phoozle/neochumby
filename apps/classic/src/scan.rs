//! The C library's `sscanf` and `atoi`, as far as the manifest and the
//! control commands use them. The feeder's manifests were written for the C
//! engine, which read them with sscanf, so the same rules apply here,
//! including the odd ones: `%23s` stops after 23 bytes and the next
//! conversion carries on from there, `%d` reads the digits at the start of
//! a word and ignores the rest, and literal text must match exactly.

/// One converted value.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    I(i64),
    S(String),
}

impl V {
    /// An integer conversion's value, as a C `int`.
    pub fn i(&self) -> i32 {
        match self {
            V::I(v) => *v as i32,
            V::S(_) => 0,
        }
    }

    /// An integer conversion's value, as a C `long` on the host.
    pub fn l(&self) -> i64 {
        match self {
            V::I(v) => *v,
            V::S(_) => 0,
        }
    }

    pub fn s(&self) -> &str {
        match self {
            V::S(s) => s,
            V::I(_) => "",
        }
    }
}

fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// Reads `input` by `fmt` (`%d`, `%ld`, `%Ns`, `%s`, `%*d`, `%*s`, spaces
/// and literal text) and returns the values assigned before the first
/// conversion that failed; its length is what sscanf returns.
pub fn scan(input: &[u8], fmt: &str) -> Vec<V> {
    let f = fmt.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut k) = (0, 0);
    while k < f.len() {
        let c = f[k];
        if space(c) {
            while i < input.len() && space(input[i]) {
                i += 1;
            }
            k += 1;
            continue;
        }
        if c != b'%' {
            if i >= input.len() || input[i] != c {
                return out;
            }
            i += 1;
            k += 1;
            continue;
        }
        k += 1;
        let skip = f.get(k) == Some(&b'*');
        if skip {
            k += 1;
        }
        let mut width = 0usize;
        while k < f.len() && f[k].is_ascii_digit() {
            width = width * 10 + (f[k] - b'0') as usize;
            k += 1;
        }
        if f.get(k) == Some(&b'l') {
            k += 1;
        }
        let conv = f.get(k).copied().unwrap_or(0);
        k += 1;
        while i < input.len() && space(input[i]) {
            i += 1;
        }
        match conv {
            b'd' => {
                let neg = matches!(input.get(i), Some(b'-'));
                if matches!(input.get(i), Some(b'-' | b'+')) {
                    i += 1;
                }
                let digits = i;
                let mut v: i64 = 0;
                while i < input.len() && input[i].is_ascii_digit() {
                    v = v.wrapping_mul(10).wrapping_add((input[i] - b'0') as i64);
                    i += 1;
                }
                if i == digits {
                    return out;
                }
                if !skip {
                    out.push(V::I(if neg { -v } else { v }));
                }
            }
            b's' => {
                let start = i;
                let max = if width == 0 { usize::MAX } else { width };
                while i < input.len() && !space(input[i]) && i - start < max {
                    i += 1;
                }
                if i == start {
                    return out;
                }
                if !skip {
                    out.push(V::S(String::from_utf8_lossy(&input[start..i]).into_owned()));
                }
            }
            _ => return out,
        }
    }
    out
}

/// C's atoi: leading white space, a sign, then digits; 0 if there are none.
pub fn atoi(s: &[u8]) -> i32 {
    let mut i = 0;
    while i < s.len() && space(s[i]) {
        i += 1;
    }
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut v: i32 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v.wrapping_mul(10).wrapping_add((s[i] - b'0') as i32);
        i += 1;
    }
    if neg { v.wrapping_neg() } else { v }
}

/// Splits like `strtok(s, "\n")`: the lines, without the empty ones.
pub fn lines(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    s.split(|&b| b == b'\n').filter(|l| !l.is_empty())
}

/// C's strstr(line, word) != 0.
pub fn contains(line: &[u8], word: &str) -> bool {
    line.windows(word.len()).any(|w| w == word.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        let v = scan(b"page 3 news_slide.raw 12 noclock", "page %d %63s %d");
        assert_eq!(v, vec![V::I(3), V::S("news_slide.raw".into()), V::I(12)]);
        // A literal prefix matches the start of a longer word, as sscanf's does.
        assert_eq!(scan(b"pagenight 0 x.raw", "page %d %63s %d"), vec![]);
        assert_eq!(scan(b"clocknight c.raw", "clock %63s %d %d %d %d"), vec![V::S("night".into())]);
        // %d stops at the first non-digit.
        assert_eq!(scan(b"a 12x", "a %d"), vec![V::I(12)]);
        assert_eq!(scan(b"a -7 +8", "a %d %d"), vec![V::I(-7), V::I(8)]);
        // A width cuts a word, and the next conversion starts where it stopped.
        assert_eq!(scan(b"abcdef 1", "%3s %s"), vec![V::S("abc".into()), V::S("def".into())]);
        // Skipped conversions assign nothing.
        assert_eq!(
            scan(b"hot home 1 2 3 4 screen music", "hot %*s %*d %*d %*d %*d %*s %23s"),
            vec![V::S("music".into())]
        );
        // Leading spaces in the input don't match a literal.
        assert_eq!(scan(b" page 1 a 2", "page %d %63s %d"), vec![]);
        assert_eq!(scan(b"sun 1793130912 1793178706", "sun %ld %ld"), vec![V::I(1793130912), V::I(1793178706)]);
    }

    #[test]
    fn atoi_like_c() {
        assert_eq!(atoi(b" 12 13"), 12);
        assert_eq!(atoi(b"-4"), -4);
        assert_eq!(atoi(b"x"), 0);
        assert_eq!(atoi(b""), 0);
    }

    #[test]
    fn strtok_lines() {
        let l: Vec<&[u8]> = lines(b"\n\na\nb\n\nc").collect();
        assert_eq!(l, vec![&b"a"[..], b"b", b"c"]);
    }
}
