// Derived from typst-syntax v0.15.1, crates/typst-syntax/src/lexer.rs.
// Copyright The Typst Project Developers. Licensed under Apache-2.0
// (third_party/typst/LICENSE). Modified for Folio.

use std::num::IntErrorKind;

use ecow::{EcoString, EcoVec, eco_format, eco_vec};
use unicode_ident::{is_xid_continue, is_xid_start};
use unicode_math_class::MathClass;
use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;
use unscanny::Scanner;

use crate::utils::default_math_class;
use crate::{SyntaxKind, SyntaxMode, SyntaxNode};

/// An iterator over a source code string which returns tokens.
#[derive(Clone)]
pub(super) struct Lexer<'s> {
    /// The scanner: contains the underlying string and location as a "cursor".
    s: Scanner<'s>,
    /// The mode the lexer is in. This determines which kinds of tokens it
    /// produces.
    mode: SyntaxMode,
    /// Whether the last token contained a newline.
    newline: bool,
    /// Whether the text is a stylesheet, where code also has colours
    /// (`#1f5fa8`), id selectors (`#{fig:ball}`), token names (`$accent`)
    /// and ratios (`16:9`).
    style: bool,
    /// An error plus hints for the current token being produced. This is always
    /// `None` between calls to [`Lexer::next`].
    error: Option<(EcoString, EcoVec<EcoString>)>,
}

impl<'s> Lexer<'s> {
    /// Create a new lexer with the given mode and a prefix to offset column
    /// calculations.
    pub(crate) fn new(text: &'s str, mode: SyntaxMode) -> Self {
        Self {
            s: Scanner::new(text),
            mode,
            newline: false,
            style: false,
            error: None,
        }
    }

    /// Whether code is lexed as in a stylesheet.
    pub(crate) fn style(&self) -> bool {
        self.style
    }

    /// Lex a stylesheet: in code, `#` starts a colour or an id selector, `$`
    /// a token name, and two integers joined by `:` form a ratio.
    pub(crate) fn set_style(&mut self, style: bool) {
        self.style = style;
    }

    /// Get the current lexing mode.
    pub(crate) fn mode(&self) -> SyntaxMode {
        self.mode
    }

    /// Change the lexing mode.
    pub(crate) fn set_mode(&mut self, mode: SyntaxMode) {
        self.mode = mode;
    }

    /// The index in the string at which the last token ends and next token
    /// will start.
    pub(crate) fn cursor(&self) -> usize {
        self.s.cursor()
    }

    /// Jump to the given index in the string.
    pub(crate) fn jump(&mut self, index: usize) {
        self.s.jump(index);
    }

    /// Whether the last token contained a newline.
    pub(crate) fn newline(&self) -> bool {
        self.newline
    }

    /// The number of characters until the most recent newline from an index.
    pub(crate) fn column(&self, index: usize) -> usize {
        let mut s = self.s; // Make a new temporary scanner (cheap).
        s.jump(index);
        s.before()
            .chars()
            .rev()
            .take_while(|&c| !is_newline(c))
            .count()
    }
}

impl Lexer<'_> {
    /// Construct a full-positioned syntax error.
    fn error(&mut self, message: impl Into<EcoString>) -> SyntaxKind {
        debug_assert!(self.error.is_none());
        self.error = Some((message.into(), eco_vec![]));
        SyntaxKind::Error
    }

    /// If the current node is an error, adds a hint.
    fn hint(&mut self, message: impl Into<EcoString>) {
        if let Some((_message, hints)) = &mut self.error {
            hints.push(message.into());
        }
    }
}

/// Shared methods with all [`SyntaxMode`].
impl Lexer<'_> {
    /// Return the next token in our text. Returns both the [`SyntaxNode`]
    /// and the raw [`SyntaxKind`] to make it more ergonomic to check the kind
    pub(crate) fn next(&mut self) -> (SyntaxKind, SyntaxNode) {
        debug_assert!(self.error.is_none());
        let start = self.s.cursor();

        self.newline = false;
        let kind = match self.s.eat() {
            Some(c) if is_space(c, self.mode) => self.whitespace(start, c),
            Some('#') if start == 0 && self.s.eat_if('!') => self.shebang(),
            Some('/') if self.s.eat_if('/') => self.line_comment(),
            Some('/') if self.s.eat_if('*') => self.block_comment(),
            Some('*') if self.s.eat_if('/') => {
                let error = self.error("unexpected end of block comment");
                self.hint(
                    "consider escaping the `*` with a backslash or \
                     opening the block comment with `/*`",
                );
                error
            }
            Some('`') if self.mode != SyntaxMode::Math => return self.raw(),
            Some('$') if self.mode != SyntaxMode::Math && self.at_math_mode() => {
                return self.tex_equation(start);
            }
            Some('#') if self.style && self.mode == SyntaxMode::Code && self.s.at('{') => {
                return self.id_selector(start);
            }
            Some(c) => match self.mode {
                SyntaxMode::Markup => self.markup(start, c),
                SyntaxMode::Math => match self.math(start, c) {
                    (kind, None) => kind,
                    (kind, Some(node)) => return (kind, node),
                },
                SyntaxMode::Code => self.code(start, c),
            },

            None => SyntaxKind::End,
        };

        let text = self.s.from(start);
        let node = match self.error.take() {
            Some((message, hints)) => SyntaxNode::error(message, text).with_hints(hints),
            None => SyntaxNode::leaf(kind, text),
        };
        (kind, node)
    }

    /// Eat whitespace characters greedily.
    fn whitespace(&mut self, start: usize, c: char) -> SyntaxKind {
        let more = self.s.eat_while(|c| is_space(c, self.mode));
        let newlines = match c {
            // Optimize eating a single space.
            ' ' if more.is_empty() => 0,
            _ => count_newlines(self.s.from(start)),
        };

        self.newline = newlines > 0;
        if self.mode == SyntaxMode::Markup && newlines >= 2 {
            SyntaxKind::Parbreak
        } else {
            SyntaxKind::Space
        }
    }

    fn shebang(&mut self) -> SyntaxKind {
        self.s.eat_until(is_newline);
        SyntaxKind::Shebang
    }

    fn line_comment(&mut self) -> SyntaxKind {
        self.s.eat_until(is_newline);
        SyntaxKind::LineComment
    }

    fn block_comment(&mut self) -> SyntaxKind {
        let mut state = '_';
        let mut depth = 1;

        // Find the first `*/` that does not correspond to a nested `/*`.
        while let Some(c) = self.s.eat() {
            state = match (state, c) {
                ('*', '/') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    '_'
                }
                ('/', '*') => {
                    depth += 1;
                    '_'
                }
                _ => c,
            }
        }

        SyntaxKind::BlockComment
    }
}

