//! The picture cache: every file the manifest names is an asset, read when
//! a page or screen that draws it is shown (or prefetched before), and kept
//! under a byte budget. When something new needs room, the least recently
//! shown assets go first; what is on screen (protected) never goes, and is
//! allowed over budget if it must be.
//!
//! Assets are identified by content: the md5 the pusher writes to
//! hashes.txt, else name, size and mtime. A file shared by several screens
//! is read once, and after a new manifest every unchanged file stays in
//! memory.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Not in memory.
    Empty,
    /// On its way from the loader; its memory is already counted.
    Loading,
    Ready,
    /// Missing, short or unreadable: drawn as nothing.
    Bad,
}

pub struct Asset {
    pub name: String,
    /// Content id: an md5 prefix (odd) or a hash of name, size and mtime (even).
    pub key: u64,
    pub size: usize,
    /// The file, as 16-bit words (the pictures are RGB565), the last one
    /// padded if the size is odd.
    pub data: Option<Vec<u16>>,
    pub state: State,
    /// Bumped on every completed load, for derived data.
    pub ser: u32,
    /// LRU clock: when its page or screen was last shown, in ms.
    pub used: i64,
    /// Equal to the cache's `prot_gen`: protected, not evictable now.
    pub prot: u32,
}

impl Asset {
    pub fn new(name: &str, key: u64, size: usize) -> Asset {
        Asset { name: name.to_string(), key, size, data: None, state: State::Empty, ser: 0, used: 0, prot: 0 }
    }
}

pub struct Cache {
    pub assets: Vec<Asset>,
    /// Memory held by assets' data, in bytes.
    pub bytes: usize,
    pub budget: usize,
    pub prot_gen: u32,
    pub load_ser: u32,
}

/// A buffer of `size` bytes, as words; None if there isn't the memory.
pub fn alloc(size: usize) -> Option<Vec<u16>> {
    let mut v = Vec::new();
    v.try_reserve_exact(size.div_ceil(2)).ok()?;
    v.resize(size.div_ceil(2), 0);
    Some(v)
}

/// Byte `k` of the file held in words (little-endian pairs).
#[inline]
pub fn byte(w: &[u16], k: usize) -> u8 {
    (w[k / 2] >> (8 * (k & 1))) as u8
}

/// Puts `src`, the file's bytes from offset `at`, into its words.
pub fn put_bytes(w: &mut [u16], mut at: usize, mut src: &[u8]) {
    if at % 2 == 1
        && let Some((&b, rest)) = src.split_first()
    {
        w[at / 2] = (w[at / 2] & 0x00FF) | (b as u16) << 8;
        at += 1;
        src = rest;
    }
    let (pairs, rest) = src.as_chunks::<2>();
    let odd = rest.first().copied();
    for (d, p) in w[at / 2..].iter_mut().zip(pairs) {
        *d = u16::from_le_bytes(*p);
    }
    if let Some(b) = odd {
        let i = (at + src.len()) / 2;
        w[i] = (w[i] & 0xFF00) | b as u16;
    }
}

impl Cache {
    pub fn new(budget: usize) -> Cache {
        Cache { assets: Vec::new(), bytes: 0, budget, prot_gen: 0, load_ser: 0 }
    }

    /// The asset's pixels, if it is in memory and good.
    pub fn px(&self, i: Option<usize>) -> Option<&[u16]> {
        let a = self.assets.get(i?)?;
        if a.state == State::Ready { a.data.as_deref() } else { None }
    }

    /// The asset's words and its size in bytes (see [`byte`]), if it is in
    /// memory and good.
    pub fn bytes(&self, i: Option<usize>) -> Option<(&[u16], usize)> {
        let a = self.assets.get(i?)?;
        if a.state != State::Ready {
            return None;
        }
        Some((a.data.as_deref()?, a.size))
    }

    pub fn state(&self, i: usize) -> State {
        self.assets[i].state
    }

    /// Frees its memory, keeping the entry (and its state).
    pub fn drop_data(&mut self, i: usize) {
        if self.holds(i) {
            self.bytes -= self.assets[i].size;
        }
        self.assets[i].data = None;
        if self.assets[i].state == State::Loading {
            self.assets[i].state = State::Empty;
        }
    }

