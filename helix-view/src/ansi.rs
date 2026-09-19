//! Minimal ANSI/SGR parser used by the native scratch output buffer.
//!
//! Converts raw ANSI-decorated text (as produced by subprocesses run with
//! `:run-here`/`:debug-here` and captured cargo/libtest/DAP output) into
//! plain, searchable text plus a list of styled character ranges suitable
//! for rendering as an overlay on top of a [`Document`](crate::Document).
//!
//! Only SGR (`ESC [ ... m`) sequences are interpreted for styling. All other
//! control sequences (other CSI sequences, OSC sequences, single-character
//! escapes) are stripped from the output text without affecting styling, so
//! that terminal escapes never leak into the buffer or its search index.
//! Malformed/unterminated sequences are dropped defensively; parsing never
//! panics on arbitrary input.

use std::ops::Range;

use crate::graphics::{Color, Modifier, Style};

const ESC: char = '\u{1b}';
const BEL: char = '\u{7}';

/// A styled span over the *parsed* (escape-free) output text, expressed as a
/// half-open char range.
pub type StyledSpan = (Range<usize>, Style);

/// Parses `input`, returning the plain text with all ANSI escape sequences
/// removed, along with the list of styled spans (in char offsets into the
/// returned text) derived from SGR sequences. Spans are emitted in order and
/// are non-overlapping.
pub fn parse_ansi(input: &str) -> (String, Vec<StyledSpan>) {
    let mut text = String::with_capacity(input.len());
    let mut spans = Vec::new();

    let mut style = Style::default();
    let mut span_start = 0usize;
    let mut char_idx = 0usize;

    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ESC {
            text.push(c);
            char_idx += 1;
            continue;
        }

        match chars.peek() {
            Some('[') => {
                chars.next(); // consume '['
                let params = consume_csi_params(&mut chars);
                // Only SGR (final byte 'm') sequences carry styling; other
                // CSI sequences (cursor movement, erase, etc.) are simply
                // dropped from the text.
                if let Some(final_byte) = params.final_byte {
                    if final_byte == 'm' {
                        let new_style = apply_sgr(style, &params.args);
                        if new_style != style {
                            if char_idx > span_start && !style_is_default(style) {
                                spans.push((span_start..char_idx, style));
                            }
                            style = new_style;
                            span_start = char_idx;
                        }
                    }
                }
                // Malformed/unterminated CSI: params.final_byte is None,
                // nothing further to do; already consumed to end of input.
            }
            Some(']') => {
                chars.next(); // consume ']'
                consume_osc(&mut chars);
            }
            Some(_) => {
                // Single-character escape (e.g. ESC(B charset select, ESC=,
                // ESC>). Consume the one following character, if any, and
                // drop both from the output.
                chars.next();
            }
            None => {
                // Trailing bare ESC with nothing after it: drop it.
            }
        }
    }

    if char_idx > span_start && !style_is_default(style) {
        spans.push((span_start..char_idx, style));
    }

    (text, spans)
}

/// Strips all ANSI escape sequences from `input`, returning plain text only.
/// Uses the same tokenizer as [`parse_ansi`] but discards styling
/// information; intended for feeding text-only consumers (status lines,
/// cargo/libtest's own plain-text parsers) without requiring a second,
/// separate parser implementation.
pub fn ansi_to_plain_text(input: &str) -> String {
    parse_ansi(input).0
}

fn style_is_default(style: Style) -> bool {
    style == Style::default()
}

struct CsiParams {
    args: Vec<i64>,
    final_byte: Option<char>,
}

/// Consumes CSI parameter/intermediate bytes up to and including the final
/// byte (0x40..=0x7E), returning the parsed numeric arguments (defaulting
/// missing/empty fields to no explicit value, represented here as -1 to
/// distinguish "no value" e.g. bare `\x1b[m` == reset). Returns
/// `final_byte: None` if the input ends before a final byte is found
/// (malformed/unterminated sequence); the iterator will have been drained to
/// the end in that case, which is safe (just stops interpreting further
/// escapes).
fn consume_csi_params(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> CsiParams {
    let mut raw = String::new();
    let mut final_byte = None;
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            final_byte = Some(c);
            break;
        }
        raw.push(c);
    }

    let args = raw
        .split(';')
        .map(|field| field.parse::<i64>().unwrap_or(-1))
        .collect();

    CsiParams { args, final_byte }
}