/// Raw.
impl Lexer<'_> {
    /// We parse entire raw segments in the lexer as a convenience to avoid
    /// going to and from the parser for each raw section.
    fn raw(&mut self) -> (SyntaxKind, SyntaxNode) {
        let start = self.s.cursor() - 1;

        // Determine number of opening backticks.
        let mut backticks = 1;
        while self.s.eat_if('`') {
            backticks += 1;
        }

        // Special case for ``.
        if backticks == 2 {
            let nodes = vec![
                SyntaxNode::leaf(SyntaxKind::RawDelim, "`"),
                SyntaxNode::leaf(SyntaxKind::RawDelim, "`"),
            ];
            return (SyntaxKind::Raw, SyntaxNode::inner(SyntaxKind::Raw, nodes));
        }

        // Find end of raw text.
        let mut found = 0;
        while found < backticks {
            match self.s.eat() {
                Some('`') => found += 1,
                Some(_) => found = 0,
                None => {
                    let message = "unclosed raw text";
                    let error = SyntaxNode::error(message, self.s.from(start));
                    return (SyntaxKind::Error, error);
                }
            }
        }
        let end = self.s.cursor();

        let mut inner = Scanner::new(self.s.get(start + backticks..end - backticks));
        let inner_len = inner.string().len();

        // Opening delimiter.
        let delim = SyntaxNode::leaf(SyntaxKind::RawDelim, self.s.from(end - backticks));
        let mut nodes = vec![delim.clone()];

        let mut tag = None;
        if delim.len() >= 3 {
            tag = Self::raw_lang_tag(&mut inner);
            if let Some(tag) = tag {
                nodes.push(SyntaxNode::leaf(SyntaxKind::RawLang, tag));
            }
            Self::blocky_raw(&mut inner, &mut nodes);
        } else {
            Self::inline_raw(&mut inner, &mut nodes);
        }

        // Closing delimiter.
        nodes.push(delim);

        let mut raw = SyntaxNode::inner(SyntaxKind::Raw, nodes);

        // A tag with no text after it is likely meant as the text.
        if let Some(tag) = tag
            && inner_len == tag.len()
        {
            raw.warn("empty raw text");
            raw.hint(eco_format!("`{tag}` is read as the language tag"));
            let tag_range = backticks..backticks + tag.len();
            raw.hint_at(
                tag_range,
                "to treat this as text, add a space after the initial backticks",
            );
        }

        (SyntaxKind::Raw, raw)
    }

    /// Lex the raw language tag: all text up to the first whitespace or
    /// backtick, so tags such as `c++` or `html.j2` work.
    fn raw_lang_tag<'a>(s: &mut Scanner<'a>) -> Option<&'a str> {
        let tag = s.eat_until(|c: char| c.is_whitespace() || c == '`');
        (!tag.is_empty()).then_some(tag)
    }

    /// Raw blocks parse a language tag, have smart behavior for trimming
    /// whitespace in the start/end lines, and trim common leading whitespace
    /// from all other lines as the "dedent". The exact behavior is described
    /// below.
    ///
    /// ### The initial line:
    /// - The language tag is already handled above.
    /// - We check the rest of the line and if all characters are whitespace,
    ///   trim it. Otherwise we trim a single leading space if present.
    ///   - If more trimmed characters follow on future lines, they will be
    ///     merged into the same trimmed element.
    /// - If we didn't trim the entire line, the rest is kept as text.
    ///
    /// ### Inner lines:
    /// - We determine the "dedent" by iterating over the lines. The dedent is
    ///   the minimum number of leading whitespace characters (not bytes) before
    ///   each line that has any non-whitespace characters.
    ///   - The opening delimiter's line does not contribute to the dedent, but
    ///     the closing delimiter's line does (even if that line is entirely
    ///     whitespace up to the delimiter).
    /// - We then trim the newline and dedent characters of each line, and add a
    ///   (potentially empty) text element of all remaining characters.
    ///
    /// ### The final line:
    /// - If the last line is entirely whitespace, it is trimmed.
    /// - Otherwise its text is kept like an inner line. However, if the last
    ///   non-whitespace character of the final line is a backtick, then one
    ///   ascii space (if present) is trimmed from the end.
    fn blocky_raw(s: &mut Scanner<'_>, nodes: &mut Vec<SyntaxNode>) {
        // The lines between the backticks.
        let mut lines = split_newlines(s.after());

        // Determine dedent level.
        let dedent = lines
            .iter()
            .skip(1)
            .filter(|line| !line.chars().all(char::is_whitespace))
            // The line with the closing ``` is always taken into account
            .chain(lines.last())
            .map(|line| line.chars().take_while(|c| c.is_whitespace()).count())
            .min()
            .unwrap_or(0);

        // Trim whitespace from the last line. Will be added as a `RawTrimmed`
        // kind by the check for `self.s.cursor() != inner_end` below.
        if lines
            .last()
            .is_some_and(|last| last.chars().all(char::is_whitespace))
        {
            lines.pop();
        } else if let Some(last) = lines.last_mut() {
            // If last line ends in a backtick, try to trim a single space. This
            // check must happen before we add the first line since the last and
            // first lines might be the same.
            if last.trim_end().ends_with('`') {
                *last = last.strip_suffix(' ').unwrap_or(last);
            }
        }

        // A closure for pushing a leaf node and updating the cursor in one
        // step.
        let mut prev = s.cursor();
        let mut push_leaf = |kind, s: &Scanner<'_>| {
            nodes.push(SyntaxNode::leaf(kind, s.from(prev)));
            prev = s.cursor();
        };

        let mut lines = lines.into_iter();

        // Handle the first line: trim if all whitespace, or trim a single space
        // at the start. Note that the first line does not affect the dedent
        // value.
        if let Some(first_line) = lines.next() {
            if first_line.chars().all(char::is_whitespace) {
                s.advance(first_line.len());
                // This is the only spot we advance the scanner, but don't
                // immediately call `push_leaf`. But the rest of the function
                // ensures we will always add this text to a `RawTrimmed` later.
                debug_assert!(!s.done());
                // A proof by cases follows:
                // # First case: The loop runs
                // If the loop runs, there must be a newline following, so
                // `cursor != inner_end`. And if the loop runs, the first thing
                // it does is add a trimmed element.
                // # Second case: The final if-statement runs.
                // To _not_ reach the loop from here, we must have only one or
                // two lines:
                // 1. If one line, we cannot be here, because the first and last
                //    lines are the same, so this line will have been removed by
                //    the check for the last line being all whitespace.
                // 2. If two lines, the loop will run unless the last is fully
                //    whitespace, but if it is, it will have been popped, then
                //    the final if-statement will run because the text removed
                //    by the last line must include at least a newline, so
                //    `cursor != inner_end` here.
            } else {
                let line_end = s.cursor() + first_line.len();
                if s.eat_if(' ') {
                    // Trim a single space after the lang tag or backticks on
                    // the first line.
                    push_leaf(SyntaxKind::RawTrimmed, s);
                }
                // We know here that the rest of the line is non-empty.
                s.jump(line_end);
                push_leaf(SyntaxKind::Text, s);
            }
        }

        // Add lines.
        for line in lines {
            let offset: usize = line.chars().take(dedent).map(char::len_utf8).sum();
            s.eat_newline();
            s.advance(offset);
            push_leaf(SyntaxKind::RawTrimmed, s);
            s.advance(line.len() - offset);
            push_leaf(SyntaxKind::Text, s);
        }

        // Add final trimmed.
        if !s.done() {
            nodes.push(SyntaxNode::leaf(SyntaxKind::RawTrimmed, s.after()));
        }
    }

    /// Inline raw text is split on lines with non-newlines as `Text` kinds and
    /// newlines as `RawTrimmed`. Inline raw text does not dedent the text, all
    /// non-newline whitespace is kept.
    fn inline_raw(s: &mut Scanner<'_>, nodes: &mut Vec<SyntaxNode>) {
        let mut prev = s.cursor();
        while !s.done() {
            if s.at(is_newline) {
                nodes.push(SyntaxNode::leaf(SyntaxKind::Text, s.from(prev)));
                prev = s.cursor();
                s.eat_newline();
                nodes.push(SyntaxNode::leaf(SyntaxKind::RawTrimmed, s.from(prev)));
                prev = s.cursor();
                continue;
            }
            s.eat();
        }
        nodes.push(SyntaxNode::leaf(SyntaxKind::Text, s.from(prev)));
    }
}

