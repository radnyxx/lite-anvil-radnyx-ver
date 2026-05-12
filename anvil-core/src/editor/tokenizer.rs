use pcre2::bytes::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::sync::Arc;

use crate::editor::error::RegexError;
use crate::editor::syntax::{PatternSpec, SubSyntaxSpec, SyntaxDefinition, TokenType};

/// A single token produced by the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token_type: String,
    pub text: String,
}

/// Count UTF-8 characters in a string.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// 1-based character substring (inclusive on both ends).
pub fn usub(text: &str, start: usize, end: usize) -> &str {
    if start == 0 || start > end {
        return "";
    }
    let mut start_byte = None;
    let mut end_byte = None;
    for (idx, (byte_idx, _)) in (1usize..).zip(text.char_indices()) {
        if idx == start {
            start_byte = Some(byte_idx);
        }
        if idx == end + 1 {
            end_byte = Some(byte_idx);
            break;
        }
    }
    let Some(start_byte) = start_byte else {
        return "";
    };
    let end_byte = end_byte.unwrap_or(text.len());
    &text[start_byte..end_byte]
}

/// Count UTF-8 characters in the first `byte_count` bytes of `text`.
pub fn prefix_ulen(text: &str, byte_count: usize) -> usize {
    let clamped = byte_count.min(text.len());
    let mut end = clamped;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].chars().count()
}

/// 1-based byte position of the `char_idx`-th character.
pub fn ucharpos(text: &str, char_idx: usize) -> Option<usize> {
    if char_idx == 0 {
        return Some(1);
    }
    let mut count = 0usize;
    for (byte_idx, _) in text.char_indices() {
        count += 1;
        if count == char_idx {
            return Some(byte_idx + 1);
        }
    }
    None
}

/// Strip a leading `^` anchor and return the code and whether it was anchored.
pub fn split_anchor(code: String) -> (String, bool) {
    if let Some(stripped) = code.strip_prefix('^') {
        (stripped.to_string(), true)
    } else {
        (code, false)
    }
}

/// Get the first byte of a string.
pub fn first_byte(value: &str) -> Option<u8> {
    value.as_bytes().first().copied()
}

/// Append a token to a list, merging with the previous token if types match
/// or the previous token is pure whitespace.
pub fn push_token(tokens: &mut Vec<Token>, token_type: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(prev) = tokens.last_mut() {
        if prev.token_type == token_type
            || (prev.text.chars().all(char::is_whitespace) && token_type != "incomplete")
        {
            prev.token_type.clear();
            prev.token_type.push_str(token_type);
            prev.text.push_str(text);
            return;
        }
    }
    tokens.push(Token {
        token_type: token_type.to_string(),
        text: text.to_string(),
    });
}

/// Append tokens from a pattern match, splitting on captures if present.
pub fn push_tokens(
    tokens: &mut Vec<Token>,
    symbols: &HashMap<String, String>,
    token_types: &[String],
    full_text: &str,
    mut find_results: Vec<usize>,
) {
    if find_results.len() > 2 {
        find_results.insert(2, find_results[0]);
        let end_copy = find_results[1] + 1;
        find_results.push(end_copy);
        for i in 2..find_results.len() - 1 {
            let start = find_results[i];
            let fin = find_results[i + 1].saturating_sub(1);
            if fin >= start {
                let text = usub(full_text, start, fin);
                let token_type = token_types
                    .get(i - 2)
                    .map(String::as_str)
                    .unwrap_or_else(|| token_types.first().map(String::as_str).unwrap_or("normal"));
                let mapped = symbols.get(text).map(String::as_str).unwrap_or(token_type);
                push_token(tokens, mapped, text);
            }
        }
    } else if find_results.len() >= 2 {
        let start = find_results[0];
        let fin = find_results[1];
        let text = usub(full_text, start, fin);
        let token_type = token_types.first().map(String::as_str).unwrap_or("normal");
        let mapped = symbols.get(text).map(String::as_str).unwrap_or(token_type);
        push_token(tokens, mapped, text);
    }
}

// ── Compiled syntax types ────────────────────────────────────────────────────

/// How a single pattern matcher operates.
#[derive(Clone)]
pub enum MatcherKind {
    LuaPattern { code: String },
    Regex { compiled: Arc<Regex> },
}

