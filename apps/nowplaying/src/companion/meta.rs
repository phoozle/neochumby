//! shairport-sync's metadata pipe: items written one after another as
//!
//!   <item><type>73736e63</type><code>6d696e6d</code><length>5</length>
//!   <data encoding="base64">
//!   SGVsbG8=</data></item>
//!
//! the type and code four-character codes in hex (`ssnc`, its own; `core`,
//! the sender's DAAP fields), the data base64, left out when the length is
//! 0. The codes are listed in shairport-sync's "METADATA CODES.md". This
//! takes the bytes as they come and hands back whole items; anything that
//! doesn't parse is skipped up to the next `<item>`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The most this holds while an item arrives: cover art comes as one item,
/// and a few megabytes of base64 is already far more than any.
const MAX_ITEM: usize = 8 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// `ssnc` or `core`.
    pub kind: [u8; 4],
    pub code: [u8; 4],
    pub data: Vec<u8>,
}

impl Item {
    #[cfg(test)]
    pub fn is(&self, kind: &[u8; 4], code: &[u8; 4]) -> bool {
        &self.kind == kind && &self.code == code
    }

    /// The data as text (shairport-sync passes strings without a NUL).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// What lies between `<tag>` and `</tag>` in `s`.
fn field<'a>(s: &'a [u8], tag: &str) -> Option<&'a [u8]> {
    let open = format!("<{tag}");
    let start = find(s, open.as_bytes())?;
    let body = start + find(&s[start..], b">")? + 1;
    let end = body + find(&s[body..], format!("</{tag}>").as_bytes())?;
    Some(&s[body..end])
}

fn code(hex: &[u8]) -> Option<[u8; 4]> {
    let v = u32::from_str_radix(std::str::from_utf8(hex).ok()?.trim(), 16).ok()?;
    Some(v.to_be_bytes())
}

/// One item's insides, or None if they don't add up.
fn item(s: &[u8]) -> Option<Item> {
    let kind = code(field(s, "type")?)?;
    let code = code(field(s, "code")?)?;
    let length: usize = std::str::from_utf8(field(s, "length")?).ok()?.trim().parse().ok()?;
    let data = match field(s, "data") {
        Some(b64) => {
            let b64: Vec<u8> = b64.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            STANDARD.decode(b64).ok()?
        }
        None => Vec::new(),
    };
    (data.len() == length).then_some(Item { kind, code, data })
}

impl Parser {
    /// The items `data` completes.
    pub fn push(&mut self, data: &[u8]) -> Vec<Item> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let Some(start) = find(&self.buf, b"<item>") else {
                // Nothing begun: keep only what could be the start of one.
                let keep = self.buf.len().min(5);
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            self.buf.drain(..start);
            let Some(end) = find(&self.buf, b"</item>") else {
                if self.buf.len() > MAX_ITEM {
                    // Never ending: skip to whatever item comes next.
                    self.buf.drain(..6);
                    continue;
                }
                break;
            };
            // An item that lost its end runs into the next: take the last
            // start before this end.
            let inner_start = self.buf[..end].windows(6).rposition(|w| w == b"<item>").unwrap_or(0);
            if let Some(i) = item(&self.buf[inner_start + 6..end]) {
                out.push(i);
            }
            self.buf.drain(..end + 7);
        }
        out
    }
}

/// An item as shairport-sync writes it, for tests and the recorded
/// sample's maker.
#[cfg(test)]
pub fn encode(kind: &[u8; 4], code: &[u8; 4], data: &[u8]) -> String {
    let mut s = format!(
        "<item><type>{:x}</type><code>{:x}</code><length>{}</length>",
        u32::from_be_bytes(*kind),
        u32::from_be_bytes(*code),
        data.len()
    );
    if !data.is_empty() {
        s += &format!("\n<data encoding=\"base64\">\n{}</data>", STANDARD.encode(data));
    }
    s + "</item>\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorded session's worth of metadata, made up: a sender connects,
    /// a track's details come in a bundle, progress, a pause and a resume,
    /// and the end. The cover art is left out (tests make their own).
    const SAMPLE: &str = include_str!("testdata/session.xml");

    #[test]
    fn the_recorded_sample() {
        let items = Parser::default().push(SAMPLE.as_bytes());
        let codes: Vec<String> = items
            .iter()
            .map(|i| format!("{}/{}", String::from_utf8_lossy(&i.kind), String::from_utf8_lossy(&i.code)))
            .collect();
        assert_eq!(
            codes,
            [
                "ssnc/snam",
                "ssnc/clip",
                "ssnc/daid",
                "ssnc/acre",
                "ssnc/pbeg",
                "ssnc/mdst",
                "core/minm",
                "core/asar",
                "core/asal",
                "core/astm",
                "ssnc/mden",
                "ssnc/prgr",
                "ssnc/dapo",
                "ssnc/pfls",
                "ssnc/prsm",
                "ssnc/pend"
            ]
        );
        let title = items.iter().find(|i| i.is(b"core", b"minm")).unwrap();
        assert_eq!(title.text(), "Sample Track (Live)");
        let artist = items.iter().find(|i| i.is(b"core", b"asar")).unwrap();
        assert_eq!(artist.text(), "Les Échantillons");
        let length = items.iter().find(|i| i.is(b"core", b"astm")).unwrap();
        assert_eq!(length.data, 215_000u32.to_be_bytes());
        assert!(items.iter().find(|i| i.is(b"ssnc", b"pbeg")).unwrap().data.is_empty());
    }

    #[test]
    fn bytes_one_at_a_time() {
        let mut p = Parser::default();
        let mut n = 0;
        for b in SAMPLE.as_bytes() {
            n += p.push(std::slice::from_ref(b)).len();
        }
        assert_eq!(n, 16);
    }

    #[test]
    fn a_large_picture_in_one_item() {
        let pict: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        let s = encode(b"ssnc", b"PICT", &pict);
        let mut p = Parser::default();
        let mut items = Vec::new();
        for chunk in s.as_bytes().chunks(4096) {
            items.extend(p.push(chunk));
        }
        assert_eq!(items, [Item { kind: *b"ssnc", code: *b"PICT", data: pict }]);
    }

    #[test]
    fn what_doesnt_parse_is_skipped() {
        let mut s = String::from("noise<item><type>zz</type></item>\n");
        // A length that disagrees with the data.
        s += "<item><type>73736e63</type><code>6d696e6d</code><length>9</length>\n<data encoding=\"base64\">\nSGVsbG8=</data></item>\n";
        // An item cut off, then a whole one.
        s += "<item><type>73736e63</type><code>";
        s += &encode(b"ssnc", b"pend", b"");
        let items = Parser::default().push(s.as_bytes());
        assert_eq!(items, [Item { kind: *b"ssnc", code: *b"pend", data: vec![] }]);
    }
}