/// Markup.
impl Lexer<'_> {
    fn markup(&mut self, start: usize, c: char) -> SyntaxKind {
        match c {
            '\\' => self.backslash(),
            'h' if self.s.eat_if("ttp://") => self.link(),
            'h' if self.s.eat_if("ttps://") => self.link(),
            '<' if self.s.at(is_label_start) => self.label(),
            '@' if ref_name_len(self.s.after()) > 0 => self.ref_marker(),

            '.' if self.s.eat_if("..") => SyntaxKind::Shorthand,
            '-' if self.s.eat_if("--") => SyntaxKind::Shorthand,
            '-' if self.s.eat_if('-') => SyntaxKind::Shorthand,
            '-' if self.s.eat_if('?') => SyntaxKind::Shorthand,
            '-' if self.s.at(char::is_numeric) => SyntaxKind::Shorthand,
            '*' if !self.in_word() => SyntaxKind::Star,
            '_' if !self.in_word() => SyntaxKind::Underscore,

            '#' => SyntaxKind::Hash,
            '[' => SyntaxKind::LeftBracket,
            ']' => SyntaxKind::RightBracket,
            '\'' => SyntaxKind::SmartQuote,
            '"' => SyntaxKind::SmartQuote,
            '$' => SyntaxKind::Dollar,
            '~' => SyntaxKind::Shorthand,
            ':' => SyntaxKind::Colon,
            '=' => {
                self.s.eat_while('=');
                if self.space_or_end() {
                    SyntaxKind::HeadingMarker
                } else {
                    self.text()
                }
            }
            '-' if self.space_or_end() => SyntaxKind::ListMarker,
            '+' if self.space_or_end() => SyntaxKind::EnumMarker,
            '/' if self.space_or_end() => SyntaxKind::TermMarker,
            '0'..='9' => self.numbering(start),

            _ => self.text(),
        }
    }

    fn backslash(&mut self) -> SyntaxKind {
        if self.s.eat_if("u{") {
            let hex = self.s.eat_while(char::is_ascii_alphanumeric);
            if !self.s.eat_if('}') {
                return self.error("unclosed Unicode escape sequence");
            }

            if u32::from_str_radix(hex, 16)
                .ok()
                .and_then(std::char::from_u32)
                .is_none()
            {
                return self.error(eco_format!("invalid Unicode codepoint: {hex}"));
            }

            return SyntaxKind::Escape;
        }

        if self.s.done() || self.s.at(char::is_whitespace) {
            SyntaxKind::Linebreak
        } else {
            self.s.eat();
            SyntaxKind::Escape
        }
    }

    fn link(&mut self) -> SyntaxKind {
        let (link, balanced) = link_prefix(self.s.after());
        self.s.advance(link.len());

        if !balanced {
            return self.error(
                "automatic links cannot contain unbalanced brackets, \
                 use the `link` function instead",
            );
        }

        SyntaxKind::Link
    }

    fn numbering(&mut self, start: usize) -> SyntaxKind {
        self.s.eat_while(char::is_ascii_digit);

        let read = self.s.from(start);
        if self.s.eat_if('.') && self.space_or_end() && read.parse::<u64>().is_ok() {
            return SyntaxKind::EnumMarker;
        }

        self.text()
    }

    /// Lex a reference marker. A label never ends with `:` or `-`, so these
    /// are left as punctuation: in `see @fig:ball: it shows`, the reference
    /// is `@fig:ball`.
    fn ref_marker(&mut self) -> SyntaxKind {
        let len = ref_name_len(self.s.after());
        self.s.advance(len);
        SyntaxKind::RefMarker
    }
}

/// Labels (shared by markup and code).
impl Lexer<'_> {
    /// Lex a label after its `<`, which is followed by a valid start
    /// character.
    fn label(&mut self) -> SyntaxKind {
        let name = self.s.eat_while(is_valid_in_label_literal);

        // A dot separates a label from a published value
        // (`<timer:task>.remaining`), so it cannot be part of the name. Report
        // `<a.b>` as one error rather than an unclosed label followed by text.
        if self.s.at('.') {
            let mut s = self.s;
            s.eat_while(|c| is_valid_in_label_literal(c) || c == '.');
            if s.eat_if('>') {
                self.s = s;
                let error = self.error("labels cannot contain `.`");
                self.hint(
                    "a dot separates a label from a published value, \
                     as in `<timer:task>.remaining`",
                );
                return error;
            }
        }

        if !self.s.eat_if('>') {
            return self.error("unclosed label");
        }

        if let Some(last @ (':' | '-')) = name.chars().next_back() {
            return self.error(eco_format!("labels cannot end with `{last}`"));
        }

        SyntaxKind::Label
    }
}