/// A single pattern matcher with its anchor state.
#[derive(Clone)]
pub struct MatcherDef {
    pub kind: MatcherKind,
    pub whole_line: bool,
}

/// A pattern that matches either a single span or an open/close pair.
#[derive(Clone)]
pub enum PatternMatcher {
    Single(MatcherDef),
    Pair {
        open: MatcherDef,
        close: MatcherDef,
        escape_byte: Option<u8>,
    },
}

/// Reference to another syntax for sub-syntax patterns.
#[derive(Clone)]
pub enum SyntaxRef {
    /// Fully compiled nested syntax, shared via `Arc` so the same graph
    /// node referenced from multiple patterns deduplicates.
    Inline(Arc<CompiledSyntax>),
    /// External lookup by name. Reserved for cross-asset references;
    /// currently unresolved at tokenize time.
    Selector(String),
}

/// A compiled pattern definition ready for tokenization.
#[derive(Clone)]
pub struct PatternDef {
    pub matcher: PatternMatcher,
    pub token_types: Vec<String>,
    pub syntax_ref: Option<SyntaxRef>,
    pub disabled: bool,
}

/// A fully compiled syntax: patterns + symbol map.
#[derive(Clone, Default)]
pub struct CompiledSyntax {
    pub patterns: Vec<PatternDef>,
    pub symbols: HashMap<String, String>,
}

/// Compile a PCRE2 regex pattern for the tokenizer.
pub fn compile_regex(pattern: &str) -> Result<Regex, RegexError> {
    let mut builder = RegexBuilder::new();
    builder.utf(true).ucp(true);
    builder
        .build(pattern)
        .map_err(|e| RegexError::Compile(e.to_string()))
}

/// Convert a Lua pattern string to a PCRE2 regex.
/// Expand a `%x` Lua class inside a character class `[...]`.
fn lua_class_to_regex_in_bracket(ch: char) -> &'static str {
    match ch {
        'a' => "\\p{L}",
        'A' => "\\P{L}",
        'd' => "0-9",
        'D' => "^0-9",
        'w' => "\\w\\p{M}",
        'W' => "^\\w\\p{M}",
        's' => "\\s",
        'S' => "\\S",
        'l' => "\\p{Ll}",
        'L' => "\\P{Ll}",
        'u' => "\\p{Lu}",
        'U' => "\\P{Lu}",
        'p' => "!-/:-@\\[-`{-~",
        _ => "",
    }
}

/// Expand a `%x` Lua class outside brackets.
fn lua_class_to_regex(ch: char) -> &'static str {
    match ch {
        'a' => "\\p{L}",
        'A' => "\\P{L}",
        'd' => "\\d",
        'D' => "\\D",
        'w' => "[\\w\\p{M}]",
        'W' => "[^\\w\\p{M}]",
        's' => "\\s",
        'S' => "\\S",
        'l' => "\\p{Ll}",
        'L' => "\\P{Ll}",
        'u' => "\\p{Lu}",
        'U' => "\\P{Lu}",
        'p' => "[^\\w\\s]",
        'P' => "[\\w\\s]",
        'c' => "[\\x00-\\x1f]",
        _ => "",
    }
}

