use tantivy::tokenizer::{AsciiFoldingFilter, TextAnalyzer, Token, TokenStream, Tokenizer};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy)]
pub(crate) struct MultilingualTokenizer {
    ngrams: bool,
}

impl MultilingualTokenizer {
    pub(crate) const fn query() -> Self {
        Self { ngrams: false }
    }

    pub(crate) const fn index() -> Self {
        Self { ngrams: true }
    }
}

pub(crate) struct MultilingualTokenStream {
    tokens: std::vec::IntoIter<Token>,
    current: Token,
}

impl Tokenizer for MultilingualTokenizer {
    type TokenStream<'a> = MultilingualTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let normalized = text.nfkc().collect::<String>();
        let base = base_tokens(&normalized);
        let tokens = if self.ngrams { ngrams(base) } else { base };
        MultilingualTokenStream {
            tokens: tokens.into_iter(),
            current: Token::default(),
        }
    }
}

impl TokenStream for MultilingualTokenStream {
    fn advance(&mut self) -> bool {
        let Some(token) = self.tokens.next() else {
            return false;
        };
        self.current = token;
        true
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

pub(crate) fn query_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(MultilingualTokenizer::query())
        .filter(AsciiFoldingFilter)
        .build()
}

pub(crate) fn index_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(MultilingualTokenizer::index())
        .filter(AsciiFoldingFilter)
        .build()
}

#[cfg(test)]
pub(crate) fn analyze(mut analyzer: TextAnalyzer, text: &str) -> Vec<String> {
    let mut stream = analyzer.token_stream(text);
    let mut tokens = Vec::new();
    stream.process(&mut |token| tokens.push(token.text.clone()));
    tokens
}

fn base_tokens(text: &str) -> Vec<Token> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Class {
        Cjk,
        Alphanumeric,
    }

    let mut spans = Vec::<(usize, usize, Class)>::new();
    let mut current: Option<(usize, usize, Class)> = None;
    for (offset, character) in text.char_indices() {
        let class = if is_cjk(character) {
            Some(Class::Cjk)
        } else if character.is_alphanumeric() {
            Some(Class::Alphanumeric)
        } else {
            None
        };
        let end = offset + character.len_utf8();
        match (current, class) {
            (Some((start, _, existing)), Some(next)) if existing == next => {
                current = Some((start, end, existing));
            }
            (Some(span), Some(next)) => {
                spans.push(span);
                current = Some((offset, end, next));
            }
            (Some(span), None) => {
                spans.push(span);
                current = None;
            }
            (None, Some(next)) => current = Some((offset, end, next)),
            (None, None) => {}
        }
    }
    if let Some(span) = current {
        spans.push(span);
    }

    let mut tokens = Vec::new();
    for (start, end, class) in spans {
        let value = &text[start..end];
        if class == Class::Cjk {
            let boundaries = value
                .char_indices()
                .map(|(offset, _)| start + offset)
                .chain(std::iter::once(end))
                .collect::<Vec<_>>();
            if boundaries.len() == 2 {
                push_token(&mut tokens, text, start, end);
            } else {
                for pair in boundaries.windows(3) {
                    push_token(&mut tokens, text, pair[0], pair[2]);
                }
            }
        } else {
            push_token(&mut tokens, text, start, end);
        }
    }
    for (position, token) in tokens.iter_mut().enumerate() {
        token.position = position;
    }
    tokens
}

fn push_token(tokens: &mut Vec<Token>, source: &str, start: usize, end: usize) {
    tokens.push(Token {
        offset_from: start,
        offset_to: end,
        position: 0,
        text: source[start..end].chars().flat_map(char::to_lowercase).collect(),
        position_length: 1,
    });
}

fn ngrams(tokens: Vec<Token>) -> Vec<Token> {
    let mut output = Vec::new();
    for token in tokens {
        let boundaries = token
            .text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain(std::iter::once(token.text.len()))
            .collect::<Vec<_>>();
        let character_count = boundaries.len() - 1;
        if character_count < 3 {
            output.push(token);
            continue;
        }
        let mut emitted_original = false;
        for start in 0..character_count {
            for size in 3..=10.min(character_count - start) {
                let text = token.text[boundaries[start]..boundaries[start + size]].to_owned();
                emitted_original |= start == 0 && size == character_count;
                output.push(Token {
                    offset_from: token.offset_from,
                    offset_to: token.offset_to,
                    position: token.position,
                    text,
                    position_length: token.position_length,
                });
            }
        }
        if !emitted_original {
            output.push(token);
        }
    }
    output
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
            | 0x3040..=0x309F
            | 0x30A0..=0x30FF
            | 0x31F0..=0x31FF
            | 0xAC00..=0xD7AF
            | 0x1100..=0x11FF
            | 0x3130..=0x318F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_analyzer_matches_kotlin_fixtures() {
        let cases = [
            (
                "The incredible adventures of Batman, the man who is also a bat!",
                vec![
                    "the",
                    "incredible",
                    "adventures",
                    "of",
                    "batman",
                    "the",
                    "man",
                    "who",
                    "is",
                    "also",
                    "a",
                    "bat",
                ],
            ),
            ("Eric", vec!["eric"]),
            ("Éric èl rojo", vec!["eric", "el", "rojo"]),
            ("Ｖｏｌ．０４", vec!["vol", "04"]),
            ("9782413016878", vec!["9782413016878"]),
            ("J", vec!["j"]),
            (
                "[不道德公會][河添太一 ][東立]Vol.04-搬运",
                vec![
                    "不道", "道德", "德公", "公會", "河添", "添太", "太一", "東立", "vol", "04", "搬运",
                ],
            ),
            (
                "不道德公會河添太一東立搬运",
                vec![
                    "不道", "道德", "德公", "公會", "會河", "河添", "添太", "太一", "一東", "東立", "立搬", "搬运",
                ],
            ),
            (
                "探偵はもう、死んでいる。",
                vec!["探偵", "偵は", "はも", "もう", "死ん", "んで", "でい", "いる"],
            ),
            ("ワンパンマン", vec!["ワン", "ンパ", "パン", "ンマ", "マン"]),
            (
                "고교생을 환불해 주세요",
                vec!["고교", "교생", "생을", "환불", "불해", "주세", "세요"],
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(analyze(query_analyzer(), input), expected, "input: {input}");
        }
    }

    #[test]
    fn index_analyzer_preserves_short_and_long_original_tokens() {
        assert_eq!(analyze(index_analyzer(), "J"), vec!["j"]);
        assert_eq!(analyze(index_analyzer(), "東京"), vec!["東京"]);
        let tokens = analyze(index_analyzer(), "Batman");
        assert!(tokens.iter().any(|token| token == "bat"));
        assert!(tokens.iter().any(|token| token == "atman"));
        assert!(tokens.iter().any(|token| token == "batman"));
        let isbn = analyze(index_analyzer(), "9782413016878");
        assert!(isbn.iter().any(|token| token == "9782413016878"));
        assert!(isbn.iter().any(|token| token == "241301"));
    }
}