    /// Whether its size is counted: it is in memory, or on its way.
    pub fn holds(&self, i: usize) -> bool {
        let a = &self.assets[i];
        a.data.is_some() || a.state == State::Loading
    }

    /// Counts its memory from now on, while it is read (the reader makes
    /// the buffer, so a load never holds two).
    pub fn start_load(&mut self, i: usize) {
        if self.assets[i].state != State::Empty {
            self.drop_data(i);
        }
        self.assets[i].state = State::Loading;
        self.bytes += self.assets[i].size;
    }

    /// The load finished: `data` holds the file.
    pub fn finish_load(&mut self, i: usize, data: Vec<u16>) {
        if !self.holds(i) {
            self.bytes += self.assets[i].size;
        }
        self.load_ser += 1;
        let a = &mut self.assets[i];
        a.data = Some(data);
        a.state = State::Ready;
        a.ser = self.load_ser;
    }

    pub fn mark_bad(&mut self, i: usize) {
        self.drop_data(i);
        self.assets[i].state = State::Bad;
    }

    /// Takes it out of memory; it is read again when next needed.
    pub fn evict(&mut self, i: usize) {
        self.drop_data(i);
        if self.assets[i].state != State::Bad {
            self.assets[i].state = State::Empty;
        }
    }

    /// Starts a new protection round: what is protected after this, and
    /// before the next, is safe from eviction.
    pub fn new_round(&mut self) {
        self.prot_gen = self.prot_gen.wrapping_add(1);
    }

    pub fn protect(&mut self, i: usize) {
        self.assets[i].prot = self.prot_gen;
    }

    pub fn protected(&self, i: usize) -> bool {
        self.assets[i].prot == self.prot_gen
    }

    /// Makes room for `n` more bytes by evicting the least recently shown
    /// unprotected assets, whose indices go to `evicted`. False if only
    /// protected ones are left; then it goes over budget only if `force`.
    pub fn make_room(&mut self, n: usize, force: bool, evicted: &mut Vec<usize>) -> bool {
        while self.bytes + n > self.budget {
            let mut best: Option<usize> = None;
            for (i, a) in self.assets.iter().enumerate() {
                if !(a.data.is_some() || a.state == State::Loading) || a.prot == self.prot_gen {
                    continue;
                }
                if best.is_none_or(|b| a.used < self.assets[b].used) {
                    best = Some(i);
                }
            }
            let Some(b) = best else { return force };
            self.evict(b);
            evicted.push(b);
        }
        true
    }

    /// How many assets are in memory and good.
    pub fn ready(&self) -> usize {
        self.assets.iter().filter(|a| a.state == State::Ready).count()
    }

    /// The bytes of every file the manifest names.
    pub fn total(&self) -> usize {
        self.assets.iter().map(|a| a.size).sum()
    }
}

/// The content id hashes.txt gives a file: its md5's first 16 hex digits,
/// made odd so it never equals a fallback id. 0 if it isn't listed. Lines
/// are md5sum's, "<32 hex>  <name>".
#[cfg(test)]
pub fn md5_key(hashes: &[u8], name: &str) -> u64 {
    for l in hashes.split(|&b| b == b'\n') {
        if l.len() > 34 && &l[34..] == name.as_bytes() && l[32] == b' ' {
            let mut k = 0u64;
            for &c in &l[..16] {
                let v = match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    _ => return 0,
                };
                k = k << 4 | v as u64;
            }
            return k | 1;
        }
    }
    0
}

/// md5_key for every name in hashes.txt at once: what md5_key would give
/// each (the first line naming a file decides), in one pass.
pub fn md5_index(hashes: &[u8]) -> std::collections::HashMap<String, u64> {
    let mut m = std::collections::HashMap::new();
    for l in hashes.split(|&b| b == b'\n') {
        if l.len() > 34 && l[32] == b' ' {
            let name = String::from_utf8_lossy(&l[34..]).into_owned();
            let k = l[..16].iter().try_fold(0u64, |k, &c| {
                let v = match c {
                    b'0'..=b'9' => c - b'0',
                    b'a'..=b'f' => c - b'a' + 10,
                    _ => return None,
                };
                Some(k << 4 | v as u64)
            });
            m.entry(name).or_insert(k.map_or(0, |k| k | 1));
        }
    }
    m
}

