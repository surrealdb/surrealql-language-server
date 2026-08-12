use ls_types::{Position, Range};

/// Line start offsets for one document, so a byte offset converts to an LSP
/// [`Position`] in `O(log lines)` instead of a scan from byte 0.
///
/// The scan mattered: [`offset_to_position`] is called once per emitted
/// semantic token and about ten times per extracted query fact, which made
/// both the highlight pass and `analyze_document` quadratic in file size. A
/// 3200-line file spent 6.3 s in `analyze_document` and walked 2.5 GB of text
/// to answer conversions alone.
///
/// Two properties keep the lookup cheap:
///
/// * `line_starts` is sorted by construction, so the line is a binary search.
/// * `all_ascii` records whether any byte is non-ASCII. For ASCII text the
///   UTF-16 column equals the byte column, so the common case needs no scan at
///   all. Otherwise only the bytes of the one line are counted, never the
///   bytes before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    /// Byte offset of the first byte of each line. Always starts with `0`, so
    /// it is never empty and the binary search always finds a line.
    line_starts: Vec<usize>,
    /// True when the source holds no byte above `0x7F`.
    all_ascii: bool,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self::new("")
    }
}

impl LineIndex {
    /// Read `source` one time and record where each line starts.
    pub fn new(source: &str) -> Self {
        let mut line_starts = Vec::with_capacity(source.len() / 40 + 1);
        line_starts.push(0);
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            line_starts,
            all_ascii: source.is_ascii(),
        }
    }

    /// The number of lines. A document always has at least one.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The line that holds `offset`, and the byte offset of that line's start.
    fn line_at(&self, offset: usize) -> (usize, usize) {
        // `Err(next)` means the offset falls inside the line before `next`.
        // `next` is never 0 because `line_starts[0]` is 0 and the search is
        // over a sorted list, so the subtraction cannot underflow.
        let line = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(next) => next - 1,
        };
        (line, self.line_starts[line])
    }

    /// Convert a byte offset to an LSP position.
    ///
    /// An offset past the end of `source` clamps to the end.
    ///
    /// An offset that points *inside* a multi-byte character counts that whole
    /// character, because [`offset_to_position`] counted every character whose
    /// start byte fell before the target. No caller reaches this case — every
    /// offset in the analyzer comes from a tree-sitter node boundary, and those
    /// are always character boundaries — but the old function is the only
    /// specification for the new one, so the edge matches it exactly.
    pub fn position(&self, source: &str, offset: usize) -> Position {
        let offset = offset.min(source.len());
        let (line, line_start) = self.line_at(offset);

        let character = if self.all_ascii {
            (offset - line_start) as u32
        } else {
            // Round up to a character boundary. Rounding up rather than down is
            // what reproduces the old count. A multi-byte character never holds
            // a `\n`, so this cannot cross into the next line.
            let mut end = offset;
            while end < source.len() && !source.is_char_boundary(end) {
                end += 1;
            }
            source[line_start..end]
                .chars()
                .map(|ch| ch.len_utf16() as u32)
                .sum()
        };

        Position::new(line as u32, character)
    }

    /// Convert an LSP position to a byte offset.
    ///
    /// Replaces [`position_to_offset`], which has the same defect as
    /// [`offset_to_position`]. A line past the end of the document returns the
    /// length of the source, and a character past the end of its line returns
    /// the offset of that line's terminator.
    pub fn offset(&self, source: &str, position: Position) -> usize {
        let Some(&line_start) = self.line_starts.get(position.line as usize) else {
            return source.len();
        };
        // The line ends where the next one starts, less its `\n`. The last
        // line has no successor and ends at the end of the source.
        let line_end = self
            .line_starts
            .get(position.line as usize + 1)
            .map(|next| next.saturating_sub(1))
            .unwrap_or(source.len());

        if self.all_ascii {
            return (line_start + position.character as usize).min(line_end);
        }

        let mut utf16 = 0u32;
        for (offset, ch) in source[line_start..line_end].char_indices() {
            if utf16 >= position.character {
                return line_start + offset;
            }
            let next = utf16 + ch.len_utf16() as u32;
            // A position that splits a surrogate pair rounds down to the start
            // of the character, which is what the old function did.
            if next > position.character {
                return line_start + offset;
            }
            utf16 = next;
        }
        line_end
    }

    /// Convert a byte range to an LSP range.
    ///
    /// One index serves both ends, so this costs two binary searches where
    /// [`byte_range_to_lsp`] cost two full scans of the document.
    pub fn range(&self, source: &str, start: usize, end: usize) -> Range {
        Range {
            start: self.position(source, start),
            end: self.position(source, end),
        }
    }
}