/// LaTeX math.
impl Lexer<'_> {
    /// Whether the `$` just eaten starts a math input mode: `$:tex`.
    fn at_math_mode(&self) -> bool {
        let mut s = self.s;
        s.eat_if(':') && s.at(is_id_start)
    }

    /// Lex a whole equation in an input mode other than native math after
    /// its opening `$`, such as `$:tex \frac{1}{2} m v^2$`. The body is kept
    /// verbatim up to the first `$` that is not escaped with a backslash.
    fn tex_equation(&mut self, start: usize) -> (SyntaxKind, SyntaxNode) {
        let mut nodes = vec![];

        let mode_start = self.s.cursor();
        self.s.eat_if(':');
        self.s.eat_while(is_id_continue);
        let mode = self.s.from(mode_start);
        nodes.push(if mode == ":tex" {
            SyntaxNode::leaf(SyntaxKind::MathMode, mode)
        } else {
            SyntaxNode::error(eco_format!("unknown math input mode: `{mode}`"), mode)
                .with_hints(eco_vec!["LaTeX math is written `$:tex ...$`".into()])
        });

        let body_start = self.s.cursor();
        let mut escaped = false;
        self.s.eat_until(|c| {
            let stop = c == '$' && !escaped;
            escaped = c == '\\' && !escaped;
            stop
        });
        let body = self.s.from(body_start);
        if !body.is_empty() {
            nodes.push(SyntaxNode::leaf(SyntaxKind::TexText, body));
        }

        let open = self.s.get(start..mode_start);
        if self.s.eat_if('$') {
            nodes.insert(0, SyntaxNode::leaf(SyntaxKind::Dollar, open));
            nodes.push(SyntaxNode::leaf(SyntaxKind::Dollar, "$"));
        } else {
            nodes.insert(0, SyntaxNode::error("unclosed delimiter", open));
        }

        (
            SyntaxKind::TexEquation,
            SyntaxNode::inner(SyntaxKind::TexEquation, nodes),
        )
    }

    fn text(&mut self) -> SyntaxKind {
        macro_rules! table {
            ($(|$c:literal)*) => {
                static TABLE: [bool; 128] = {
                    let mut t = [false; 128];
                    $(t[$c as usize] = true;)*
                    t
                };
            };
        }

        table! {
            | ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\\' | '/'
            | '[' | ']' | '~' | '-' | '.' | '\'' | '"' | '*' | '_'
            | ':' | 'h' | '`' | '$' | '<' | '>' | '@' | '#'
        };

        loop {
            self.s.eat_until(|c: char| {
                TABLE
                    .get(c as usize)
                    .copied()
                    .unwrap_or_else(|| c.is_whitespace())
            });

            // Continue with the same text node if the thing would become text
            // anyway.
            let mut s = self.s;
            match s.eat() {
                Some(' ') if s.at(char::is_alphanumeric) => {}
                Some('/') if !s.at(['/', '*']) => {}
                Some('-') if !s.at(['-', '?']) => {}
                Some('.') if !s.at("..") => {}
                Some('h') if !s.at("ttp://") && !s.at("ttps://") => {}
                Some('@') if ref_name_len(s.after()) == 0 => {}
                _ => break,
            }

            self.s = s;
        }

        SyntaxKind::Text
    }

    fn in_word(&self) -> bool {
        let wordy = |c: Option<char>| {
            c.is_some_and(|c| {
                c.is_alphanumeric()
                    && !matches!(
                        c.script(),
                        Script::Han | Script::Hiragana | Script::Katakana | Script::Hangul
                    )
            })
        };
        let prev = self.s.scout(-2);
        let next = self.s.peek();
        wordy(prev) && wordy(next)
    }

    fn space_or_end(&self) -> bool {
        self.s.done() || self.s.at(char::is_whitespace) || self.s.at("//") || self.s.at("/*")
    }
}

