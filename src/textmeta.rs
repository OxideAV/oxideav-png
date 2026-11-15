//! Text chunks as the container's string metadata.
//!
//! The demuxer reports each `tEXt` / `zTXt` / `iTXt` chunk (other than
//! the XMP packet, which is a blob) as one `(key, value)` pair, in file
//! order; the muxer writes the pairs it is handed back as text chunks.
//! The keywords W3C PNG3 §11.3.3.2 predefines map onto the framework's
//! lower-case vocabulary, every other keyword is its own key:
//!
//! | keyword | key |
//! |---|---|
//! | `Title` | `title` |
//! | `Author` | `artist` |
//! | `Description` | `description` |
//! | `Copyright` | `copyright` |
//! | `Creation Time` | `date` |
//! | `Software` | `software` |
//! | `Disclaimer` | `disclaimer` |
//! | `Warning` | `warning` |
//! | `Source` | `source` |
//! | `Comment` | `comment` |

use crate::chunk::{ChunkRef, PNG_MAGIC, write_chunk};
use crate::decoder::parse_all_chunks;
use crate::error::Result;
use crate::metadata::{Itxt, Text, Ztxt};
use crate::sideinfo::XMP_KEYWORD;

/// Predefined keyword ↔ framework key.
const KEYWORD_KEYS: [(&str, &str); 10] = [
    ("Title", "title"),
    ("Author", "artist"),
    ("Description", "description"),
    ("Copyright", "copyright"),
    ("Creation Time", "date"),
    ("Software", "software"),
    ("Disclaimer", "disclaimer"),
    ("Warning", "warning"),
    ("Source", "source"),
    ("Comment", "comment"),
];

/// A text value this long or longer is written compressed (`zTXt`, or
/// `iTXt` with the compression flag): W3C PNG3 §11.3.3.3 recommends the
/// compressed chunk "for storing large blocks of text".
const COMPRESS_FROM: usize = 1024;

/// The metadata key of a text chunk keyword.
pub(crate) fn key_of_keyword(keyword: &str) -> String {
    KEYWORD_KEYS
        .iter()
        .find(|(k, _)| *k == keyword)
        .map_or_else(|| keyword.to_string(), |(_, key)| (*key).to_string())
}

/// The keyword a metadata key is written under: the predefined keyword
/// for a key of the table, else the key itself when it is a valid
/// keyword (1–79 Latin-1 printable characters, no leading, trailing or
/// doubled space; W3C PNG3 §11.3.3.2). `None` for a key PNG cannot
/// spell.
pub(crate) fn keyword_of_key(key: &str) -> Option<String> {
    let keyword = KEYWORD_KEYS
        .iter()
        .find(|(_, k)| *k == key)
        .map_or(key, |(keyword, _)| *keyword);
    crate::metadata::validate_keyword(keyword, "tEXt")
        .is_ok()
        .then(|| keyword.to_string())
}

/// The `(key, value)` pair of a text chunk; `None` for any other chunk,
/// the XMP `iTXt`, and a text chunk that does not parse.
fn pair_of(c: &ChunkRef<'_>) -> Option<(String, String)> {
    let (keyword, text) = match &c.chunk_type {
        b"tEXt" => Text::parse(c.data).map(|t| (t.keyword, t.text)).ok()?,
        b"zTXt" => Ztxt::parse(c.data).map(|t| (t.keyword, t.text)).ok()?,
        b"iTXt" => {
            let t = Itxt::parse(c.data).ok()?;
            if t.keyword == XMP_KEYWORD {
                return None;
            }
            (t.keyword, t.text)
        }
        _ => return None,
    };
    Some((key_of_keyword(&keyword), text))
}