pub fn compact_preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 80 {
        collapsed
    } else {
        let preview = collapsed.chars().take(77).collect::<String>();
        format!("{preview}...")
    }
}

pub fn offset_to_position(source: &str, offset: usize) -> Position {
    let target = offset.min(source.len());
    let mut line = 0u32;
    let mut character = 0u32;

    for (byte, ch) in source.char_indices() {
        if byte >= target {
            break;
        }

        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }

    Position::new(line, character)
}

pub fn position_to_offset(source: &str, position: Position) -> usize {
    let mut line = 0u32;
    let mut character = 0u32;

    for (byte, ch) in source.char_indices() {
        if line == position.line && character >= position.character {
            return byte;
        }

        if ch == '\n' {
            if line == position.line {
                return byte;
            }
            line += 1;
            character = 0;
        } else if line == position.line {
            let next = character + ch.len_utf16() as u32;
            if next > position.character {
                return byte;
            }
            character = next;
        }
    }

    source.len()
}

pub fn byte_range_to_lsp(source: &str, start: usize, end: usize) -> Range {
    Range {
        start: offset_to_position(source, start),
        end: offset_to_position(source, end),
    }
}

/// Round `offset` down to a character boundary.
fn floor_boundary(source: &str, mut offset: usize) -> usize {
    while offset > 0 && !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Round `offset` up to a character boundary.
fn ceil_boundary(source: &str, mut offset: usize) -> usize {
    while offset < source.len() && !source.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// The character immediately before `offset`, and the byte where it starts.
///
/// `None` at offset 0. This is the primitive for a backward scan that must not
/// allocate: a caller walks left by feeding the returned start back in, which
/// costs one character per step instead of a `Vec<char>` of everything before
/// the cursor.
pub fn preceding_char(source: &str, offset: usize) -> Option<(usize, char)> {
    if offset == 0 || offset > source.len() {
        return None;
    }
    let start = floor_boundary(source, offset - 1);
    source[start..].chars().next().map(|ch| (start, ch))
}

/// The character that starts last before `offset`, and where it starts.
///
/// At offset 0 no character starts before the cursor, so the first character is
/// returned instead. That reproduces the `saturating_sub(1)` in the scanning
/// implementations these helpers replaced.
fn char_before(source: &str, offset: usize) -> Option<(usize, char)> {
    let start = if offset == 0 {
        0
    } else {
        floor_boundary(source, offset - 1)
    };
    source[start..].chars().next().map(|ch| (start, ch))
}

/// The byte range of the token the cursor sits in or directly after.
///
/// Walks outward from the cursor over the surrounding characters. The scanning
/// version this replaced built a `Vec<(usize, char)>` of the whole document —
/// about 16 bytes of allocation per character of the file — to read the handful
/// of characters around one cursor.
fn token_bounds(source: &str, lines: &LineIndex, position: Position) -> Option<(usize, usize)> {
    if source.is_empty() {
        return None;
    }
    let offset = lines.offset(source, position);
    let (cursor_start, cursor_char) = char_before(source, offset)?;
    if !is_token_char(cursor_char) {
        return None;
    }

    let mut start = cursor_start;
    while start > 0 {
        let previous = floor_boundary(source, start - 1);
        match source[previous..].chars().next() {
            Some(ch) if is_token_char(ch) => start = previous,
            _ => break,
        }
    }

    let mut end = cursor_start + cursor_char.len_utf8();
    for ch in source[end..].chars() {
        if !is_token_char(ch) {
            break;
        }
        end += ch.len_utf8();
    }

    Some((start, end))
}

pub fn is_token_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '$' | '<' | '>' | '-')
}

