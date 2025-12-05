use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "./glscript_subset_grammar.pest"]
struct GlScriptSubsetGrammar;

#[derive(Clone)]
pub enum TokenRule {
    IncludeToken,
    IncludePath,
    LineTerminator,
    EndOfInput,
    Untracked,
}

#[derive(Clone)]
pub struct Token {
    pub rule: TokenRule,
    pub text: String,
    pub line: u32,
    pub col: u32,
}

impl Token {
    fn new(pair: pest::iterators::Pair<'_, Rule>) -> Self {
        let rule = match pair.as_rule() {
            Rule::IncludeToken => TokenRule::IncludeToken,
            Rule::IncludePath => TokenRule::IncludePath,
            Rule::LineTerminator => TokenRule::LineTerminator,
            Rule::EOI => TokenRule::EndOfInput,
            _ => TokenRule::Untracked,
        };
        let sp = pair.as_span();
        let text = sp.as_str().to_string();
        let line_col = sp.start_pos().line_col();
        let (line, col) = (line_col.0 as u32 - 1, line_col.1 as u32 - 1);

        Self {
            rule,
            line,
            col,
            text,
        }
    }
}

pub fn parse(raw_text: &str) -> Vec<Token> {
    GlScriptSubsetGrammar::parse(self::Rule::SourceFile, &raw_text)
        .expect("parsed sourceFile")
        .next()
        .expect("sourceFile entry rule")
        .into_inner()
        .map(Token::new)
        .collect()
}
