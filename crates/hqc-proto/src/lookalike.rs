//! Text that reads as one of the hub's or the worker's own marks, and fields that could forge
//! one (SPEC §7.1, §7.1.1).
//!
//! A line that opens a block the hub or the worker wrote starts with a mark (`[hqc room`,
//! `[hqc peer`). A model reads a fullwidth `［ｈｑｃ　ｒｏｏｍ`, a `[hqc` and `room` with a
//! no-break space or two spaces between them, or a mark with a zero-width or bidi character
//! inside as the mark itself, so [`escape`] compares folded text: invisible characters
//! ([`is_invisible`]) are dropped, every other character is folded by NFKC (one character at a
//! time) and lowercased, and a run of whitespace, the Hangul fillers (a blank on the screen)
//! included, is one space. Letters of another script that look alike (Cyrillic `с`, `о`) and
//! lookalike brackets (`【`) are not folded: NFKC does not map them to the mark's characters.

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// The Hangul fillers: Default_Ignorable_Code_Point, but shown as a blank, so they count as
/// whitespace in [`escape`] and in a name, and are not [`is_invisible`].
pub fn is_filler(c: char) -> bool {
    matches!(c, '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}')
}

/// The Default_Ignorable_Code_Point ranges of Unicode's DerivedCoreProperties.txt, the Hangul
/// fillers ([`is_filler`]) aside.
const DEFAULT_IGNORABLE: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),
    ('\u{034F}', '\u{034F}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{17B4}', '\u{17B5}'),
    ('\u{180B}', '\u{180F}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{206F}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF0}', '\u{FFF8}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0000}', '\u{E0FFF}'),
];

/// Whether `c` shows nothing: of Unicode category Cf (zero-width, bidi, soft hyphen, BOM, ...)
/// or a Default_Ignorable_Code_Point (combining grapheme joiner, variation selectors, ...), the
/// Hangul fillers aside. Text with one inside reads as text without it.
pub fn is_invisible(c: char) -> bool {
    c.general_category() == GeneralCategory::Format
        || DEFAULT_IGNORABLE
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// Whether the NFKC form of `c` contains one of `set` (`［` folds to `[`, `｜` to `|`, `＂` to `"`).
pub fn folds_to_any(c: char, set: &[char]) -> bool {
    std::iter::once(c).nfkc().any(|f| set.contains(&f))
}

/// `text` with every place that reads as `mark` replaced, from its first character to its last
/// (invisible characters inside it go with it), by `escaped`; the rest is kept byte for byte. `mark` is
/// lowercase ASCII with one space between its words; "reads as" is in the module's header.
/// Escaping again changes nothing as long as `escaped` does not read as `mark`.
pub fn escape(text: &str, mark: &str, escaped: &str) -> String {
    // The folded characters of `text`, each with the span of the source character it came from;
    // a run of whitespace is one entry spanning the run.
    let mut folded: Vec<(char, usize, usize)> = Vec::new();
    let mut in_space = false;
    for (start, c) in text.char_indices() {
        if is_invisible(c) {
            continue;
        }
        let end = start + c.len_utf8();
        for f in std::iter::once(c).nfkc().flat_map(char::to_lowercase) {
            if f.is_whitespace() || is_filler(f) {
                if in_space {
                    if let Some(last) = folded.last_mut() {
                        last.2 = end;
                    }
                } else {
                    folded.push((' ', start, end));
                    in_space = true;
                }
            } else {
                folded.push((f, start, end));
                in_space = false;
            }
        }
    }
    let mark: Vec<char> = mark.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i + mark.len() <= folded.len() {
        if folded[i..i + mark.len()]
            .iter()
            .map(|f| f.0)
            .eq(mark.iter().copied())
        {
            let (from, to) = (folded[i].1, folded[i + mark.len() - 1].2);
            // A match inside the span of one character that folds to several, already replaced.
            if from < copied {
                i += 1;
                continue;
            }
            out.push_str(&text[copied..from]);
            out.push_str(escaped);
            copied = to;
            i += mark.len();
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: &str = "[hqc room";
    const ESCAPED: &str = "[hqc-room-text";

    fn e(text: &str) -> String {
        escape(text, MARK, ESCAPED)
    }

    #[test]
    fn escapes_the_mark_in_any_case() {
        assert_eq!(e("plain"), "plain");
        assert_eq!(e("ünï [hqc room ✓"), "ünï [hqc-room-text ✓");
        assert_eq!(e("[[HQC Room"), "[[hqc-room-text");
        assert_eq!(e("[hqc roo"), "[hqc roo");
        assert_eq!(e(&e("[hqc room")), "[hqc-room-text");
    }

    #[test]
    fn escapes_what_reads_as_the_mark() {
        for lookalike in [
            "［hqc room",
            "[hqc\u{3000}room",
            "[hqc\u{a0}room",
            "[hqc  room",
            "[hqc \t room",
            "[hq\u{200b}c room",
            "[\u{202e}hqc room",
            "[hqc room\u{200b}",
            "[hq\u{34f}c room",
            "[hqc\u{fe0f} room",
            "[hqc\u{3164}room",
            "[hqc\u{ffa0}\u{1160} room",
            "[hqc\u{e0100} room",
            "[ｈｑｃ ｒｏｏｍ",
            "[ＨＱＣ　ＲＯＯＭ",
        ] {
            let out = e(&format!("a {lookalike} b"));
            assert!(
                out == "a [hqc-room-text b" || out == "a [hqc-room-text\u{200b} b",
                "{lookalike:?} -> {out:?}"
            );
        }
        // Only the mark goes; the characters around it stay.
        assert_eq!(e("x\u{200b}[hqc  room]y"), "x\u{200b}[hqc-room-text]y");
    }

    #[test]
    fn a_match_inside_one_expanding_character_is_not_cut_twice() {
        // `㎜` folds to `mm`: both folded characters come from one source character.
        assert_eq!(escape("㎜", "m", "x"), "x");
        assert_eq!(escape("a㎜b", "mm", "x"), "axb");
    }

    #[test]
    fn a_name_is_checked_character_by_character() {
        for c in [
            '\u{200b}',
            '\u{202e}',
            '\u{ad}',
            '\u{34f}',
            '\u{fe0f}',
            '\u{e0100}',
            '\u{180b}',
        ] {
            assert!(is_invisible(c), "{c:?}");
        }
        for c in [
            'a', ' ', '\u{3000}', '\u{3164}', '\u{ffa0}', '\u{115f}', '\u{1160}',
        ] {
            assert!(!is_invisible(c), "{c:?}");
        }
        assert!(folds_to_any('［', &['[']) && folds_to_any('｜', &['|']));
        assert!(folds_to_any('＂', &['"']) && folds_to_any('］', &[']']));
        assert!(!folds_to_any('a', &['[']) && !folds_to_any('ü', &['[', ']']));
    }
}
