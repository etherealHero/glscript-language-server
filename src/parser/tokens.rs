mod grammar {
    use faster_pest::*;

    #[derive(Parser)]
    #[grammar = "src/parser/glscript_subset_grammar.pest"]
    pub struct GlScriptSubsetGrammar;
}

use derive_more::{Constructor, From};
pub use grammar::{GlScriptSubsetGrammar, Ident, Rule};

#[allow(unused)]
pub type Pair<'a> = faster_pest::Pair2<'a, Ident<'a>>;
pub type Pairs<'a> = faster_pest::Pairs2<'a, Ident<'a>>;

#[derive(Debug)]
pub enum Token<'a> {
    Include(Span),
    IncludePath(PathLiteral<'a>),
    RegionOpen(Span),
    RegionClose(Span),
    LineTerminator(LineCol),
    Common(RawToken<'a>),
    CommonWithLineEnding(RawToken<'a>),
    Eoi(LineCol),
}

#[derive(Debug, Constructor)]
pub struct RawToken<'a> {
    pub line_col: LineCol,
    pub text: &'a str,
}

#[derive(Debug, Constructor)]
pub struct PathLiteral<'a> {
    pub line_col: LineCol,
    pub path: &'a str,
}

#[derive(Debug, Constructor)]
pub struct Span {
    pub line_col: LineCol,
    pub len: u32,
}

#[derive(Debug, From)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

#[derive(Constructor)]
pub struct Pending {
    pub init_line_col: LineCol,
    init_pos: usize,
    pub pending_len: u32,
    pub has_linebreak: bool,
}

impl Pending {
    pub fn flush<'a>(self, raw_text: &'a str) -> Token<'a> {
        let pending_range = self.init_pos..self.init_pos + self.pending_len as usize;
        let text = unsafe { raw_text.get_unchecked(pending_range) };
        let token = RawToken::new(self.init_line_col, text);
        match self.has_linebreak {
            true => Token::CommonWithLineEnding(token),
            false => Token::Common(token),
        }
    }
}
