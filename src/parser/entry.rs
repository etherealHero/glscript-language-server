mod grammar {
    use faster_pest::*;

    #[derive(Parser)]
    #[grammar = "src/parser/glscript_subset_grammar.pest"]
    pub struct GlScriptSubsetGrammar;
}

pub use grammar::{GlScriptSubsetGrammar, Ident, Rule};

#[allow(unused)]
pub type Pair<'a> = faster_pest::Pair2<'a, Ident<'a>>;
pub type Pairs<'a> = faster_pest::Pairs2<'a, Ident<'a>>;

pub fn get_pairs<'a>(raw_text: &'a str) -> Pairs<'a> {
    let pairs = parse_raw_text(Rule::SourceFileFast, raw_text);
    let (mut pos, mut ok) = (0, true);

    for p in pairs.clone() {
        let s = p.as_span().as_str();
        let end = pos + s.len();

        if raw_text.get(pos..end) != Some(s) {
            ok = false;
            break;
        }

        pos = end;
    }

    match ok && pos == raw_text.len() {
        true => pairs,
        false => parse_raw_text(Rule::SourceFile, raw_text), // fallback
    }
}

fn parse_raw_text(entry_rule: Rule, raw_text: &str) -> Pairs<'_> {
    GlScriptSubsetGrammar::parse(entry_rule, raw_text)
        .unwrap()
        .next()
        .unwrap()
        .into_inner()
}