/// Math.
impl Lexer<'_> {
    fn math(&mut self, start: usize, c: char) -> (SyntaxKind, Option<SyntaxNode>) {
        let kind = match c {
            '\\' => self.backslash(),
            '"' => self.string(),

            '<' if self.at_math_label() => self.label(),
            '-' if self.s.eat_if(">>") => SyntaxKind::MathShorthand,
            '-' if self.s.eat_if('>') => SyntaxKind::MathShorthand,
            '-' if self.s.eat_if("->") => SyntaxKind::MathShorthand,
            ':' if self.s.eat_if('=') => SyntaxKind::MathShorthand,
            ':' if self.s.eat_if(":=") => SyntaxKind::MathShorthand,
            '!' if self.s.eat_if('=') => SyntaxKind::MathShorthand,
            '.' if self.s.eat_if("..") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("==>") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("-->") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("--") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("-<") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("->") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("<-") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("<<") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("=>") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("==") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if("~~") => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if('=') => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if('<') => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if('-') => SyntaxKind::MathShorthand,
            '<' if self.s.eat_if('~') => SyntaxKind::MathShorthand,
            '>' if self.s.eat_if("->") => SyntaxKind::MathShorthand,
            '>' if self.s.eat_if(">>") => SyntaxKind::MathShorthand,
            '=' if self.s.eat_if("=>") => SyntaxKind::MathShorthand,
            '=' if self.s.eat_if('>') => SyntaxKind::MathShorthand,
            '=' if self.s.eat_if(':') => SyntaxKind::MathShorthand,
            '>' if self.s.eat_if('=') => SyntaxKind::MathShorthand,
            '>' if self.s.eat_if('>') => SyntaxKind::MathShorthand,
            '|' if self.s.eat_if("->") => SyntaxKind::MathShorthand,
            '|' if self.s.eat_if("=>") => SyntaxKind::MathShorthand,
            '|' if self.s.eat_if('|') => SyntaxKind::MathShorthand,
            '~' if self.s.eat_if("~>") => SyntaxKind::MathShorthand,
            '~' if self.s.eat_if('>') => SyntaxKind::MathShorthand,
            '*' | '-' | '~' => SyntaxKind::MathShorthand,

            '.' => SyntaxKind::Dot,
            ',' => SyntaxKind::Comma,
            ';' => SyntaxKind::Semicolon,

            '#' => SyntaxKind::Hash,
            '_' => SyntaxKind::Underscore,
            '$' => SyntaxKind::Dollar,
            '/' => SyntaxKind::Slash,
            '^' => SyntaxKind::Hat,
            '&' => SyntaxKind::MathAlignPoint,
            '√' | '∛' | '∜' => SyntaxKind::Root,
            '!' => SyntaxKind::Bang,

            '\'' => {
                self.s.eat_while('\'');
                SyntaxKind::MathPrimes
            }

            // We lex delimiters as `{Left,Right}{Brace,Paren}` and convert back
            // to `MathText` or `MathShorthand` in the parser.
            '(' => SyntaxKind::LeftParen,
            ')' => SyntaxKind::RightParen,
            // TODO: We may instead want to add `MathOpening` and `MathClosing`
            // kinds for these.
            '[' if self.s.eat_if('|') => SyntaxKind::LeftBrace,
            '|' if self.s.eat_if(']') => SyntaxKind::RightBrace,
            c if default_math_class(c) == Some(MathClass::Opening) => SyntaxKind::LeftBrace,
            c if default_math_class(c) == Some(MathClass::Closing) => SyntaxKind::RightBrace,

            // Identifiers.
            c if is_math_id_start(c) && self.s.at(is_math_id_continue) => {
                self.s.eat_while(is_math_id_continue);
                let (last_index, _) = self
                    .s
                    .from(start)
                    .grapheme_indices(true)
                    .next_back()
                    .unwrap();
                if last_index == 0 {
                    // If this was just a single grapheme.
                    SyntaxKind::MathText
                } else {
                    let (kind, node) = self.math_ident_or_field(start);
                    return (kind, Some(node));
                }
            }

            // Other math atoms.
            _ => self.math_text(start, c),
        };
        (kind, None)
    }

    /// Whether the `<` just eaten starts a label that ends a line of an
    /// equation: `<eq:suvat>` followed, after spaces, by a line break `\`,
    /// the closing `$` or the end of the text (SPEC §8.1). Anywhere else,
    /// `<` is less-than or starts a shorthand such as `<=`, so `a<b>c` is
    /// a comparison.
    fn at_math_label(&self) -> bool {
        let mut s = self.s;
        if !s.at(is_label_start) {
            return false;
        }
        s.eat_while(is_valid_in_label_literal);
        if !s.eat_if('>') {
            return false;
        }
        s.eat_while([' ', '\t']);
        if s.done() || s.at('$') {
            return true;
        }
        s.eat_if('\\') && (s.done() || s.at(char::is_whitespace))
    }

    /// Parse a single `MathIdent` or an entire `MathFieldAccess`.
    fn math_ident_or_field(&mut self, start: usize) -> (SyntaxKind, SyntaxNode) {
        let mut kind = SyntaxKind::MathIdent;
        let mut node = SyntaxNode::leaf(kind, self.s.from(start));
        while let Some(ident) = self.maybe_dot_ident() {
            kind = SyntaxKind::MathFieldAccess;
            let field_children = vec![
                node,
                SyntaxNode::leaf(SyntaxKind::Dot, '.'),
                SyntaxNode::leaf(SyntaxKind::MathIdent, ident),
            ];
            node = SyntaxNode::inner(kind, field_children);
        }
        (kind, node)
    }

    /// If at a dot and a math identifier, eat and return the identifier.
    fn maybe_dot_ident(&mut self) -> Option<&str> {
        if self.s.scout(1).is_some_and(is_math_id_start) && self.s.eat_if('.') {
            let ident_start = self.s.cursor();
            self.s.eat();
            self.s.eat_while(is_math_id_continue);
            Some(self.s.from(ident_start))
        } else {
            None
        }
    }

    fn math_text(&mut self, start: usize, c: char) -> SyntaxKind {
        // Keep numbers and grapheme clusters together.
        if c.is_numeric() {
            self.s.eat_while(char::is_numeric);
            let mut s = self.s;
            if s.eat_if('.') && !s.eat_while(char::is_numeric).is_empty() {
                self.s = s;
            }
        } else {
            let len = self
                .s
                .get(start..self.s.string().len())
                .graphemes(true)
                .next()
                .map_or(0, str::len);
            self.s.jump(start + len);
        }
        SyntaxKind::MathText
    }

    /// Handle named arguments in math function call.
    pub(crate) fn maybe_math_named_arg(&mut self, start: usize) -> Option<SyntaxNode> {
        let cursor = self.s.cursor();
        self.s.jump(start);
        if self.s.eat_if(is_id_start) {
            self.s.eat_while(is_id_continue);
            // Check that a colon directly follows the identifier, and not the
            // `:=` or `::=` math shorthands.
            if self.s.at(':') && !self.s.at(":=") && !self.s.at("::=") {
                // Check that the identifier is not just `_`.
                let node = if self.s.from(start) != "_" {
                    SyntaxNode::leaf(SyntaxKind::Ident, self.s.from(start))
                } else {
                    let message = "expected identifier, found underscore";
                    SyntaxNode::error(message, self.s.from(start))
                };
                return Some(node);
            }
        }
        self.s.jump(cursor);
        None
    }

    /// Handle spread arguments in math function call.
    pub(crate) fn maybe_math_spread_arg(&mut self, start: usize) -> Option<SyntaxNode> {
        let cursor = self.s.cursor();
        self.s.jump(start);
        if self.s.eat_if("..") {
            // We only infer a spread operator if it is not followed by:
            // - a space/trivia/end
            // - a dot (this would clash with the `...` math shorthand)
            // - an end of arg character: `,`, `;`, ')', `$` (spreads nothing)
            if !self.space_or_end() && !self.s.at(['.', ',', ';', ')', '$']) {
                let node = SyntaxNode::leaf(SyntaxKind::Dots, self.s.from(start));
                return Some(node);
            }
        }
        self.s.jump(cursor);
        None
    }
}