/// Every text chunk of a chunk list as a metadata pair, in file order.
pub(crate) fn pairs_from_chunks(chunks: &[ChunkRef<'_>]) -> Vec<(String, String)> {
    chunks.iter().filter_map(pair_of).collect()
}

/// The text chunk `(type, body)` a pair is written as: `tEXt` when the
/// value is Latin-1 (`zTXt` from [`COMPRESS_FROM`] bytes), else `iTXt`
/// (compressed from the same length). `None` when the key has no
/// keyword or the value cannot be stored (it holds a NUL).
fn chunk_of_pair(key: &str, value: &str) -> Option<([u8; 4], Vec<u8>)> {
    let keyword = keyword_of_key(key)?;
    let long = value.len() >= COMPRESS_FROM;
    if value.chars().all(|c| (c as u32) <= 0xFF) {
        let latin1 = if long {
            Ztxt::new(keyword.clone(), value.to_string())
                .to_bytes()
                .map(|b| (*b"zTXt", b))
        } else {
            Text::new(keyword.clone(), value.to_string())
                .to_bytes()
                .map(|b| (*b"tEXt", b))
        };
        if let Ok(chunk) = latin1 {
            return Some(chunk);
        }
    }
    Itxt::new(keyword, value.to_string())
        .with_compressed(long)
        .to_bytes()
        .ok()
        .map(|b| (*b"iTXt", b))
}

/// `true` when [`apply_pairs`] stores a pair under `key`.
pub(crate) fn carries_key(key: &str) -> bool {
    keyword_of_key(key).is_some()
}

/// `file` with its text chunks brought in line with `pairs`.
///
/// Each key `pairs` names is decided by `pairs`: a text chunk of the
/// file under that key stays, untouched (its chunk type, language tag
/// and compression as they are), when a pair repeats its value, and is
/// removed otherwise; a pair no chunk answers is written as a new text
/// chunk ahead of the image data. Text chunks under keys `pairs` does
/// not name are left alone, so a file that already carries its text
/// (an encoder's packet, a remuxed file) is not doubled by handing the
/// muxer the pairs the demuxer reported. A pair whose key or value PNG
/// cannot store is skipped.
pub(crate) fn apply_pairs(file: &[u8], pairs: &[(String, String)]) -> Result<Vec<u8>> {
    if pairs.is_empty() {
        return Ok(file.to_vec());
    }
    let chunks = parse_all_chunks(file)?;
    // Settle every text chunk first: `keep[i]` is false for a chunk
    // under a named key that no (not yet answered) pair repeats.
    let mut unused: Vec<bool> = vec![true; pairs.len()];
    let keep: Vec<bool> = chunks
        .iter()
        .map(|c| {
            let Some((key, value)) = pair_of(c) else {
                return true;
            };
            if !pairs.iter().any(|(k, _)| *k == key) {
                return true;
            }
            let answer =
                (0..pairs.len()).find(|&i| unused[i] && pairs[i].0 == key && pairs[i].1 == value);
            match answer {
                Some(i) => {
                    unused[i] = false;
                    true
                }
                None => false,
            }
        })
        .collect();
    let mut out = Vec::with_capacity(file.len() + 64);
    out.extend_from_slice(&PNG_MAGIC);
    let mut inserted = false;
    for (c, keep) in chunks.iter().zip(keep) {
        if !inserted && matches!(&c.chunk_type, b"IDAT" | b"fcTL" | b"IEND") {
            for ((key, value), _) in pairs.iter().zip(&unused).filter(|(_, u)| **u) {
                if let Some((ty, body)) = chunk_of_pair(key, value) {
                    write_chunk(&mut out, &ty, &body);
                }
            }
            inserted = true;
        }
        if keep {
            write_chunk(&mut out, &c.chunk_type, c.data);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_keywords_map_both_ways() {
        for (keyword, key) in KEYWORD_KEYS {
            assert_eq!(key_of_keyword(keyword), key);
            assert_eq!(keyword_of_key(key).as_deref(), Some(keyword));
        }
        assert_eq!(key_of_keyword("Camera"), "Camera");
        assert_eq!(keyword_of_key("Camera").as_deref(), Some("Camera"));
        assert_eq!(keyword_of_key(""), None);
        assert_eq!(keyword_of_key(" lead"), None);
        assert_eq!(keyword_of_key(&"k".repeat(80)), None);
        assert_eq!(keyword_of_key("two  spaces"), None);
    }

    #[test]
    fn value_picks_the_narrowest_chunk() {
        assert_eq!(chunk_of_pair("title", "plain").unwrap().0, *b"tEXt");
        assert_eq!(chunk_of_pair("title", "caf\u{e9}").unwrap().0, *b"tEXt");
        assert_eq!(
            chunk_of_pair("title", &"x".repeat(COMPRESS_FROM))
                .unwrap()
                .0,
            *b"zTXt"
        );
        assert_eq!(chunk_of_pair("title", "\u{540d}").unwrap().0, *b"iTXt");
        assert!(chunk_of_pair("title", "nul\0inside").is_none());
        assert!(chunk_of_pair("bad  key", "v").is_none());
    }
}
