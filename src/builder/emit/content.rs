use async_lsp::lsp_types::Url as Uri;

use crate::builder::emit::{Context, Emit};
use crate::parser::Token;
use crate::types::{DocumentLinkStatement, SourceHash};

#[derive(Default)]
pub struct PatternMatched {
    literal: bool,
    source: bool,
}

/// Destination content
impl Emit {
    pub fn content(st: &mut Emit, ctx: &mut Context, target: &Uri) -> Option<PatternMatched> {
        if target == ctx.defult_document {
            ctx.is_default_context = true;
        }

        let Ok(d) = ctx.proxy_state.get_doc(target) else {
            return None;
        };

        let mut matched = ctx.pat.as_ref().map(|p| PatternMatched {
            source: p.source == d.source_hash,
            literal: p.source == d.source_hash,
        });

        match ctx.visited_sources.contains(&d.source_hash) {
            false => ctx.visited_sources.insert(d.source_hash),
            true => return None,
        };

        if ctx.pat_sources.as_ref().map(|h| h.contains(&d.source_hash)) == Some(false) {
            st.push_str(&format!("\n/** skip resolve {} */\n", d.source));
            return None;
        }

        if ctx.resolve_deps {
            if let Some(dep_match) = Emit::content(st, ctx, ctx.defult_document) {
                let current_matched = matched.as_ref().unwrap();
                matched = Some(PatternMatched {
                    literal: current_matched.literal.max(dep_match.literal),
                    source: current_matched.source.max(dep_match.source),
                });
            }

            st.push_str(&d.decl_stmt);
        }

        let mut lt_ro_skip = false;
        for t in d.parse.compressed_tokens.iter() {
            match t {
                Token::Include(t) => match ctx.resolve_deps {
                    true => (0..t.len).for_each(|_| st.push(' ')),
                    false => st.push_str(&format!("{: <len$}", "import", len = t.len as usize)),
                },
                Token::IncludePath(t) => {
                    if !ctx.resolve_deps {
                        st.push('"');
                        st.push_str(t.lit);
                        st.push('"');
                        continue;
                    }

                    let dep_path = ctx.proxy_state.path_resolver(&d.path, t.lit);
                    let dep_uri = || ctx.proxy_state.path_to_uri(&dep_path);
                    let dep_doc = dep_uri().and_then(|uri| ctx.proxy_state.get_doc(&uri));
                    let link = dep_doc.as_ref().map(|d| d.link_stmt.as_str());

                    st.push_str(link.unwrap_or(&DocumentLinkStatement::undefined()));

                    if dep_doc.is_ok()
                        && let Some(dep_match) = Emit::content(st, ctx, &dep_uri().unwrap())
                    {
                        let current_matched = matched.as_ref().unwrap();
                        matched = Some(PatternMatched {
                            literal: current_matched.literal.max(dep_match.literal),
                            source: current_matched.source.max(dep_match.source),
                        });
                    }

                    st.push('\n'); // traling statements after include path on current line
                    (0..(t.line_col.col + t.lit.len() as u32 + 2)).for_each(|_| st.push(' '));
                }
                Token::RegionOpen(t) => {
                    lt_ro_skip = true;
                    (0..(t.len - 1)).for_each(|_| st.push(' '));
                    st.push('`');
                }
                Token::LineTerminator(_) if lt_ro_skip => {
                    lt_ro_skip = false;
                }
                Token::RegionClose(t) => {
                    st.push('`');
                    st.push(';');
                    (0..(t.len - 2)).for_each(|_| st.push(' '));
                }
                Token::LineTerminator(_) => st.push('\n'),
                Token::CommonWithLineEnding(t) => st.traverse_common(ctx, &mut matched, t.text),
                Token::Common(t) => st.traverse_common(ctx, &mut matched, t.text),
                Token::Eoi(_) => {}
            }
        }

        if ctx.is_default_context {
            if matches!(matched.as_ref().map(|m| m.literal && m.source), Some(true)) {
                st.push_pattern_source(d.source_hash);
            }
        } else if matches!(matched.as_ref().map(|m| m.literal), Some(true)) {
            st.push_pattern_source(d.source_hash);
        }

        if target == ctx.defult_document {
            ctx.is_default_context = false;
        }

        matched
    }
}

impl Emit {
    fn push_str(&mut self, str: &str) {
        match self {
            Emit::WithDstContent(dst_content, _) => dst_content.push_str(str),
            _ => unreachable!(),
        }
    }

    fn push_pattern_source(&mut self, source: SourceHash) {
        match self {
            Emit::WithDstContent(_, Some(sources)) => sources.insert(source),
            _ => unreachable!(),
        };
    }

    fn push(&mut self, char: char) {
        match self {
            Emit::WithDstContent(dst_content, _) => dst_content.push(char),
            _ => unreachable!(),
        }
    }

    fn traverse_common(
        &mut self,
        ctx: &mut Context<'_>,
        matched: &mut Option<PatternMatched>,
        t: &str,
    ) {
        let check = |_| t.contains(ctx.pat.as_ref().unwrap().lit);
        if matched.as_ref().map(check).is_some_and(|matched| matched) {
            let is_source_traversed = matched.as_ref().unwrap().source;
            *matched = Some(PatternMatched {
                literal: true,
                source: is_source_traversed,
            });
        }
        self.push_str(t)
    }
}