/// Code.
impl Lexer<'_> {
    fn code(&mut self, start: usize, c: char) -> SyntaxKind {
        match c {
            // Stylesheets write labels as id selectors (`#{fig:ball}`), so
            // `<` is always an operator there: `[marks<2]`.
            '<' if !self.style && self.s.at(is_label_start) => self.label(),
            '#' if self.style => self.color(),
            '$' if self.style && self.s.at(is_id_start) => {
                self.s.eat_while(is_id_continue);
                SyntaxKind::TokenName
            }
            '0'..='9' => self.number(start, c),
            '.' if self.s.at(char::is_ascii_digit) => self.number(start, c),
            '"' => self.string(),

            '=' if self.s.eat_if('=') => SyntaxKind::EqEq,
            '!' if self.s.eat_if('=') => SyntaxKind::ExclEq,
            '<' if self.s.eat_if('=') => SyntaxKind::LtEq,
            '>' if self.s.eat_if('=') => SyntaxKind::GtEq,
            '+' if self.s.eat_if('=') => SyntaxKind::PlusEq,
            '-' | '\u{2212}' if self.s.eat_if('=') => SyntaxKind::HyphEq,
            '*' if self.s.eat_if('=') => SyntaxKind::StarEq,
            '/' if self.s.eat_if('=') => SyntaxKind::SlashEq,
            '.' if self.s.eat_if('.') => SyntaxKind::Dots,
            '=' if self.s.eat_if('>') => SyntaxKind::Arrow,

            '{' => SyntaxKind::LeftBrace,
            '}' => SyntaxKind::RightBrace,
            '[' => SyntaxKind::LeftBracket,
            ']' => SyntaxKind::RightBracket,
            '(' => SyntaxKind::LeftParen,
            ')' => SyntaxKind::RightParen,
            '$' => SyntaxKind::Dollar,
            ',' => SyntaxKind::Comma,
            ';' => SyntaxKind::Semicolon,
            ':' => SyntaxKind::Colon,
            '.' => SyntaxKind::Dot,
            '+' => SyntaxKind::Plus,
            '-' | '\u{2212}' => SyntaxKind::Minus,
            '*' => SyntaxKind::Star,
            '/' => SyntaxKind::Slash,
            '=' => SyntaxKind::Eq,
            '<' => SyntaxKind::Lt,
            '>' => SyntaxKind::Gt,
            '@' => SyntaxKind::At,

            c if is_id_start(c) => self.ident(start),

            c => self.invalid_char_in_code(c),
        }
    }

    /// Error for an invalid character in code, but try to give good hints for
    /// commonly confusing operators.
    fn invalid_char_in_code(&mut self, c: char) -> SyntaxKind {
        let invalid_char = || eco_format!("the character `{c}` is not valid in code");
        let invalid_str = |s: &str| eco_format!("`{s}` is not valid in code");
        match c {
            // Give a custom hint if we immediately follow a hash.
            _ if self.s.scout(-2) == Some('#') => {
                self.error(invalid_char());
                // This is only an accurate hint if we just came from markup or
                // math, but `#!` or `##` in code should be rare enough that
                // it's fine (and the first hash will produce its own error).
                self.hint("the preceding hash is causing this to parse in code mode");
                self.hint("try escaping the preceding hash: `\\#`");
                // The span for these hints isn't great, but it's hard to fix.
            }
            '#' => {
                self.error(invalid_char());
                self.hint("you are already in code mode");
                self.hint("try removing the `#`");
            }
            '&' if self.s.eat_if('&') => {
                self.error(invalid_str("&&"));
                self.hint("in Folio, `and` is used for logical AND");
            }
            '|' if self.s.eat_if('|') => {
                self.error(invalid_str("||"));
                self.hint("in Folio, `or` is used for logical OR");
            }
            '!' => {
                self.error(invalid_char());
                self.hint("in Folio, `not` is used for negation");
                self.hint("or did you mean to write `!=` for not-equal?");
            }
            '~' if self.s.eat_if('=') => {
                self.error(invalid_str("~="));
                self.hint("in Folio, `!=` is used for not-equal");
            }
            _ => {
                self.error(invalid_char());
            }
        }
        SyntaxKind::Error
    }

    fn ident(&mut self, start: usize) -> SyntaxKind {
        self.s.eat_while(is_id_continue);
        let ident = self.s.from(start);

        let prev = self.s.get(0..start);
        if (!prev.ends_with(['.', '@']) || prev.ends_with(".."))
            && let Some(keyword) = keyword(ident)
        {
            return keyword;
        }

        if ident == "_" {
            SyntaxKind::Underscore
        } else {
            SyntaxKind::Ident
        }
    }

    /// Lex a single number, either an integer or a float, possibly with a
    /// numeric suffix (`pt`, `deg`, `%`, etc.). Integers may also have a prefix
    /// for binary, octal, or hexadecimal bases, but only base-10 integers can
    /// have a numeric suffix (if so, they will be treated as floats in the
    /// AST).
    ///
    /// Floating point numbers can use exponent notation with `e` or `E`, such
    /// as `5.0e-3in` for `0.005in`.
    fn number(&mut self, start: usize, first_c: char) -> SyntaxKind {
        // Handle alternative integer bases.
        let base = match first_c {
            '0' if self.s.eat_if('b') => 2,
            '0' if self.s.eat_if('o') => 8,
            '0' if self.s.eat_if('x') => 16,
            _ => 10,
        };

        // Read the initial digits.
        if base == 16 {
            self.s.eat_while(char::is_ascii_alphanumeric);
        } else {
            self.s.eat_while(char::is_ascii_digit);
        }

        // Read floating point digits and exponents.
        let mut is_float = false;
        if base == 10 {
            // Read digits following a dot. Make sure not to confuse a spread
            // operator or a method call for the decimal separator.
            if first_c == '.' {
                is_float = true; // We already ate the trailing digits above.
            } else if !self.s.at("..")
                && !self.s.scout(1).is_some_and(is_id_start)
                && self.s.eat_if('.')
            {
                is_float = true;
                self.s.eat_while(char::is_ascii_digit);
            }

            // Read the exponent.
            if !self.s.at("em") && self.s.eat_if(['e', 'E']) {
                is_float = true;
                self.s.eat_if(['+', '-']);
                self.s.eat_while(char::is_ascii_digit);
            }
        }

        // A ratio in a stylesheet: `16:9`, `2.39:1`.
        if self.style
            && base == 10
            && self.s.at(':')
            && self.s.scout(1).is_some_and(|c| c.is_ascii_digit())
        {
            self.s.eat_if(':');
            self.s.eat_while(char::is_ascii_digit);
            if self.s.at('.') && self.s.scout(1).is_some_and(|c| c.is_ascii_digit()) {
                self.s.eat_if('.');
                self.s.eat_while(char::is_ascii_digit);
            }
            let suffix = self
                .s
                .eat_while(|c: char| c.is_ascii_alphanumeric() || c == '%');
            if !suffix.is_empty() {
                return self.error(eco_format!("a ratio cannot have a suffix: `{suffix}`"));
            }
            return SyntaxKind::Ratio;
        }

        let number = self.s.from(start);
        let suffix = self
            .s
            .eat_while(|c: char| c.is_ascii_alphanumeric() || c == '%');

        // Parse large integer literals as floats
        if base == 10
            && !is_float
            && let Err(e) = i64::from_str_radix(number, base)
            && matches!(
                e.kind(),
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow
            )
            && number.parse::<f64>().is_ok()
        {
            is_float = true;
        }

        let mut suffix_result = match suffix {
            "" => Ok(None),
            "pt" | "mm" | "cm" | "in" | "deg" | "rad" | "em" | "fr" | "%" | "ms" | "s" | "min" => {
                Ok(Some(()))
            }
            _ => Err(eco_format!("invalid number suffix: `{suffix}`")),
        };

        let number_result = if is_float && number.parse::<f64>().is_err() {
            // The only invalid case should be when a float lacks digits after
            // the exponent: e.g. `1.2e`, `2.3E-`, or `1EM`.
            Err(eco_format!("invalid floating point number: `{number}`"))
        } else if base == 10 {
            Ok(())
        } else {
            let name = match base {
                2 => "binary",
                8 => "octal",
                16 => "hexadecimal",
                _ => unreachable!(),
            };
            // The index `[2..]` skips the leading `0b`/`0o`/`0x`.
            match i64::from_str_radix(&number[2..], base) {
                Ok(_) if suffix.is_empty() => Ok(()),
                Ok(value) => {
                    if suffix_result.is_ok() {
                        suffix_result =
                            Err(eco_format!("try using a decimal number: `{value}{suffix}`"));
                    }
                    Err(eco_format!("{name} numbers cannot have a suffix"))
                }
                Err(e) if *e.kind() == IntErrorKind::Empty => Err(eco_format!(
                    "expected a{} {name} number",
                    if base == 8 { "n" } else { "" },
                )),
                Err(_) => Err(eco_format!("invalid {name} number: `{number}`")),
            }
        };

        // Return our number or write an error with helpful hints.
        match (number_result, suffix_result) {
            // Valid numbers :D
            (Ok(()), Ok(None)) if is_float => SyntaxKind::Float,
            (Ok(()), Ok(None)) => SyntaxKind::Int,
            (Ok(()), Ok(Some(()))) => SyntaxKind::Numeric,
            // Invalid numbers :(
            (Err(number_err), Err(suffix_err)) => {
                let error = self.error(number_err);
                self.hint(suffix_err);
                error
            }
            (Ok(()), Err(msg)) | (Err(msg), Ok(_)) => self.error(msg),
        }
    }

    /// Lex a colour in a stylesheet after its `#`: `#1f5fa8`, `#fff`, or
    /// with alpha `#1f5fa880`, `#fff8`.
    fn color(&mut self) -> SyntaxKind {
        let digits = self.s.eat_while(|c: char| c.is_ascii_alphanumeric());
        if digits.is_empty() {
            let error = self.error("expected a colour or an id selector after `#`");
            self.hint("a colour is written `#1f5fa8` and an id selector `#{fig:ball}`");
            return error;
        }
        if !digits.chars().all(|c| c.is_ascii_hexdigit()) || ![3, 4, 6, 8].contains(&digits.len()) {
            let error = self.error(eco_format!("invalid colour: `#{digits}`"));
            self.hint("a colour has 3, 4, 6 or 8 hexadecimal digits: `#1f5fa8`");
            return error;
        }
        SyntaxKind::Color
    }

    /// Lex an id selector after its `#`, which is followed by `{`:
    /// `#{fig:ball}`. Braces are needed because labels may contain `:`.
    ///
    /// The parser also calls this for scoped rules in content, where `#` is
    /// not otherwise a token in code.
    pub(crate) fn id_selector(&mut self, start: usize) -> (SyntaxKind, SyntaxNode) {
        let hash = SyntaxNode::leaf(SyntaxKind::Hash, self.s.from(start));
        let brace_start = self.s.cursor();
        self.s.eat_if('{');
        let brace = self.s.from(brace_start);

        let name_start = self.s.cursor();
        self.s.eat_while(is_valid_in_label_literal);
        // Take everything up to a closing brace on the same line into the
        // name, so that a broken label does not unbalance the braces.
        let mut s = self.s;
        s.eat_until(|c: char| c == '}' || c == '{' || c == ';' || is_newline(c));
        if s.at('}') {
            self.s = s;
        }
        let name = self.s.from(name_start);

        let mut nodes = vec![hash];
        let closed = self.s.at('}');
        nodes.push(if closed {
            SyntaxNode::leaf(SyntaxKind::LeftBrace, brace)
        } else {
            SyntaxNode::error("unclosed delimiter", brace)
        });
        if name.is_empty() {
            if closed {
                nodes.push(SyntaxNode::error("expected label", ""));
            }
        } else if is_valid_label_literal_id(name) {
            nodes.push(SyntaxNode::leaf(SyntaxKind::LabelName, name));
        } else {
            let message = if name.contains('.') {
                "labels cannot contain `.`".into()
            } else if let Some(last @ (':' | '-')) = name.chars().next_back() {
                eco_format!("labels cannot end with `{last}`")
            } else {
                eco_format!("invalid label: `{name}`")
            };
            nodes.push(
                SyntaxNode::error(message, name)
                    .with_hints(["a label consists of letters, digits, `-`, `_` and `:`".into()]),
            );
        }
        if self.s.eat_if('}') {
            nodes.push(SyntaxNode::leaf(SyntaxKind::RightBrace, "}"));
        }

        (
            SyntaxKind::IdSelector,
            SyntaxNode::inner(SyntaxKind::IdSelector, nodes),
        )
    }

    fn string(&mut self) -> SyntaxKind {
        let mut escaped = false;
        self.s.eat_until(|c| {
            let stop = c == '"' && !escaped;
            escaped = c == '\\' && !escaped;
            stop
        });

        if !self.s.eat_if('"') {
            return self.error("unclosed string");
        }

        SyntaxKind::Str
    }
}

