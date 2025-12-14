use faster_pest::*;

// TODO: benchmark with multiline literals without linebreak + post parsing split by linebreak
#[derive(Parser)]
#[grammar = "src/glscript_subset_grammar.pest"]
struct GlScriptSubsetGrammar;

// TODO: wrap with enum variants
// TODO: emit buld hash to doc hash (on parse) - CHECK bench without hasher on emit
#[derive(Clone, Debug)]
pub struct Token {
    pub rule: self::Rule,
    pub text: String,
    pub line: u32,
    pub col: u32,
}

impl Token {
    fn new(pair: faster_pest::Pair2<'_, Ident>) -> Self {
        let rule = pair.as_rule();
        let sp = pair.as_span();
        let text = sp.as_str().to_owned();
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

#[tracing::instrument(skip(raw_text))]
pub fn parse(raw_text: &str) -> Vec<Token> {
    GlScriptSubsetGrammar::parse(self::Rule::SourceFile, raw_text)
        .expect("sourceFile entry rule")
        .next()
        .expect("sourceFile contents")
        .into_inner()
        .map(Token::new)
        .collect()
}