/// The partial token immediately before the cursor, used to filter completions.
///
/// Returns an empty string when the cursor does not follow a token character —
/// the user is starting a fresh token, and returning the previous keyword would
/// filter every candidate against it.
pub fn token_prefix(source: &str, lines: &LineIndex, position: Position) -> Option<String> {
    if source.is_empty() {
        return Some(String::new());
    }
    let offset = lines.offset(source, position);
    // The prefix ends at the cursor. A cursor inside a character rounds up to
    // that character's end, which is where the scanning version put it.
    let end = ceil_boundary(source, offset);
    if end == 0 {
        return Some(String::new());
    }

    let previous_start = floor_boundary(source, end - 1);
    let Some(previous) = source[previous_start..].chars().next() else {
        return Some(String::new());
    };
    if !is_token_char(previous) {
        return Some(String::new());
    }

    let mut start = previous_start;
    while start > 0 {
        let candidate = floor_boundary(source, start - 1);
        match source[candidate..].chars().next() {
            Some(ch) if is_token_char(ch) => start = candidate,
            _ => break,
        }
    }
    source.get(start..end).map(ToOwned::to_owned)
}

pub fn token_at(source: &str, lines: &LineIndex, position: Position) -> Option<String> {
    let (start, end) = token_bounds(source, lines, position)?;
    source.get(start..end).map(ToOwned::to_owned)
}

pub fn word_range(source: &str, lines: &LineIndex, position: Position) -> Option<Range> {
    let (start, end) = token_bounds(source, lines, position)?;
    Some(lines.range(source, start, end))
}

#[cfg(test)]
mod tests {
    use super::{LineIndex, compact_preview, offset_to_position, position_to_offset};
    use ls_types::Position;

    /// The corpus every differential test runs over. Each entry is a shape that
    /// broke, or could break, a hand-written offset walk.
    fn corpus() -> Vec<(&'static str, &'static str)> {
        vec![
            ("empty", ""),
            ("no newline at all", "SELECT * FROM person"),
            ("trailing newline", "SELECT 1;\n"),
            ("no trailing newline", "SELECT 1;\nSELECT 2;"),
            ("blank first line", "\nSELECT 1;"),
            ("consecutive newlines", "SELECT 1;\n\n\nSELECT 2;"),
            ("only newlines", "\n\n\n"),
            ("carriage returns", "SELECT 1;\r\nSELECT 2;\r\n"),
            // 3-byte UTF-8, 1 UTF-16 unit.
            ("three-byte char", "SET sym = '₹';\nSELECT sym;"),
            // 4-byte UTF-8, 2 UTF-16 units — the surrogate-pair case, which is
            // where a byte-column and a UTF-16-column implementation diverge.
            ("four-byte char", "SET emoji = '🚀';\nSELECT emoji;"),
            ("four-byte char at line start", "🚀 = 1;\nSELECT 2;"),
            ("many four-byte chars", "'🚀🚀🚀';\n'🚀';\n"),
            ("mixed widths", "a₹b🚀c;\nd;\n"),
            ("non-ascii on the last line", "SELECT 1;\n'🚀'"),
        ]
    }

    /// `LineIndex::position` must agree with `offset_to_position` at every byte
    /// offset, including one past the end.
    #[test]
    fn line_index_position_matches_the_scan_at_every_offset() {
        for (name, source) in corpus() {
            let index = LineIndex::new(source);
            for offset in 0..=source.len() + 2 {
                assert_eq!(
                    index.position(source, offset),
                    offset_to_position(source, offset),
                    "{name}: position mismatch at offset {offset} of {source:?}"
                );
            }
        }
    }

