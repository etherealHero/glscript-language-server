use async_lsp::lsp_types::Url as Uri;
use std::sync::Arc;

use crate::builder::emit::{Context, Emit};
use crate::parser::{LineCol, Token};
use crate::types::{DocumentLinkStatement, Source};

/// SourceMap
impl Emit {
    #[cfg_attr(feature = "profiling", tracing::instrument(skip_all))]
    pub fn sourcemap(st: &mut Emit, ctx: &mut Context, target: &Uri) {
        Emit::_sourcemap(st, ctx, target);
    }

    fn _sourcemap(st: &mut Emit, ctx: &mut Context, target: &Uri) {
        let Ok(d) = ctx.proxy_state.get_doc(target) else {
            return;
        };

        match ctx.visited_sources.contains(&d.source_hash) {
            false => ctx.visited_sources.insert(d.source_hash),
            true => return,
        };

        st.add_source_stack((*d.source).clone(), ctx);

        if ctx.pat_sources.as_ref().map(|h| h.contains(&d.source_hash)) == Some(false) {
            st.line_break();
            st.line_break();
            return;
        }

        let src_id = st.add_source(d.source.clone());

        if ctx.resolve_deps {
            let source = ctx
                .proxy_state
                .get_doc(ctx.default_document)
                .map(|d| (*d.source).clone());

            if let Ok(source) = source {
                let stack = (source, (0, 0).into(), 0);
                ctx.stack.push(stack);
                Emit::_sourcemap(st, ctx, ctx.default_document);
                ctx.stack.pop();
            }

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

        for t in d.parse.compressed_tokens.iter() {
            match t {
                Token::Include(t) => add_map(t.line_col.col, &t.line_col, st, lt_ro, lt_ro_offset),
                Token::IncludePath(t) => {
                    let dep_path = ctx.proxy_state.path_resolver(&d.path, t.lit);
                    let dep_uri = || ctx.proxy_state.path_to_uri(&dep_path);
                    let dep_doc = dep_uri().and_then(|uri| ctx.proxy_state.get_doc(&uri));
                    let dep_exists_and_not_visited = dep_doc
                        .as_ref()
                        .map(|d| ctx.visited_sources.contains(&d.source_hash))
                        .is_ok_and(|visited| !visited);

                    if let Ok(doc) = dep_doc.as_ref() {
                        let source = doc.source.clone();
                        let source = (*source).clone();
                        let stack = (source.clone(), t.line_col.clone(), t.lit.len());
                        ctx.stack.push(stack);

                        // 1* - emit stack for transpile builds like bundle
                        if !ctx.resolve_deps {
                            st.add_source_stack(source, ctx);
                            ctx.stack.pop();
                        }
                    }

                    if !ctx.resolve_deps {
                        let real_col = t.line_col.col - 1;
                        let pos = &((t.line_col.line, real_col).into());
                        add_map(real_col, pos, st, lt_ro, lt_ro_offset);

                        continue;
                    }

                    let link = dep_doc.as_ref().map(|d| d.link_stmt.clone());
                    let link = link.unwrap_or(DocumentLinkStatement::undefined().into());

                    st.line_break();
                    st.add_token(link.left_offset, t.line_col.line, t.line_col.col, src_id);
                    st.add_token(link.right_offset, 0, 0, !0);
                    st.line_break();

                    // 1*
                    if dep_exists_and_not_visited {
                        Emit::_sourcemap(st, ctx, &dep_uri().unwrap());
                        ctx.stack.pop();
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
    }

    #[inline]
    pub fn line_break(&mut self) {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(_, dst_line, _) => *dst_line += 1,
            _ => unreachable!(),
        };
    }
}

impl Emit {
    /// returns source id of [`crate::builder::SourceMapBuilder`]
    fn add_source(&mut self, source: Arc<Source>) -> u32 {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(builder, _, _) => {
                builder.add_source_with_id(source)
            }
            _ => unreachable!(),
        }
    }

    fn add_source_stack(&mut self, source: Source, ctx: &Context) {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(_, _, sources_stack) => {
                if !ctx.stack.is_empty() {
                    let mut ctx_stack = ctx.stack.clone();

                    sources_stack
                        .entry(source)
                        .and_modify(|source_links| {
                            assert!(
                                !ctx.resolve_deps,
                                "extensible stack for transpile builds only"
                            );
                            source_links.append(&mut ctx_stack);
                        })
                        .or_insert(ctx_stack);
                };
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
