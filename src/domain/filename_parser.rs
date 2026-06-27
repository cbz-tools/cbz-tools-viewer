#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFilename {
    pub original: String,
    pub parts: Vec<FilenamePart>,
    pub warnings: Vec<ParseWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilenamePart {
    pub role: FilenamePartRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilenamePartRole {
    Kind,
    Author,
    AuthorAlias,
    Title,
    Work,
    Edition,
    Extra,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseWarning {
    EmptyInput,
    UnmatchedBracket,
}

pub fn parse_filename(filename: &str) -> ParsedFilename {
    let base = strip_extension(filename);
    let mut working = base.trim().to_string();
    let mut warnings = Vec::new();

    if working.is_empty() {
        warnings.push(ParseWarning::EmptyInput);
    }

    let mut parts = Vec::new();
    let mut saw_author = false;

    loop {
        let trimmed = working.trim_start();
        let leading_ws = working.len().saturating_sub(trimmed.len());
        if let Some(token) = consume_leading_bracket_token(trimmed) {
            if leading_ws + token.consumed_len > working.len() {
                break;
            }
            let token_kind = token.kind;
            let token_text = token.text.trim().to_string();
            if !token_text.is_empty() {
                if token_kind == BracketKind::Square && !saw_author {
                    append_author_parts(&mut parts, &token_text, &mut saw_author);
                } else {
                    parts.push(FilenamePart {
                        role: match token_kind {
                            BracketKind::Round => FilenamePartRole::Kind,
                            BracketKind::Square => FilenamePartRole::Extra,
                        },
                        text: token_text,
                    });
                }
            }
            working = working[(leading_ws + token.consumed_len)..].to_string();
        } else {
            break;
        }
    }

    let mut suffix_parts = Vec::new();
    loop {
        let trimmed = working.trim_end();
        if trimmed.is_empty() {
            working.clear();
            break;
        }
        let trailing_ws = working.len().saturating_sub(trimmed.len());
        if let Some(token) = consume_trailing_bracket_token(trimmed) {
            if token.consumed_len + trailing_ws > working.len() {
                break;
            }
            let role = match token.kind {
                BracketKind::Round => FilenamePartRole::Work,
                BracketKind::Square => FilenamePartRole::Edition,
            };
            let text = token.text.trim().to_string();
            if !text.is_empty() {
                suffix_parts.push(FilenamePart { role, text });
            }
            let keep_len = trimmed.len().saturating_sub(token.consumed_len);
            working = working[..keep_len].to_string();
        } else {
            break;
        }
    }
    suffix_parts.reverse();

    let title = working.trim().to_string();
    if !title.is_empty() {
        parts.push(FilenamePart {
            role: FilenamePartRole::Title,
            text: title,
        });
    }
    parts.extend(suffix_parts);

    if parts.is_empty() {
        let fallback = base.trim().to_string();
        parts.push(FilenamePart {
            role: FilenamePartRole::Title,
            text: fallback,
        });
    }

    if has_unmatched_bracket(base.trim()) {
        warnings.push(ParseWarning::UnmatchedBracket);
    }

    ParsedFilename {
        original: filename.to_string(),
        parts,
        warnings,
    }
}

fn append_author_parts(parts: &mut Vec<FilenamePart>, token_text: &str, saw_author: &mut bool) {
    if let Some((author, alias)) = split_author_alias(token_text) {
        if !author.is_empty() {
            parts.push(FilenamePart {
                role: FilenamePartRole::Author,
                text: author,
            });
            *saw_author = true;
        }
        if !alias.is_empty() {
            parts.push(FilenamePart {
                role: FilenamePartRole::AuthorAlias,
                text: alias,
            });
        }
    } else {
        parts.push(FilenamePart {
            role: FilenamePartRole::Author,
            text: token_text.to_string(),
        });
        *saw_author = true;
    }
}

fn split_author_alias(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    if !trimmed.ends_with(')') {
        return None;
    }
    let mut depth = 0usize;
    let mut open_idx = None;
    for (idx, ch) in trimmed.char_indices().rev() {
        if ch == ')' {
            depth += 1;
            continue;
        }
        if ch == '(' {
            if depth == 0 {
                return None;
            }
            depth -= 1;
            if depth == 0 {
                open_idx = Some(idx);
                break;
            }
        }
    }
    let start = open_idx?;
    let alias_start = start + '('.len_utf8();
    if alias_start > trimmed.len() || trimmed.is_empty() {
        return None;
    }
    let alias_end = trimmed.len().saturating_sub(')'.len_utf8());
    if alias_end < alias_start {
        return None;
    }
    let alias = trimmed[alias_start..alias_end].trim().to_string();
    let author = trimmed[..start].trim().to_string();
    if author.is_empty() || alias.is_empty() {
        return None;
    }
    Some((author, alias))
}

fn strip_extension(filename: &str) -> &str {
    let mut dot_pos = None;
    for (idx, ch) in filename.char_indices().rev() {
        if ch == '.' {
            dot_pos = Some(idx);
            break;
        }
        if ch == '/' || ch == '\\' {
            break;
        }
    }
    match dot_pos {
        Some(0) | None => filename,
        Some(idx) => &filename[..idx],
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BracketKind {
    Round,
    Square,
}

struct BracketToken {
    kind: BracketKind,
    text: String,
    consumed_len: usize,
}

fn consume_leading_bracket_token(input: &str) -> Option<BracketToken> {
    let mut chars = input.char_indices();
    let (start, first) = chars.next()?;
    if start != 0 {
        return None;
    }
    let (open, close, kind) = match first {
        '(' => ('(', ')', BracketKind::Round),
        '[' => ('[', ']', BracketKind::Square),
        _ => return None,
    };
    let mut depth = 0usize;
    for (idx, ch) in input.char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close {
            if depth == 0 {
                return None;
            }
            depth -= 1;
            if depth == 0 {
                let inner_start = first.len_utf8();
                if idx < inner_start {
                    return None;
                }
                let inner = input[inner_start..idx].to_string();
                let consumed_len = idx + close.len_utf8();
                return Some(BracketToken {
                    kind,
                    text: inner,
                    consumed_len,
                });
            }
        }
    }
    None
}

fn consume_trailing_bracket_token(input: &str) -> Option<BracketToken> {
    let mut chars = input.char_indices().rev();
    let (close_idx, close_ch) = chars.next()?;
    let (open, close, kind) = match close_ch {
        ')' => ('(', ')', BracketKind::Round),
        ']' => ('[', ']', BracketKind::Square),
        _ => return None,
    };
    let close_len = close.len_utf8();
    if close_idx + close_len != input.len() {
        return None;
    }

    let mut depth = 0usize;
    for (idx, ch) in input.char_indices().rev() {
        if ch == close {
            depth += 1;
        } else if ch == open {
            if depth == 0 {
                return None;
            }
            depth -= 1;
            if depth == 0 {
                if idx > 0 {
                    let prev = input[..idx].chars().next_back();
                    if prev.is_some_and(|c| !c.is_whitespace()) {
                        return None;
                    }
                }
                let inner_start = idx + open.len_utf8();
                let inner_end = close_idx;
                if inner_start > inner_end || inner_end > input.len() {
                    return None;
                }
                let inner = input[inner_start..inner_end].to_string();
                let consumed_len = input.len().saturating_sub(idx);
                return Some(BracketToken {
                    kind,
                    text: inner,
                    consumed_len,
                });
            }
        }
    }
    None
}

fn has_unmatched_bracket(input: &str) -> bool {
    let mut round = 0usize;
    let mut square = 0usize;
    for ch in input.chars() {
        match ch {
            '(' => round += 1,
            ')' => {
                if round == 0 {
                    return true;
                }
                round -= 1;
            }
            '[' => square += 1,
            ']' => {
                if square == 0 {
                    return true;
                }
                square -= 1;
            }
            _ => {}
        }
    }
    round != 0 || square != 0
}
