use async_lsp::lsp_types::Url as Uri;
use std::{collections::HashSet, sync::Arc};

use crate::builder::emit::{Context, Emit};
use crate::parser::{LineCol, Token};
use crate::types::{DocumentLinkStatement, Source};

/// SourceMap
impl Emit {
    pub fn sourcemap(st: &mut Emit, ctx: &mut Context, target: &Uri) {
        let d = match ctx.proxy_state.get_doc(target) {
            Ok(doc) => doc,
            Err(_) => return,
        };
        let (source, path, tokens) = (&d.source, &d.path, d.parse.compressed_tokens.iter());
        let source = (*source.clone()).clone();

        if ctx.visited_sources.contains(&d.source_hash) {
            return;
        }

        ctx.visited_sources.insert(d.source_hash);
        ctx.stack.push((source, (0, 0).into(), 0));

        let src_id = st.add_source(d.source.clone(), ctx);

        let c = |h: &HashSet<_>| h.contains(&d.source_hash);
        if ctx.pat_sources.as_ref().map(c) == Some(false) {
            st.line_break();
            st.line_break();
            return;
        }

        if ctx.resolve_deps {
            Emit::sourcemap(st, ctx, ctx.defult_document);

            // DocumentDeclarationStatement
            st.line_break();
            st.add_token(0, 0, 0, src_id);
            st.line_break();
        }

        let mut lt_ro_skip = false;
        let mut lt_ro = false;
        let mut lt_ro_offset = 0u32;
        let add_map =
            |dst_col: u32, pos: &LineCol, st: &mut Emit, lt_ro: bool, lt_ro_offset: u32| {
                let dst_col = match lt_ro {
                    true => dst_col + lt_ro_offset,
                    false => dst_col,
                };
                st.add_token(dst_col, pos.line, pos.col, src_id);
            };

        for t in tokens {
            match t {
                Token::Include(t) => add_map(t.line_col.col, &t.line_col, st, lt_ro, lt_ro_offset),
                Token::IncludePath(t) => {
                    let dep_path = ctx.proxy_state.path_resolver(path, t.lit);
                    let dep_uri = ctx.proxy_state.path_to_uri(&dep_path);
                    let (left_offset, right_offset, doc_uri) = if let Ok(uri) = dep_uri {
                        if let Ok(d) = ctx.proxy_state.get_doc(&uri) {
                            (d.link_stmt.left_offset, d.link_stmt.right_offset, Some(uri))
                        } else {
                            let stmt = DocumentLinkStatement::undefined();
                            (stmt.left_offset, stmt.right_offset, None)
                        }
                    } else {
                        let stmt = DocumentLinkStatement::undefined();
                        (stmt.left_offset, stmt.right_offset, None)
                    };

                    if !ctx.resolve_deps {
                        let real_col = t.line_col.col - 1;
                        let pos = &((t.line_col.line, real_col).into());
                        add_map(real_col, pos, st, lt_ro, lt_ro_offset);

                        if let Some(dep_uri) = doc_uri {
                            let dep_source = ctx.proxy_state.get_doc(&dep_uri).unwrap().source;
                            let arc_dep_source = dep_source.clone();
                            let dep_source = (*arc_dep_source.clone()).clone();
                            ctx.stack
                                .push((dep_source, t.line_col.clone(), t.lit.len()));
                            st.add_source(arc_dep_source, ctx);
                        }

                        continue;
                    }

                    st.line_break();
                    st.add_token(left_offset, t.line_col.line, t.line_col.col, src_id);
                    st.add_token(right_offset, 0, 0, !0);
                    st.line_break();

                    if let Some(target) = doc_uri {
                        Emit::sourcemap(st, ctx, &target);
                    }

                    st.line_break(); // traling statements after include path on current line
                }
                Token::RegionOpen(t) => {
                    add_map(0, &t.line_col, st, lt_ro, lt_ro_offset);
                    lt_ro_skip = true;
                    lt_ro_offset = t.len;
                }
                Token::LineTerminator(_) if lt_ro_skip => {
                    lt_ro_skip = false;
                    lt_ro = true;
                }
                Token::RegionClose(t) => add_map(0, &t.line_col, st, lt_ro, lt_ro_offset),
                Token::LineTerminator(t) => {
                    add_map(t.col, t, st, lt_ro, lt_ro_offset);
                    lt_ro = false;
                    st.line_break();
                }
                Token::CommonWithLineEnding(t) => {
                    add_map(t.line_col.col, &t.line_col, st, lt_ro, lt_ro_offset);
                    lt_ro = false;
                    st.line_break();
                }
                Token::Common(t) => add_map(t.line_col.col, &t.line_col, st, lt_ro, lt_ro_offset),
                Token::Eoi(t) => add_map(t.col, t, st, lt_ro, lt_ro_offset),
            }
        }

        ctx.stack.pop();
    }

    pub fn line_break(&mut self) {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(_, dst_line, _) => *dst_line += 1,
            _ => unreachable!(),
        };
    }
}

impl Emit {
    /// returns source id of [`crate::builder::SourceMapBuilder`]
    fn add_source(&mut self, source: Arc<Source>, ctx: &Context) -> u32 {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(builder, _, swis) => {
                let stack: Vec<_> = ctx.stack.clone().drain(1..).collect();
                if !stack.is_empty() {
                    swis.insert(source.clone(), stack);
                };

                builder.add_source_with_id(source)
            }
            _ => unreachable!(),
        }
    }

    fn add_token(&mut self, dst_col: u32, src_line: u32, src_col: u32, src_id: u32) {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(builder, dst_line, _) => {
                builder.tokens.push(sourcemap::RawToken {
                    dst_line: *dst_line,
                    dst_col,
                    src_line,
                    src_col,
                    src_id,
                    name_id: !0,
                    is_range: false,
                })
            }
            _ => unreachable!(),
        }
    }
}
