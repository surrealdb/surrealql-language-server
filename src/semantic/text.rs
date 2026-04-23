use tower_lsp_server::ls_types::{Position, Range};

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

pub fn is_token_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '$' | '<' | '>' | '-')
}

pub fn token_prefix(source: &str, position: Position) -> Option<String> {
    let offset = position_to_offset(source, position);
    if source.is_empty() {
        return Some(String::new());
    }

    let chars: Vec<(usize, char)> = source.char_indices().collect();
    let cursor_index = chars.partition_point(|(byte, _)| *byte < offset);

    // If the character immediately before the cursor is not a token char
    // (i.e., the cursor is in whitespace, after a punctuation mark, or at
    // the very start of the document), the user is starting a fresh token —
    // the prefix is empty. Returning the trailing keyword token here would
    // make `completion_items` filter every table/function/keyword whose name
    // doesn't start with that previous keyword.
    let Some((_, prev_char)) = cursor_index
        .checked_sub(1)
        .and_then(|index| chars.get(index))
    else {
        return Some(String::new());
    };
    if !is_token_char(*prev_char) {
        return Some(String::new());
    }

    let mut start = cursor_index - 1;
    while start > 0 && is_token_char(chars[start - 1].1) {
        start -= 1;
    }

    let start_byte = chars[start].0;
    let end_byte = chars
        .get(cursor_index)
        .map(|(byte, _)| *byte)
        .unwrap_or(source.len());
    source.get(start_byte..end_byte).map(ToOwned::to_owned)
}

pub fn token_at(source: &str, position: Position) -> Option<String> {
    let offset = position_to_offset(source, position);
    let chars: Vec<(usize, char)> = source.char_indices().collect();
    if chars.is_empty() {
        return None;
    }

    let index = chars
        .partition_point(|(byte, _)| *byte < offset)
        .saturating_sub(1);
    let current = chars.get(index)?;
    if !is_token_char(current.1) {
        return None;
    }

    let mut start = index;
    while start > 0 && is_token_char(chars[start - 1].1) {
        start -= 1;
    }

    let mut end = index + 1;
    while end < chars.len() && is_token_char(chars[end].1) {
        end += 1;
    }

    let start_byte = chars[start].0;
    let end_byte = chars
        .get(end)
        .map(|(byte, _)| *byte)
        .unwrap_or(source.len());
    source.get(start_byte..end_byte).map(ToOwned::to_owned)
}

pub fn word_range(source: &str, position: Position) -> Option<Range> {
    let offset = position_to_offset(source, position);
    let chars: Vec<(usize, char)> = source.char_indices().collect();
    if chars.is_empty() {
        return None;
    }

    let index = chars
        .partition_point(|(byte, _)| *byte < offset)
        .saturating_sub(1);
    let current = chars.get(index)?;
    if !is_token_char(current.1) {
        return None;
    }

    let mut start = index;
    while start > 0 && is_token_char(chars[start - 1].1) {
        start -= 1;
    }

    let mut end = index + 1;
    while end < chars.len() && is_token_char(chars[end].1) {
        end += 1;
    }

    let start_byte = chars[start].0;
    let end_byte = chars
        .get(end)
        .map(|(byte, _)| *byte)
        .unwrap_or(source.len());
    Some(byte_range_to_lsp(source, start_byte, end_byte))
}

#[cfg(test)]
mod tests {
    use super::compact_preview;

    #[test]
    fn compact_preview_preserves_unicode_boundaries() {
        let text = "UPSERT currency:inr SET name = 'Indian Rupee', iso_code = 'INR', symbol = '₹', subunits = 2";

        let preview = compact_preview(text);

        assert!(preview.ends_with("..."));
        assert!(preview.contains('₹'));
        assert!(preview.is_char_boundary(preview.len()));
    }
}
