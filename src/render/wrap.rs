//! Display-width aware wrapping. The chat renderer hard-wraps every block to the
//! viewport width so each `RenderedLine` corresponds to exactly one screen row.
//! That makes cursor math, the scrollbar, and virtualization exact (no relying
//! on the widget's own wrapping, which would desync line indices).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Word-wrap prose to `width` columns. Over-long words are hard-broken.
/// Always returns at least one (possibly empty) line.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;

    for word in text.split(' ') {
        let ww = UnicodeWidthStr::width(word);
        if ww > width {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            let chunks = hard_chunks(word, width);
            let n = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                if i + 1 == n {
                    cur_w = UnicodeWidthStr::width(chunk.as_str());
                    cur = chunk;
                } else {
                    lines.push(chunk);
                }
            }
            continue;
        }
        let add = if cur.is_empty() { ww } else { ww + 1 };
        if cur_w + add > width {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_w = ww;
        } else {
            if !cur.is_empty() {
                cur.push(' ');
                cur_w += 1;
            }
            cur.push_str(word);
            cur_w += ww;
        }
    }
    lines.push(cur);
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Hard-break a string into chunks no wider than `width` columns.
pub fn hard_chunks(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_one_line() {
        assert_eq!(wrap_words("hello world", 40), vec!["hello world"]);
    }

    #[test]
    fn wraps_on_word_boundary() {
        assert_eq!(
            wrap_words("the quick brown fox", 9),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn empty_text_is_one_empty_line() {
        assert_eq!(wrap_words("", 10), vec![""]);
    }

    #[test]
    fn long_word_is_hard_broken() {
        assert_eq!(wrap_words("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn long_word_after_text() {
        // "hi " then a 6-wide word into width 4 → "hi", then chunks
        let out = wrap_words("hi abcdefg", 4);
        assert_eq!(out, vec!["hi", "abcd", "efg"]);
    }

    #[test]
    fn hard_chunks_respects_width() {
        assert_eq!(hard_chunks("aaaaa", 2), vec!["aa", "aa", "a"]);
    }

    #[test]
    fn never_exceeds_width() {
        let text = "lorem ipsum dolor sit amet consectetur adipiscing elit";
        for line in wrap_words(text, 12) {
            assert!(UnicodeWidthStr::width(line.as_str()) <= 12, "line too wide: {:?}", line);
        }
    }
}