/// Consumes an OSC (`ESC ] ...`) sequence up to its terminator: either BEL
/// (`\x07`) or the two-character ST (`ESC \`). Defensive against
/// unterminated sequences: simply drains to end of input.
fn consume_osc(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(c) = chars.next() {
        if c == BEL {
            return;
        }
        if c == ESC {
            if chars.peek() == Some(&'\\') {
                chars.next();
            }
            return;
        }
    }
}

fn apply_sgr(mut style: Style, args: &[i64]) -> Style {
    let mut i = 0;
    // A bare `\x1b[m` parses to a single empty field -> [-1], which is a
    // reset, matching the "no args" ECMA-48 convention.
    if args.is_empty() {
        return Style::default();
    }
    while i < args.len() {
        let code = args[i];
        match code {
            -1 | 0 => style = Style::default(),
            1 => style.add_modifier |= Modifier::BOLD,
            2 => style.add_modifier |= Modifier::DIM,
            3 => style.add_modifier |= Modifier::ITALIC,
            4 => style.underline_style = Some(crate::graphics::UnderlineStyle::Line),
            5 | 6 => style.add_modifier |= Modifier::SLOW_BLINK,
            7 => style.add_modifier |= Modifier::REVERSED,
            8 => style.add_modifier |= Modifier::HIDDEN,
            9 => style.add_modifier |= Modifier::CROSSED_OUT,
            21 => style.underline_style = Some(crate::graphics::UnderlineStyle::DoubleLine),
            22 => {
                style.add_modifier.remove(Modifier::BOLD | Modifier::DIM);
            }
            23 => {
                style.add_modifier.remove(Modifier::ITALIC);
            }
            24 => style.underline_style = None,
            25 => {
                style
                    .add_modifier
                    .remove(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK);
            }
            27 => {
                style.add_modifier.remove(Modifier::REVERSED);
            }
            28 => {
                style.add_modifier.remove(Modifier::HIDDEN);
            }
            29 => {
                style.add_modifier.remove(Modifier::CROSSED_OUT);
            }
            30..=37 => style.fg = Some(ansi_16_color((code - 30) as u8)),
            38 => {
                if let Some((color, consumed)) = extended_color(&args[i + 1..]) {
                    style.fg = Some(color);
                    i += consumed;
                } else {
                    // Malformed extended color sequence: stop interpreting
                    // remaining args defensively rather than misparsing.
                    break;
                }
            }
            39 => style.fg = None,
            40..=47 => style.bg = Some(ansi_16_color((code - 40) as u8)),
            48 => {
                if let Some((color, consumed)) = extended_color(&args[i + 1..]) {
                    style.bg = Some(color);
                    i += consumed;
                } else {
                    break;
                }
            }
            49 => style.bg = None,
            90..=97 => style.fg = Some(ansi_16_color((code - 90) as u8 + 8)),
            100..=107 => style.bg = Some(ansi_16_color((code - 100) as u8 + 8)),
            _ => {}
        }
        i += 1;
    }
    style
}

fn ansi_16_color(index: u8) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::Gray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

/// Parses the `5;N` (indexed) or `2;R;G;B` (truecolor) tail of a `38`/`48`
/// SGR extended color sequence. Returns the resolved color and the number of
/// trailing args consumed (not counting the `38`/`48` selector itself).
/// Returns `None` on malformed/incomplete input, guarding against panics on
/// truncated escape sequences.
fn extended_color(rest: &[i64]) -> Option<(Color, usize)> {
    match rest.first()? {
        5 => {
            let idx = *rest.get(1)?;
            if !(0..=255).contains(&idx) {
                return None;
            }
            Some((Color::Indexed(idx as u8), 2))
        }
        2 => {
            let r = *rest.get(1)?;
            let g = *rest.get(2)?;
            let b = *rest.get(3)?;
            if ![r, g, b].iter().all(|v| (0..=255).contains(v)) {
                return None;
            }
            Some((Color::Rgb(r as u8, g as u8, b as u8), 4))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_spans_use_plain_text_character_offsets() {
        let (text, spans) = parse_ansi("plain \x1b[38;2;1;2;3m彩色\x1b[0m end");

        assert_eq!(text, "plain 彩色 end");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, 6..8);
        assert_eq!(spans[0].1.fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn terminal_controls_never_enter_searchable_text() {
        let text =
            ansi_to_plain_text("before\x1b]8;;https://example.com\x07link\x1b]8;;\x07\x1b[2Kafter");

        assert_eq!(text, "beforelinkafter");
    }

    #[test]
    fn incomplete_extended_colour_keeps_following_text_unstyled() {
        let (text, spans) = parse_ansi("\x1b[38;2;1mvisible");

        assert_eq!(text, "visible");
        assert!(spans.is_empty());
    }
}