/// Try to parse an identifier into a keyword.
fn keyword(ident: &str) -> Option<SyntaxKind> {
    Some(match ident {
        "none" => SyntaxKind::None,
        "auto" => SyntaxKind::Auto,
        "true" => SyntaxKind::Bool,
        "false" => SyntaxKind::Bool,
        "not" => SyntaxKind::Not,
        "and" => SyntaxKind::And,
        "or" => SyntaxKind::Or,
        "let" => SyntaxKind::Let,
        "if" => SyntaxKind::If,
        "else" => SyntaxKind::Else,
        "for" => SyntaxKind::For,
        "in" => SyntaxKind::In,
        "while" => SyntaxKind::While,
        "break" => SyntaxKind::Break,
        "continue" => SyntaxKind::Continue,
        "return" => SyntaxKind::Return,
        "import" => SyntaxKind::Import,
        "include" => SyntaxKind::Include,
        "as" => SyntaxKind::As,
        _ => return None,
    })
}

trait ScannerExt {
    fn advance(&mut self, by: usize);
    fn eat_newline(&mut self) -> bool;
}

impl ScannerExt for Scanner<'_> {
    fn advance(&mut self, by: usize) {
        self.jump(self.cursor() + by);
    }

    fn eat_newline(&mut self) -> bool {
        let ate = self.eat_if(is_newline);
        if ate && self.before().ends_with('\r') {
            self.eat_if('\n');
        }
        ate
    }
}