    /// `LineIndex::offset` must agree with `position_to_offset` at every
    /// position a client could send, including positions past the end of a line
    /// and past the end of the document.
    #[test]
    fn line_index_offset_matches_the_scan_at_every_position() {
        for (name, source) in corpus() {
            let index = LineIndex::new(source);
            // Two lines and four characters past the end, so the clamping
            // behaviour is compared too.
            for line in 0..index.line_count() as u32 + 2 {
                for character in 0..20u32 {
                    let position = Position::new(line, character);
                    assert_eq!(
                        index.offset(source, position),
                        position_to_offset(source, position),
                        "{name}: offset mismatch at {position:?} of {source:?}"
                    );
                }
            }
        }
    }

    /// The two conversions must compose back to the same position, which is
    /// what an editor relies on when it echoes a range back to the server.
    #[test]
    fn position_and_offset_round_trip() {
        for (name, source) in corpus() {
            let index = LineIndex::new(source);
            for offset in 0..=source.len() {
                if !source.is_char_boundary(offset) {
                    continue;
                }
                let position = index.position(source, offset);
                assert_eq!(
                    index.offset(source, position),
                    offset,
                    "{name}: round trip broke at offset {offset} of {source:?}"
                );
            }
        }
    }

    #[test]
    fn line_index_range_matches_the_scan() {
        for (name, source) in corpus() {
            let index = LineIndex::new(source);
            for start in 0..=source.len() {
                for end in start..=source.len() {
                    assert_eq!(
                        index.range(source, start, end),
                        super::byte_range_to_lsp(source, start, end),
                        "{name}: range mismatch for {start}..{end} of {source:?}"
                    );
                }
            }
        }
    }

    /// A document with no newline still has one line, so the binary search
    /// always finds a line and `line_at` never underflows.
    #[test]
    fn every_document_has_at_least_one_line() {
        assert_eq!(LineIndex::new("").line_count(), 1);
        assert_eq!(LineIndex::new("no newline").line_count(), 1);
        assert_eq!(LineIndex::new("one\n").line_count(), 2);
    }