/// The fallback content id: FNV-1a of the name, size and mtime, made even.
pub fn file_key(name: &str, size: u64, mtime: i64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.bytes().chain(size.to_le_bytes()).chain(mtime.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(sizes: &[usize], budget: usize) -> Cache {
        let mut c = Cache::new(budget);
        for (i, &s) in sizes.iter().enumerate() {
            c.assets.push(Asset::new(&format!("f{i}"), i as u64 * 2, s));
        }
        c
    }

    fn load(c: &mut Cache, i: usize, used: i64) {
        c.start_load(i);
        let d = alloc(c.assets[i].size).unwrap();
        c.finish_load(i, d);
        c.assets[i].used = used;
    }

    #[test]
    fn accounting() {
        let mut c = cache(&[100, 51], 1000);
        load(&mut c, 0, 1);
        load(&mut c, 1, 2);
        assert_eq!(c.bytes, 151);
        assert_eq!(c.bytes(Some(1)).unwrap().1, 51);
        assert_eq!(c.ready(), 2);
        c.evict(0);
        assert_eq!(c.bytes, 51);
        assert_eq!(c.state(0), State::Empty);
        assert!(c.px(Some(0)).is_none());
        c.mark_bad(1);
        assert_eq!(c.bytes, 0);
        c.evict(1);
        assert_eq!(c.state(1), State::Bad, "a bad file stays bad");
        // A load that was in flight and is finished counts once.
        c.start_load(0);
        assert_eq!(c.bytes, 100);
        c.finish_load(0, alloc(100).unwrap());
        assert_eq!(c.bytes, 100);
        assert_eq!(c.assets[0].ser, 3);
    }

    #[test]
    fn evicts_least_recently_shown_first() {
        let mut c = cache(&[100, 100, 100, 100], 300);
        load(&mut c, 0, 30);
        load(&mut c, 1, 10);
        load(&mut c, 2, 20);
        // Every caller starts a protection round first.
        c.new_round();
        let mut ev = Vec::new();
        assert!(c.make_room(100, false, &mut ev));
        assert_eq!(ev, vec![1]);
        assert!(c.make_room(200, false, &mut ev));
        assert_eq!(ev, vec![1, 2]);
        assert_eq!(c.bytes, 100);
    }

    #[test]
    fn protected_assets_stay() {
        let mut c = cache(&[100, 100, 100], 200);
        load(&mut c, 0, 1);
        load(&mut c, 1, 2);
        c.new_round();
        c.protect(0);
        c.protect(1);
        let mut ev = Vec::new();
        assert!(!c.make_room(100, false, &mut ev), "only protected ones left");
        assert!(ev.is_empty());
        assert!(c.make_room(100, true, &mut ev), "forced: over budget");
        // A new round drops the protection.
        c.new_round();
        c.protect(1);
        assert!(c.make_room(100, false, &mut ev));
        assert_eq!(ev, vec![0]);
    }

    #[test]
    fn bytes_into_words() {
        let mut w = vec![0u16; 4];
        put_bytes(&mut w, 0, &[1, 2, 3]);
        put_bytes(&mut w, 3, &[4, 5]);
        put_bytes(&mut w, 5, &[6, 7]);
        assert_eq!(w, vec![0x0201, 0x0403, 0x0605, 0x0007]);
        assert_eq!((0..7).map(|k| byte(&w, k)).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn content_ids() {
        let h = b"0123456789abcdef0123456789abcdef  page0.raw\nffffffffffffffff0123456789abcdef  x.raw\n";
        assert_eq!(md5_key(h, "page0.raw"), 0x0123456789abcdef);
        assert_eq!(md5_key(h, "x.raw"), u64::MAX);
        assert_eq!(md5_key(h, "page"), 0);
        assert_eq!(md5_key(h, "y.raw"), 0);
        assert_eq!(md5_key(b"0123456789abcdeg0123456789abcdef  z\n", "z"), 0);
        let ix = md5_index(h);
        assert_eq!((ix["page0.raw"], ix["x.raw"]), (md5_key(h, "page0.raw"), md5_key(h, "x.raw")));
        assert_eq!(md5_index(b"0123456789abcdeg0123456789abcdef  z\n")["z"], 0);
        let k = file_key("a.raw", 10, 5);
        assert_eq!(k & 1, 0);
        assert_ne!(k, file_key("a.raw", 10, 6));
    }
}