/// Whether a character will become a [`SyntaxKind::Space`] token.
#[inline]
fn is_space(character: char, mode: SyntaxMode) -> bool {
    match mode {
        SyntaxMode::Markup => matches!(character, ' ' | '\t') || is_newline(character),
        _ => character.is_whitespace(),
    }
}

/// Whether a character is interpreted as a newline by Folio.
#[inline]
pub fn is_newline(character: char) -> bool {
    matches!(
        character,
        // Line Feed, Vertical Tab, Form Feed, Carriage Return.
        '\n' | '\x0B' | '\x0C' | '\r' |
        // Next Line, Line Separator, Paragraph Separator.
        '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// Extracts a prefix of the text that is a link and also returns whether the
/// parentheses and brackets in the link were balanced.
pub fn link_prefix(text: &str) -> (&str, bool) {
    let mut s = Scanner::new(text);
    let mut brackets = Vec::new();

    #[rustfmt::skip]
    s.eat_while(|c: char| {
        match c {
            | '0' ..= '9'
            | 'a' ..= 'z'
            | 'A' ..= 'Z'
            | '!' | '#' | '$' | '%' | '&' | '*' | '+'
            | ',' | '-' | '.' | '/' | ':' | ';' | '='
            | '?' | '@' | '_' | '~' | '\'' => true,
            '[' => {
                brackets.push(b'[');
                true
            }
            '(' => {
                brackets.push(b'(');
                true
            }
            ']' => brackets.pop() == Some(b'['),
            ')' => brackets.pop() == Some(b'('),
            _ => false,
        }
    });

    // Don't include the trailing characters likely to be part of text.
    while matches!(s.scout(-1), Some('!' | ',' | '.' | ':' | ';' | '?' | '\'')) {
        s.uneat();
    }

    (s.before(), brackets.is_empty())
}

/// Split text at newlines. These newline characters are not kept.
pub fn split_newlines(text: &str) -> Vec<&str> {
    let mut s = Scanner::new(text);
    let mut lines = Vec::new();
    let mut start = 0;
    let mut end = 0;

    while let Some(c) = s.eat() {
        if is_newline(c) {
            if c == '\r' {
                s.eat_if('\n');
            }

            lines.push(&text[start..end]);
            start = s.cursor();
        }
        end = s.cursor();
    }

    lines.push(&text[start..]);
    lines
}

/// Count the number of newlines in text.
fn count_newlines(text: &str) -> usize {
    let mut newlines = 0;
    let mut s = Scanner::new(text);
    while let Some(c) = s.eat() {
        if is_newline(c) {
            if c == '\r' {
                s.eat_if('\n');
            }
            newlines += 1;
        }
    }
    newlines
}

/// Whether a string is a valid Folio identifier.
///
/// In addition to what is specified in the [Unicode Standard][uax31], we allow:
/// - `_` as a starting character,
/// - `_` and `-` as continuing characters.
///
/// [uax31]: http://www.unicode.org/reports/tr31/
#[inline]
pub fn is_ident(string: &str) -> bool {
    let mut chars = string.chars();
    chars
        .next()
        .is_some_and(|c| is_id_start(c) && chars.all(is_id_continue))
}

/// Whether a character can start an identifier.
#[inline]
pub fn is_id_start(c: char) -> bool {
    is_xid_start(c) || c == '_'
}

/// Whether a character can continue an identifier.
#[inline]
pub fn is_id_continue(c: char) -> bool {
    is_xid_continue(c) || c == '_' || c == '-'
}

/// Whether a character can start an identifier in math.
#[inline]
fn is_math_id_start(c: char) -> bool {
    is_xid_start(c)
}

/// Whether a character can continue an identifier in math.
#[inline]
fn is_math_id_continue(c: char) -> bool {
    is_xid_continue(c) && c != '_'
}

/// Whether a character can start a label's name: a letter, a digit or `_`.
#[inline]
fn is_label_start(c: char) -> bool {
    is_xid_continue(c) || c == '_'
}

/// Whether a character can be part of a label's name: a letter, a digit, `-`,
/// `_` or `:`. A dot is not, because it separates a label from a published
/// value (`<timer:task>.remaining`).
#[inline]
fn is_valid_in_label_literal(c: char) -> bool {
    is_id_continue(c) || c == ':'
}

/// The length of the reference name at the start of `text`, which follows an
/// `@`, or zero if there is none. Trailing `:` and `-` are punctuation, since
/// a label never ends with them.
fn ref_name_len(text: &str) -> usize {
    if !text.starts_with(is_label_start) {
        return 0;
    }
    let end = text
        .find(|c| !is_valid_in_label_literal(c))
        .unwrap_or(text.len());
    text[..end].trim_end_matches([':', '-']).len()
}

/// Whether a string is a valid label name (without the angle brackets).
///
/// A label name consists of letters, digits, `-`, `_` and `:`; it starts with
/// a letter, a digit or `_`, and does not end with `:` or `-` (SPEC §4.2).
pub fn is_valid_label_literal_id(id: &str) -> bool {
    id.starts_with(is_label_start)
        && id.chars().all(is_valid_in_label_literal)
        && !id.ends_with([':', '-'])
}