    /// The ASCII fast path must produce the same answer as the counted path.
    /// This guards the `all_ascii` branch against a divergence that the corpus
    /// above would only catch by accident.
    #[test]
    fn the_ascii_fast_path_agrees_with_the_counted_path() {
        let ascii = "SELECT name FROM person;\nSELECT 2;\n";
        let fast = LineIndex::new(ascii);
        assert!(fast.all_ascii, "corpus entry must exercise the fast path");

        // The same text with the flag forced off, so the counting branch runs.
        let counted = LineIndex {
            line_starts: fast.line_starts.clone(),
            all_ascii: false,
        };
        for offset in 0..=ascii.len() {
            assert_eq!(
                fast.position(ascii, offset),
                counted.position(ascii, offset),
                "fast and counted paths disagree at offset {offset}"
            );
        }
        for line in 0..3u32 {
            for character in 0..30u32 {
                let position = Position::new(line, character);
                assert_eq!(
                    fast.offset(ascii, position),
                    counted.offset(ascii, position),
                    "fast and counted paths disagree at {position:?}"
                );
            }
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // The cursor helpers, against the scanning versions they replaced.
    //
    // These three references are the previous implementations, kept here
    // verbatim. They are the specification for the rewrites: the rewrites must
    // not change which token the cursor resolves to, only what it costs to find
    // out. Delete these when the behaviour is deliberately changed, not before.
    // ──────────────────────────────────────────────────────────────────

    fn token_prefix_scanning(source: &str, position: Position) -> Option<String> {
        let offset = position_to_offset(source, position);
        if source.is_empty() {
            return Some(String::new());
        }
        let chars: Vec<(usize, char)> = source.char_indices().collect();
        let cursor_index = chars.partition_point(|(byte, _)| *byte < offset);
        let Some((_, prev_char)) = cursor_index
            .checked_sub(1)
            .and_then(|index| chars.get(index))
        else {
            return Some(String::new());
        };
        if !super::is_token_char(*prev_char) {
            return Some(String::new());
        }
        let mut start = cursor_index - 1;
        while start > 0 && super::is_token_char(chars[start - 1].1) {
            start -= 1;
        }
        let start_byte = chars[start].0;
        let end_byte = chars
            .get(cursor_index)
            .map(|(byte, _)| *byte)
            .unwrap_or(source.len());
        source.get(start_byte..end_byte).map(ToOwned::to_owned)
    }

    /// The shared body of the old `token_at` and `word_range`.
    fn token_bounds_scanning(source: &str, position: Position) -> Option<(usize, usize)> {
        let offset = position_to_offset(source, position);
        let chars: Vec<(usize, char)> = source.char_indices().collect();
        if chars.is_empty() {
            return None;
        }
        let index = chars
            .partition_point(|(byte, _)| *byte < offset)
            .saturating_sub(1);
        let current = chars.get(index)?;
        if !super::is_token_char(current.1) {
            return None;
        }
        let mut start = index;
        while start > 0 && super::is_token_char(chars[start - 1].1) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < chars.len() && super::is_token_char(chars[end].1) {
            end += 1;
        }
        let start_byte = chars[start].0;
        let end_byte = chars
            .get(end)
            .map(|(byte, _)| *byte)
            .unwrap_or(source.len());
        Some((start_byte, end_byte))
    }

    /// Cursor positions worth probing for a given source: every line, and every
    /// character column up to a little past the longest line.
    fn probe_positions(source: &str) -> Vec<Position> {
        let lines = source.split('\n').count().max(1);
        let widest = source
            .split('\n')
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0);
        let mut out = Vec::new();
        for line in 0..lines as u32 + 1 {
            for character in 0..widest as u32 + 3 {
                out.push(Position::new(line, character));
            }
        }
        out
    }

    /// A corpus aimed at the cursor helpers: token runs, punctuation edges, and
    /// multi-byte characters inside and beside a token.
    fn cursor_corpus() -> Vec<&'static str> {
        vec![
            "",
            "SELECT",
            "SELECT * FROM person",
            "SELECT name FROM person WHERE age > 21;",
            "fn::slugify($text)",
            "$param",
            "table:id",
            "a-b_c<d>e",
            "   leading space",
            "trailing space   ",
            ";;;",
            "SELECT 1;\nSELECT 2;",
            "SET sym = '₹';\nSELECT sym;",
            "SET emoji = '🚀';",
            "naïve_name",
            "🚀token",
            "token🚀",
            "\n\n\n",
        ]
    }

    #[test]
    fn token_at_matches_the_scanning_version() {
        for source in cursor_corpus() {
            let index = LineIndex::new(source);
            for position in probe_positions(source) {
                let expected = token_bounds_scanning(source, position)
                    .and_then(|(s, e)| source.get(s..e).map(ToOwned::to_owned));
                assert_eq!(
                    super::token_at(source, &index, position),
                    expected,
                    "token_at differs at {position:?} of {source:?}"
                );
            }
        }
    }

    #[test]
    fn word_range_matches_the_scanning_version() {
        for source in cursor_corpus() {
            let index = LineIndex::new(source);
            for position in probe_positions(source) {
                let expected = token_bounds_scanning(source, position)
                    .map(|(s, e)| super::byte_range_to_lsp(source, s, e));
                assert_eq!(
                    super::word_range(source, &index, position),
                    expected,
                    "word_range differs at {position:?} of {source:?}"
                );
            }
        }
    }

    #[test]
    fn token_prefix_matches_the_scanning_version() {
        for source in cursor_corpus() {
            let index = LineIndex::new(source);
            for position in probe_positions(source) {
                assert_eq!(
                    super::token_prefix(source, &index, position),
                    token_prefix_scanning(source, position),
                    "token_prefix differs at {position:?} of {source:?}"
                );
            }
        }
    }

    #[test]
    fn compact_preview_preserves_unicode_boundaries() {
        let text = "UPSERT currency:inr SET name = 'Indian Rupee', iso_code = 'INR', symbol = '₹', subunits = 2";

        let preview = compact_preview(text);

        assert!(preview.ends_with("..."));
        assert!(preview.contains('₹'));
        assert!(preview.is_char_boundary(preview.len()));
    }
}
