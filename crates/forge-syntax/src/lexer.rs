// crates/forge-syntax/src/lexer.rs

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut tokens = Vec::new();
    let mut diags = Vec::new();

    while i < bytes.len() {
        let start = i;
        // `i` is always a valid UTF-8 boundary (see advancement below), so
        // decoding the real char here — rather than casting a raw byte —
        // avoids slicing mid-character on multi-byte input.
        let c = src[i..].chars().next().unwrap();
        let c_len = c.len_utf8();

        if c.is_whitespace() {
            i += c_len;
            continue;
        }

        if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
            let mut is_float = false;
            if i < bytes.len()
                && bytes[i] == b'.'
                && i + 1 < bytes.len()
                && bytes[i + 1].is_ascii_digit()
            {
                is_float = true;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                    i += 1;
                }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                let save = i;
                let mut j = i + 1;
                if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    is_float = true;
                    i = j;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let text: String = src[start..i].chars().filter(|&c| c != '_').collect();
            tokens.push(Token {
                kind: if is_float {
                    TokenKind::Float
                } else {
                    TokenKind::Int
                },
                span: Span::new(start as u32, i as u32),
                text,
            });
            continue;
        }

        if c.is_ascii_alphabetic() || c == '_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let text = &src[start..i];
            let kind = match text {
                "if" => TokenKind::If,
                "then" => TokenKind::Then,
                "else" => TokenKind::Else,
                "let" => TokenKind::Let,
                "in" => TokenKind::In,
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                _ => TokenKind::Ident,
            };
            tokens.push(Token {
                kind,
                span: Span::new(start as u32, i as u32),
                text: text.to_string(),
            });
            continue;
        }

        // Only ASCII bytes participate in two-char operators, but guard the
        // slice with `is_char_boundary` regardless so a multi-byte char
        // sitting right after (or straddling) this offset can never cause
        // an out-of-boundary panic here.
        if i + 2 <= bytes.len() && src.is_char_boundary(i + 2) {
            let two = &src[i..i + 2];
            let two_kind = match two {
                "==" => Some(TokenKind::EqEq),
                "!=" => Some(TokenKind::NotEq),
                "<=" => Some(TokenKind::Le),
                ">=" => Some(TokenKind::Ge),
                "&&" => Some(TokenKind::AndAnd),
                "||" => Some(TokenKind::OrOr),
                "<<" => Some(TokenKind::Shl),
                ">>" => Some(TokenKind::Shr),
                _ => None,
            };
            if let Some(kind) = two_kind {
                tokens.push(Token {
                    kind,
                    span: Span::new(start as u32, (start + 2) as u32),
                    text: String::new(),
                });
                i += 2;
                continue;
            }
        }

        let kind = match c {
            '(' => Some(TokenKind::LParen),
            ')' => Some(TokenKind::RParen),
            ',' => Some(TokenKind::Comma),
            '@' => Some(TokenKind::At),
            '=' => Some(TokenKind::Assign),
            '|' => Some(TokenKind::Pipe),
            '^' => Some(TokenKind::Caret),
            '&' => Some(TokenKind::Amp),
            '<' => Some(TokenKind::Lt),
            '>' => Some(TokenKind::Gt),
            '+' => Some(TokenKind::Plus),
            '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star),
            '/' => Some(TokenKind::Slash),
            '%' => Some(TokenKind::Percent),
            '!' => Some(TokenKind::Bang),
            '~' => Some(TokenKind::Tilde),
            _ => None,
        };
        match kind {
            Some(k) => {
                tokens.push(Token {
                    kind: k,
                    span: Span::new(start as u32, (start + c_len) as u32),
                    text: String::new(),
                });
                i += c_len;
            }
            None => {
                diags.push(Diagnostic::error(
                    format!("unexpected character '{c}'"),
                    Span::new(start as u32, (start + c_len) as u32),
                    "not a valid token",
                ));
                i += c_len;
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::new(bytes.len() as u32, bytes.len() as u32),
        text: String::new(),
    });
    (tokens, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (tokens, diags) = lex(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        tokens.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn numbers() {
        let (tokens, diags) = lex("3.14159 42 1_000 6.02e23 1e-9");
        assert!(diags.is_empty());
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(tokens[0].kind, TokenKind::Float);
        assert_eq!(texts[0], "3.14159");
        assert_eq!(tokens[1].kind, TokenKind::Int);
        assert_eq!(texts[1], "42");
        assert_eq!(tokens[2].kind, TokenKind::Int);
        assert_eq!(texts[2], "1000");
        assert_eq!(tokens[3].kind, TokenKind::Float);
        assert_eq!(texts[3], "6.02e23");
        assert_eq!(tokens[4].kind, TokenKind::Float);
        assert_eq!(texts[4], "1e-9");
    }

    #[test]
    fn keywords_and_idents() {
        assert_eq!(
            kinds("if then else let in x true false"),
            vec![
                TokenKind::If,
                TokenKind::Then,
                TokenKind::Else,
                TokenKind::Let,
                TokenKind::In,
                TokenKind::Ident,
                TokenKind::True,
                TokenKind::False,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn multi_char_operators_before_single_char() {
        assert_eq!(
            kinds("== != <= >= && || << >> ="),
            vec![
                TokenKind::EqEq,
                TokenKind::NotEq,
                TokenKind::Le,
                TokenKind::Ge,
                TokenKind::AndAnd,
                TokenKind::OrOr,
                TokenKind::Shl,
                TokenKind::Shr,
                TokenKind::Assign,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn bitwise_and_shift_tokens() {
        assert_eq!(
            kinds("& | ^ ~"),
            vec![
                TokenKind::Amp,
                TokenKind::Pipe,
                TokenKind::Caret,
                TokenKind::Tilde,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn unknown_char_produces_diagnostic_not_panic() {
        let (tokens, diags) = lex("1 $ 2");
        assert_eq!(diags.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert_eq!(tokens[1].kind, TokenKind::Int); // lexer skips the bad char and continues
    }

    #[test]
    fn multibyte_unknown_char_does_not_panic() {
        // '€' is 3 bytes in UTF-8 (0xE2 0x82 0xAC); previously the lexer
        // cast raw bytes to `char` and sliced `&src[i..i+2]` for two-char
        // operator lookahead, which panicked when `i` landed mid-character.
        let (tokens, diags) = lex("1 € 2");
        assert_eq!(diags.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Int);
        assert_eq!(tokens[1].kind, TokenKind::Int); // lexer skips the bad char and continues
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }
}