fn lua_pattern_to_regex(pat: &str) -> String {
    let mut out = String::new();
    let mut chars = pat.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(&next) = chars.peek() {
                chars.next();
                if next == 'f' {
                    // %f[set] -- Lua frontier pattern. Matches the empty
                    // string at a position where the previous character
                    // does NOT match `set` and the current character DOES.
                    // Map to PCRE2's `(?<![set])(?=[set])`. Emitting only
                    // the lookahead (the historical form) caused patterns
                    // like `%f[^>][^<]` to fire mid-content and swallow
                    // tag names as plain text in XML/HTML grammars.
                    if chars.peek() == Some(&'[') {
                        chars.next(); // consume '['
                        // Collect the bracket contents once so we can emit
                        // them in both the lookbehind and the lookahead.
                        let mut set = String::new();
                        while let Some(&c) = chars.peek() {
                            if c == ']' {
                                chars.next();
                                break;
                            }
                            if c == '%' {
                                chars.next();
                                if let Some(&nc) = chars.peek() {
                                    chars.next();
                                    let expanded = lua_class_to_regex_in_bracket(nc);
                                    if expanded.is_empty() {
                                        if "\\]^-".contains(nc) {
                                            set.push('\\');
                                        }
                                        set.push(nc);
                                    } else {
                                        set.push_str(expanded);
                                    }
                                }
                            } else {
                                chars.next();
                                set.push(c);
                            }
                        }
                        out.push_str("(?<![");
                        out.push_str(&set);
                        out.push_str("])(?=[");
                        out.push_str(&set);
                        out.push_str("])");
                    }
                } else if next == 'b' {
                    // %bxy balanced match -- approximate.
                    if let (Some(open), Some(_close)) = (chars.next(), chars.next()) {
                        if "\\.*+?^${}()|[]".contains(open) {
                            out.push('\\');
                        }
                        out.push(open);
                    }
                } else {
                    let expanded = lua_class_to_regex(next);
                    if expanded.is_empty() {
                        // Literal escape.
                        if "\\.*+?^${}()|[]".contains(next) {
                            out.push('\\');
                        }
                        out.push(next);
                    } else {
                        out.push_str(expanded);
                    }
                }
            }
        } else if ch == '[' {
            // Character class -- need to handle %x inside brackets.
            out.push('[');
            while let Some(&c) = chars.peek() {
                if c == ']' {
                    chars.next();
                    out.push(']');
                    break;
                }
                if c == '%' {
                    chars.next();
                    if let Some(&nc) = chars.peek() {
                        chars.next();
                        let expanded = lua_class_to_regex_in_bracket(nc);
                        if expanded.is_empty() {
                            if "\\]^-".contains(nc) {
                                out.push('\\');
                            }
                            out.push(nc);
                        } else {
                            out.push_str(expanded);
                        }
                    }
                } else {
                    chars.next();
                    out.push(c);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build a `MatcherDef` from a pattern or regex string.
pub fn make_matcher(kind_name: &str, code: String) -> Result<MatcherDef, RegexError> {
    let (code, whole_line) = split_anchor(code);
    // Convert Lua patterns to PCRE2 regex.
    let regex_code = if kind_name == "regex" {
        code
    } else {
        lua_pattern_to_regex(&code)
    };
    let kind = match compile_regex(&regex_code) {
        Ok(compiled) => MatcherKind::Regex {
            compiled: Arc::new(compiled),
        },
        Err(_) => {
            // Fall back to storing as LuaPattern if regex compilation fails.
            MatcherKind::LuaPattern { code: regex_code }
        }
    };
    Ok(MatcherDef { kind, whole_line })
}

/// Run a regex matcher at a character offset and return 1-based character positions.
pub fn regex_find(
    matcher: &MatcherDef,
    text: &str,
    next: usize,
    anchored: bool,
) -> Result<Vec<usize>, RegexError> {
    let MatcherKind::Regex { compiled, .. } = &matcher.kind else {
        return Ok(Vec::new());
    };

    let start_byte = ucharpos(text, next)
        .unwrap_or(text.len() + 1)
        .saturating_sub(1);
    let mut locs = compiled.capture_locations();
    match compiled.captures_read_at(&mut locs, text.as_bytes(), start_byte) {
        Ok(Some(_)) => {
            let Some((s, e)) = locs.get(0) else {
                return Ok(Vec::new());
            };
            if anchored && s != start_byte {
                return Ok(Vec::new());
            }

            let mut res = vec![s + 1, e];
            for i in 1..=compiled.captures_len() {
                if let Some((cs, ce)) = locs.get(i) {
                    if cs == ce {
                        res.push(cs + 1);
                    }
                }
            }

            let char_pos_1 = if res[0] > next {
                prefix_ulen(text, res[0])
            } else {
                next
            };
            let char_pos_2 = prefix_ulen(text, res[1]);
            res[0] = char_pos_1;
            res[1] = char_pos_2;
            for item in res.iter_mut().skip(2) {
                *item = prefix_ulen(text, item.saturating_sub(1)) + 1;
            }
            Ok(res)
        }
        Ok(None) => Ok(Vec::new()),
        Err(err) => Err(RegexError::Match(err.to_string())),
    }
}

/// Tokenize a single line using a compiled syntax, returning a flat list of tokens.
pub fn tokenize_line(syntax: &CompiledSyntax, line: &str) -> Vec<Token> {
    tokenize_line_with_state(syntax, line, &[]).0
}

/// Run a pattern's open or close matcher (with optional escape handling).
/// `at_start` anchors the match to the position; `close=true` selects the
/// close side of a Pair (or the only matcher for Singles).
pub fn find_text(
    line: &str,
    pattern: &PatternDef,
    offset: usize,
    at_start: bool,
    close: bool,
) -> Result<Vec<usize>, RegexError> {
    if pattern.disabled {
        return Ok(Vec::new());
    }
    let (matcher, escape_byte) = match &pattern.matcher {
        PatternMatcher::Single(m) => (m, None),
        PatternMatcher::Pair {
            open,
            close: closer,
            escape_byte,
        } => {
            if close {
                (closer, *escape_byte)
            } else {
                (open, *escape_byte)
            }
        }
    };
    if matcher.whole_line && offset > 1 {
        return Ok(Vec::new());
    }
    let anchored = at_start || matcher.whole_line;
    let res = regex_find(matcher, line, offset, anchored)?;
    if res.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(escape_byte) = escape_byte {
        // Count preceding escape bytes; odd → the delimiter is escaped,
        // skip it and look further. Mirrors legacy `find_text` semantics.
        let mut count = 0usize;
        let mut i = res[0].saturating_sub(1);
        while i >= 1 {
            let byte = line.as_bytes().get(i - 1).copied();
            if byte != Some(escape_byte) {
                break;
            }
            count += 1;
            if i == 1 {
                break;
            }
            i -= 1;
        }
        if count % 2 == 0 {
            return Ok(res);
        }
        if at_start || !close {
            return Ok(Vec::new());
        }
        let new_offset = res[0].saturating_add(1);
        if new_offset <= offset {
            return Ok(Vec::new());
        }
        return find_text(line, pattern, new_offset, at_start, close);
    }

    Ok(res)
}

/// State derived from the byte-stack `state`: which compiled syntax is
/// currently active, which pair we're waiting to close, and how deep we
/// are in the nesting stack.
struct SyntaxStateView<'a> {
    current_syntax: &'a CompiledSyntax,
    /// `(parent_syntax, parent_pattern_idx)` when the active syntax was
    /// entered via a sub-syntax pair on `parent_syntax`. The close of
    /// `parent_syntax.patterns[parent_pattern_idx]` pops back out.
    subsyntax_info: Option<(&'a CompiledSyntax, usize)>,
    /// 1-based pattern index in `current_syntax` that opened the
    /// current pair (when no sub-syntax descent happened). `0` means
    /// no open pair at this level.
    current_pattern_idx: usize,
    /// 1-based depth in the state byte stack.
    current_level: usize,
}

fn retrieve_syntax_state<'a>(base: &'a CompiledSyntax, state: &[u8]) -> SyntaxStateView<'a> {
    let mut current_syntax: &'a CompiledSyntax = base;
    let mut subsyntax_info: Option<(&'a CompiledSyntax, usize)> = None;
    let mut current_pattern_idx = state.first().copied().unwrap_or(0) as usize;
    let mut current_level = 1usize;

    if current_pattern_idx > 0
        && current_syntax
            .patterns
            .get(current_pattern_idx - 1)
            .is_some()
    {
        for (i, target) in state.iter().enumerate() {
            let target = *target as usize;
            if target == 0 {
                break;
            }
            let Some(pattern) = current_syntax.patterns.get(target - 1) else {
                break;
            };
            match &pattern.syntax_ref {
                Some(SyntaxRef::Inline(sub)) => {
                    subsyntax_info = Some((current_syntax, target - 1));
                    current_syntax = sub.as_ref();
                    current_pattern_idx = 0;
                    current_level = i + 2;
                }
                _ => {
                    current_pattern_idx = target;
                    break;
                }
            }
        }
    }

    SyntaxStateView {
        current_syntax,
        subsyntax_info,
        current_pattern_idx,
        current_level,
    }
}

/// Tokenize one line carrying multi-line state across calls. `in_state`
/// is a byte stack: each level holds a 1-based pattern index, descending
/// into sub-syntaxes as Pair patterns with `syntax_ref` are opened.
/// Empty (or a single `0`) means "no carry-over". Callers thread the
/// returned state into the next line so block comments, multi-line
/// strings, and other paired constructs span line boundaries.
pub fn tokenize_line_with_state(
    syntax: &CompiledSyntax,
    line: &str,
    in_state: &[u8],
) -> (Vec<Token>, Vec<u8>) {
    let mut tokens = Vec::new();
    let line_len = char_len(line);

    let mut state: Vec<u8> = in_state.to_vec();
    if state.is_empty() {
        state.push(0);
    }

    if line_len == 0 {
        return (tokens, finalize_state(state));
    }

    let mut syn_state = retrieve_syntax_state(syntax, &state);
    let mut i = 1usize;

    while i <= line_len {
        if syn_state.current_pattern_idx > 0 {
            let pattern_idx = syn_state.current_pattern_idx - 1;
            let current_syntax = syn_state.current_syntax;
            if let Some(pattern) = current_syntax.patterns.get(pattern_idx) {
                let find_results = find_text(line, pattern, i, false, true).unwrap_or_default();
                let s = find_results.first().copied();
                let e = find_results.get(1).copied();
                let token_type = pattern
                    .token_types
                    .first()
                    .map(String::as_str)
                    .unwrap_or("normal");

                let mut cont = true;
                if let Some((parent, sub_idx)) = syn_state.subsyntax_info {
                    if let Some(sub_pattern) = parent.patterns.get(sub_idx) {
                        let sub_find =
                            find_text(line, sub_pattern, i, false, true).unwrap_or_default();
                        if let Some(ss) = sub_find.first().copied() {
                            if s.is_none() || ss < s.unwrap_or(usize::MAX) {
                                if ss > i {
                                    let text_part = usub(line, i, ss - 1);
                                    push_token(&mut tokens, token_type, text_part);
                                }
                                i = ss;
                                cont = false;
                            }
                        }
                    }
                }

                if cont {
                    if let (Some(s), Some(e)) = (s, e) {
                        if s > i {
                            let text_part = usub(line, i, s - 1);
                            push_token(&mut tokens, token_type, text_part);
                        }
                        push_tokens(
                            &mut tokens,
                            &current_syntax.symbols,
                            &pattern.token_types,
                            line,
                            find_results,
                        );
                        let level_idx = syn_state.current_level.saturating_sub(1);
                        if level_idx < state.len() {
                            state[level_idx] = 0;
                        }
                        syn_state.current_pattern_idx = 0;
                        i = e + 1;
                    } else {
                        let text_part = usub(line, i, line_len);
                        push_token(&mut tokens, token_type, text_part);
                        return (tokens, finalize_state(state));
                    }
                }
            }
        }

        // Pop sub-syntaxes whose close matches at the current position.
        // Each pop emits the close token via the parent's pair pattern.
        while let Some((parent, sub_idx)) = syn_state.subsyntax_info {
            let Some(sub_pattern) = parent.patterns.get(sub_idx) else {
                break;
            };
            let find_results = find_text(line, sub_pattern, i, true, true).unwrap_or_default();
            let s = find_results.first().copied();
            let e = find_results.get(1).copied();
            if let (Some(_), Some(e)) = (s, e) {
                push_tokens(
                    &mut tokens,
                    &syn_state.current_syntax.symbols,
                    &sub_pattern.token_types,
                    line,
                    find_results,
                );
                syn_state.current_level = syn_state.current_level.saturating_sub(1);
                state.truncate(syn_state.current_level);
                if state.is_empty() {
                    state.push(0);
                }
                let level_idx = syn_state.current_level.saturating_sub(1);
                if level_idx < state.len() {
                    state[level_idx] = 0;
                }
                syn_state = retrieve_syntax_state(syntax, &state);
                i = e + 1;
                if i > line_len {
                    return (tokens, finalize_state(state));
                }
            } else {
                break;
            }
        }

        if i > line_len {
            break;
        }

        let current_syntax = syn_state.current_syntax;
        let mut matched = false;
        for (n, pattern) in current_syntax.patterns.iter().enumerate() {
            let find_results = find_text(line, pattern, i, true, false).unwrap_or_default();
            if find_results.len() < 2 {
                continue;
            }
            if find_results[0] > find_results[1] {
                continue;
            }

            push_tokens(
                &mut tokens,
                &current_syntax.symbols,
                &pattern.token_types,
                line,
                find_results.clone(),
            );

            if matches!(pattern.matcher, PatternMatcher::Pair { .. }) {
                let level_idx = syn_state.current_level.saturating_sub(1);
                if level_idx >= state.len() {
                    state.push((n + 1) as u8);
                } else {
                    state[level_idx] = (n + 1) as u8;
                }
                if let Some(SyntaxRef::Inline(sub)) = &pattern.syntax_ref {
                    syn_state.current_level += 1;
                    syn_state.subsyntax_info = Some((current_syntax, n));
                    syn_state.current_syntax = sub.as_ref();
                    syn_state.current_pattern_idx = 0;
                } else {
                    syn_state.current_pattern_idx = n + 1;
                }
            }
            i = find_results[1] + 1;
            matched = true;
            break;
        }

        if !matched {
            let text_part = usub(line, i, i);
            push_token(&mut tokens, "normal", text_part);
            i += 1;
        }
    }

    (tokens, finalize_state(state))
}

/// Strip trailing zero levels (no carry-over) and return an empty vec
/// when the entire stack is zero. Callers treat an empty state as
/// "fresh".
fn finalize_state(mut state: Vec<u8>) -> Vec<u8> {
    while state.last() == Some(&0) {
        state.pop();
    }
    state
}

/// Compile a `SyntaxDefinition` (from `native::syntax`) into a `CompiledSyntax`.
pub fn compile_from_definition(def: &SyntaxDefinition) -> Result<CompiledSyntax, RegexError> {
    let mut patterns = Vec::new();

    for rule in &def.patterns {
        let kind_name;
        let spec;
        if let Some(p) = &rule.pattern {
            kind_name = "pattern";
            spec = p;
        } else if let Some(r) = &rule.regex {
            kind_name = "regex";
            spec = r;
        } else {
            continue;
        };

        let matcher = match spec {
            PatternSpec::Single(code) => {
                PatternMatcher::Single(make_matcher(kind_name, code.clone())?)
            }
            PatternSpec::Pair {
                open,
                close,
                escape,
            } => {
                let open_def = make_matcher(kind_name, open.clone())?;
                let close_def = make_matcher(kind_name, close.clone())?;
                let escape_byte = escape.as_deref().and_then(first_byte);
                PatternMatcher::Pair {
                    open: open_def,
                    close: close_def,
                    escape_byte,
                }
            }
        };

        let token_types = match &rule.token_type {
            TokenType::Single(s) => vec![s.clone()],
            TokenType::Multi(v) => v.clone(),
        };

        let syntax_ref = match &rule.syntax {
            Some(SubSyntaxSpec::Selector(name)) => Some(SyntaxRef::Selector(name.clone())),
            Some(SubSyntaxSpec::Inline(sub_def)) => {
                let sub_compiled = compile_from_definition(sub_def)?;
                Some(SyntaxRef::Inline(Arc::new(sub_compiled)))
            }
            None => None,
        };

        patterns.push(PatternDef {
            matcher,
            token_types,
            syntax_ref,
            disabled: false,
        });
    }

    Ok(CompiledSyntax {
        patterns,
        symbols: def.symbols.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn data_dir() -> String {
        for candidate in ["data", "../data"] {
            if Path::new(candidate).join("assets/syntax").is_dir() {
                return candidate.to_string();
            }
        }
        panic!("cannot locate data/ directory");
    }

    fn compile_single_pattern(pattern: &str, token_type: &str) -> CompiledSyntax {
        let def = SyntaxDefinition {
            name: "Test".into(),
            patterns: vec![crate::editor::syntax::PatternRule {
                pattern: Some(PatternSpec::Single(pattern.into())),
                regex: None,
                token_type: TokenType::Single(token_type.into()),
                syntax: None,
            }],
            ..Default::default()
        };
        compile_from_definition(&def).unwrap()
    }

    fn markdown_syntax() -> CompiledSyntax {
        let defs = crate::editor::syntax::load_syntax_assets(&data_dir());
        let def = defs
            .into_iter()
            .find(|def| def.name == "Markdown")
            .expect("should load Markdown syntax asset");
        compile_from_definition(&def).expect("should compile Markdown syntax")
    }

    #[test]
    fn char_len_ascii() {
        assert_eq!(char_len("hello"), 5);
    }

    #[test]
    fn char_len_multibyte() {
        assert_eq!(char_len("\u{00E9}\u{00E8}"), 2);
    }

    #[test]
    fn usub_basic() {
        assert_eq!(usub("hello", 2, 4), "ell");
    }

    #[test]
    fn usub_full() {
        assert_eq!(usub("hello", 1, 5), "hello");
    }

    #[test]
    fn usub_empty() {
        assert_eq!(usub("hello", 3, 2), "");
    }

    #[test]
    fn prefix_ulen_basic() {
        assert_eq!(prefix_ulen("hello", 3), 3);
    }

    #[test]
    fn prefix_ulen_multibyte() {
        let s = "\u{00E9}bc";
        assert_eq!(prefix_ulen(s, 2), 1); // 2 bytes = 1 char for e-acute
    }

    #[test]
    fn ucharpos_basic() {
        assert_eq!(ucharpos("hello", 3), Some(3));
        assert_eq!(ucharpos("hello", 0), Some(1));
    }

    #[test]
    fn split_anchor_with_caret() {
        let (code, anchored) = split_anchor("^foo".to_string());
        assert_eq!(code, "foo");
        assert!(anchored);
    }

    #[test]
    fn split_anchor_without() {
        let (code, anchored) = split_anchor("foo".to_string());
        assert_eq!(code, "foo");
        assert!(!anchored);
    }

    #[test]
    fn push_token_merges_same_type() {
        let mut tokens = Vec::new();
        push_token(&mut tokens, "normal", "hello");
        push_token(&mut tokens, "normal", " world");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "hello world");
    }

    #[test]
    fn push_token_merges_whitespace_into_next() {
        let mut tokens = Vec::new();
        push_token(&mut tokens, "normal", "  ");
        push_token(&mut tokens, "keyword", "if");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "  if");
        assert_eq!(tokens[0].token_type, "keyword");
    }

    #[test]
    fn push_token_separates_different_types() {
        let mut tokens = Vec::new();
        push_token(&mut tokens, "keyword", "if");
        push_token(&mut tokens, "normal", " x");
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn compile_from_definition_basic() {
        let def = SyntaxDefinition {
            name: "Test".into(),
            patterns: vec![crate::editor::syntax::PatternRule {
                pattern: Some(PatternSpec::Single("%w+".into())),
                regex: None,
                token_type: TokenType::Single("symbol".into()),
                syntax: None,
            }],
            symbols: HashMap::from([("if".into(), "keyword".into())]),
            ..Default::default()
        };
        let compiled = compile_from_definition(&def).unwrap();
        assert_eq!(compiled.patterns.len(), 1);
        assert_eq!(compiled.symbols.get("if"), Some(&"keyword".to_string()));
    }

    #[test]
    fn compile_from_definition_regex() {
        let def = SyntaxDefinition {
            name: "RegexTest".into(),
            patterns: vec![crate::editor::syntax::PatternRule {
                pattern: None,
                regex: Some(PatternSpec::Single(r"\d+".into())),
                token_type: TokenType::Single("number".into()),
                syntax: None,
            }],
            symbols: HashMap::new(),
            ..Default::default()
        };
        let compiled = compile_from_definition(&def).unwrap();
        assert_eq!(compiled.patterns.len(), 1);
        assert!(matches!(
            compiled.patterns[0].matcher,
            PatternMatcher::Single(MatcherDef {
                kind: MatcherKind::Regex { .. },
                ..
            })
        ));
    }

    #[test]
    fn regex_find_basic() {
        let matcher = make_matcher("regex", r"\d+".to_string()).unwrap();
        let results = regex_find(&matcher, "abc 123 def", 1, false).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0], 5); // '1' is at char position 5
        assert_eq!(results[1], 7); // '3' is at char position 7
    }

    #[test]
    fn lua_pattern_to_regex_uses_unicode_properties() {
        assert_eq!(lua_pattern_to_regex("%a"), r"\p{L}");
        assert_eq!(lua_pattern_to_regex("[%a_]"), r"[\p{L}_]");
        assert_eq!(lua_pattern_to_regex("[%w_]"), r"[\w\p{M}_]");
        assert_eq!(lua_pattern_to_regex("%u"), r"\p{Lu}");
        assert_eq!(lua_pattern_to_regex("%l"), r"\p{Ll}");
    }

    #[test]
    fn tokenize_line_matches_unicode_words() {
        let syntax = compile_single_pattern("[%a_][%w_]*", "symbol");
        for line in [
            "\u{00E1}rv\u{00ED}zt\u{0171}r\u{0151}",
            "f\u{00FC}ggv\u{00E9}ny_1",
            "a\u{0301}r",
        ] {
            let tokens = tokenize_line(&syntax, line);
            assert_eq!(
                tokens,
                vec![Token {
                    token_type: "symbol".into(),
                    text: line.into(),
                }],
                "expected full-line symbol token for {line:?}"
            );
        }
    }

    #[test]
    fn tokenize_line_matches_unicode_case_classes() {
        let syntax = compile_single_pattern("<[%u%l][%w_%.:-]*>", "tag");
        let line = "<\u{00C1}rWidget>";
        let tokens = tokenize_line(&syntax, line);
        assert_eq!(
            tokens,
            vec![Token {
                token_type: "tag".into(),
                text: line.into(),
            }]
        );
    }

    #[test]
    fn markdown_asset_highlights_unicode_italic_text() {
        let syntax = markdown_syntax();
        let line = "_\u{00E1}rv\u{00ED}zt\u{0171}r\u{0151}_";
        let tokens = tokenize_line(&syntax, line);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            line
        );
        assert!(
            tokens.iter().any(|token| token.token_type != "normal"),
            "expected Markdown syntax to apply a non-normal token to {line:?}"
        );
    }

    fn python_syntax() -> CompiledSyntax {
        let defs = crate::editor::syntax::load_syntax_assets(&data_dir());
        let def = defs
            .into_iter()
            .find(|def| def.name == "Python")
            .expect("should load Python syntax asset");
        compile_from_definition(&def).expect("should compile Python syntax")
    }

    #[test]
    fn python_for_loop_tokenizes_keywords_and_close() {
        // Regression: the simplified tokenizer used to collapse pair-syntax
        // patterns (for/if/etc.) into one flat token, eating the entire line
        // including the `:` and beyond. With sub-syntax descent, `for` is
        // keyword, `in` is keyword (via symbol map), and the close `:`
        // arrives after the bracketed `[1:]`, not the first colon inside it.
        let syntax = python_syntax();
        let line = "for f in sys.argv[1:]: cat(f)";
        let tokens = tokenize_line(&syntax, line);
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, line, "every byte must round-trip");
        let has_for_keyword = tokens
            .iter()
            .any(|t| t.text == "for" && t.token_type == "keyword");
        assert!(has_for_keyword, "`for` should be keyword, got {tokens:?}");
        let has_in_keyword = tokens
            .iter()
            .any(|t| t.text == "in" && t.token_type == "keyword");
        assert!(has_in_keyword, "`in` should be keyword, got {tokens:?}");
        let has_cat_function = tokens
            .iter()
            .any(|t| t.text == "cat" && t.token_type == "function");
        assert!(has_cat_function, "`cat` should be function, got {tokens:?}");
    }

    #[test]
    fn markdown_asset_highlights_unicode_heading_text() {
        let syntax = markdown_syntax();
        let line = "# \u{00C1}rv\u{00ED}zt\u{0171}r\u{0151} t\u{00FC}k\u{00F6}rf\u{00FA}r\u{00F3}g\u{00E9}p {#arvizturo}";
        let tokens = tokenize_line(&syntax, line);
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            joined, line,
            "every byte should round-trip through the token stream"
        );
        let first = tokens.first().expect("at least one token");
        assert_eq!(first.token_type, "keyword");
        assert!(
            first
                .text
                .starts_with("# \u{00C1}rv\u{00ED}zt\u{0171}r\u{0151}"),
            "heading text should be one keyword run, got {first:?}"
        );
    }
}
