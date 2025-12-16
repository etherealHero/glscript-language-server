use std::collections::HashSet;
use std::sync::Arc;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;
use sourcemap::SourceMap;

use crate::parser::Token;
use crate::proxy::PROXY_WORKSPACE;
use crate::state::State;
use crate::types::{DependencyHash, DocumentIdentifier, PendingMap, Source, SourceHash};

pub const BUILD_FILE: &'static str = "build.js.emitted";

#[cfg(debug_assertions)]
pub const BUILD_SOURCEMAP_FILE: &'static str = "build.js.emitted.map";

#[derive(Clone, Debug)]
pub struct Build {
    pub emit_text: String,
    pub emit_uri: Uri,

    dependency_hash: Vec<DependencyHash>,
    source_map: SourceMap,
}

impl Build {
    pub fn sources(&self) -> HashSet<Source> {
        self.source_map
            .sources()
            .map(|s| Source::new(s.into()))
            .collect()
    }

    pub fn dependency_hash(&self) -> DependencyHash {
        (&self.dependency_hash).into()
    }

    pub fn forward_src_position(
        &self,
        pos: &lsp::Position,
        pos_source: &Source,
    ) -> Option<lsp::Position> {
        let mut token: Option<sourcemap::Token> = None;

        if !self.sources().contains(pos_source) {
            return None;
        }

        for t in self.source_map.tokens() {
            if t.get_source() != Some(&pos_source) {
                continue;
            }
            if t.get_src_line() == pos.line && t.get_src_col() <= pos.character {
                token = Some(t);
            }
            if t.get_src_line() > pos.line {
                break;
            }
        }

        if let Some(t) = token {
            let line = t.get_dst_line();
            let character = t.get_dst_col() + (pos.character - t.get_src_col());
            Some(lsp::Position::new(line, character))
        } else {
            None
        }
    }

    pub fn forward_src_range(
        &self,
        range: &lsp::Range,
        range_source: &Source,
    ) -> Option<lsp::Range> {
        let build_start_pos = self.forward_src_position(&range.start, range_source);
        let build_end_pos = self.forward_src_position(&range.end, range_source);
        match (build_start_pos, build_end_pos) {
            (Some(start), Some(end)) => Some(lsp::Range::new(start, end)),
            _ => None,
        }
    }

    pub fn forward_build_position(&self, pos: &lsp::Position) -> Option<(lsp::Position, Source)> {
        match self.source_map.lookup_token(pos.line, pos.character) {
            Some(t) if t.get_source().is_none() => return None,
            None => return None,
            Some(t) => {
                let line = t.get_src_line();
                let character = t.get_src_col() + (pos.character - t.get_dst_col());
                let source = t.get_source().expect("forward back token must have source");
                Some((
                    lsp::Position::new(line, character),
                    Source::new(source.into()),
                ))
            }
        }
    }

    pub fn forward_build_range(&self, range: &lsp::Range) -> Option<(lsp::Range, Source)> {
        let source_start_pos = self.forward_build_position(&range.start);
        let source_end_pos = self.forward_build_position(&range.end);
        match (source_start_pos, source_end_pos) {
            (Some((start, source)), Some((end, _))) => Some((lsp::Range::new(start, end), source)),
            _ => None,
        }
    }
}

impl Build {
    #[tracing::instrument(skip(state, uri, prev_build))]
    pub fn new(state: &State, uri: &Uri, prev_build: Option<Arc<Self>>) -> anyhow::Result<Self> {
        let (ref mut pending_maps, dependency_hash, emit_buffer) = {
            if let Some(pb) = prev_build {
                (
                    Vec::with_capacity(pb.source_map.get_token_count() as usize),
                    Vec::with_capacity(pb.sources().len()),
                    String::with_capacity(pb.emit_text.len()),
                )
            } else {
                (vec![], vec![], String::new())
            }
        };

        let mut ctx = EmitCtx {
            state,
            dst_line: 0,
            emit_buffer,
            pending_maps,
            visited_sources: HashSet::<SourceHash>::with_capacity(dependency_hash.len()),
            global_document: &state.get_global_doc(),
            dependency_hash,
        };

        emit(&mut ctx, uri)?;

        let source_map = PendingMap::into_sourcemap(ctx.pending_maps);

        #[cfg(debug_assertions)]
        {
            let p = state.get_project();
            let mut sm_json = Vec::new();
            let _ = source_map.to_writer(&mut sm_json);
            let emitted_source_map = String::from_utf8(sm_json)?;
            let _ = std::fs::write(p.join(BUILD_SOURCEMAP_FILE), emitted_source_map);
            let build = format!(
                "{}\n//# sourceMappingURL=/{BUILD_SOURCEMAP_FILE}",
                &ctx.emit_buffer
            );
            let _ = std::fs::write(p.join(BUILD_FILE), build);
        }

        // FIXME: change to <project.join(PROXY_WORKSPACE)>/<source_path>/<source_hash.js>
        //                                                  ^^^^^^^^^^^^^ add subdirs like source file
        //          instead <project.join(PROXY_WORKSPACE)>/<source_hash.js>
        let ident = state.get_doc(uri)?.source_ident.to_string();
        let emit_path = state
            .get_project()
            .join(PROXY_WORKSPACE)
            .join(format!("{ident}.js"));
        let emit_uri = Uri::from_file_path(emit_path).unwrap();

        let b = Self {
            dependency_hash: ctx.dependency_hash,
            emit_text: ctx.emit_buffer,
            source_map,
            emit_uri,
        };

        Ok(b)
    }
}

struct EmitCtx<'a> {
    state: &'a State,
    global_document: &'a Uri,

    pending_maps: &'a mut Vec<PendingMap>,
    dst_line: u32,

    visited_sources: HashSet<SourceHash>,
    emit_buffer: String,
    dependency_hash: Vec<DependencyHash>,
}

impl<'a> EmitCtx<'a> {
    fn map(&mut self, dst_col: u32, src_line: u32, src_col: u32, source: Option<Arc<Source>>) {
        self.pending_maps.push(PendingMap::new(
            self.dst_line,
            dst_col,
            src_line,
            src_col,
            source,
        ));
    }

    fn push(&mut self, char: char) {
        self.emit_buffer.push(char);
    }

    fn push_str(&mut self, str: &str) {
        self.emit_buffer.push_str(str);
    }

    fn line(&mut self) {
        self.dst_line += 1;
    }
}

fn emit(ctx: &mut EmitCtx, target: &Uri) -> anyhow::Result<()> {
    let d = ctx.state.get_doc(target)?;
    let (source, path, tokens, decl_stmt) = (&d.source, &d.path, d.tokens.iter(), &d.decl_stmt);

    match ctx.visited_sources.contains(&d.source_hash) {
        true => return Ok(()),
        false => ctx.visited_sources.insert(d.source_hash),
    };

    ctx.dependency_hash.push(d.dependency_hash);

    let _ = emit(ctx, ctx.global_document);

    // TODO: ? append context prefix with root entry uri if definition failed with other builds
    ctx.push_str(decl_stmt);
    ctx.map(0, 0, 0, Some(source.clone()));
    ctx.line();

    let mut skip_lt_after_region_open = false;
    let mut first_lt_after_region_open = false;
    let mut first_lt_after_region_open_offset = 0;

    for t in tokens {
        match t {
            Token::Include => {}
            Token::IncludePath(t) => {
                let (line, col) = (t.line, t.col);
                let dep_lit = t.text.trim_matches(|c| ['\'', '"', '<', '>'].contains(&c));
                let dep_path = ctx.state.path_resolver(&path, dep_lit);
                let dep_uri = &Uri::from_file_path(dep_path.as_path()).unwrap();
                let dep_link = ctx
                    .state
                    .get_doc(dep_uri)
                    .and_then(|d| Ok(d.link_stmt.clone()))
                    .unwrap_or_else(|_| {
                        let dep_ident = &DocumentIdentifier::new(&Source::new(dep_lit.into()));
                        Arc::new(dep_ident.into())
                    });

                ctx.push_str(&dep_link);
                ctx.line();
                ctx.map(dep_link.left_offset, line, col, Some(source.clone()));
                ctx.map(dep_link.right_offset, 0, 0, None);
                ctx.line();
                ctx.line();

                // TODO: if err ? should emit blk ?
                let _ = emit(ctx, dep_uri);
                for _ in 0..(col + t.text.len() as u32) {
                    ctx.push(' ');
                }
            }
            _ => {
                let mut add_sourcemap = |dst_col: u32, line: u32, col: u32| {
                    let source = Some(source.clone());
                    let dst_col = match first_lt_after_region_open {
                        true => dst_col + first_lt_after_region_open_offset,
                        false => dst_col,
                    };
                    ctx.map(dst_col, line, col, source);
                };

                match t {
                    Token::LineTerminator(_) if skip_lt_after_region_open => {
                        skip_lt_after_region_open = false;
                        first_lt_after_region_open = true;
                    }
                    Token::RegionOpen(t) => {
                        add_sourcemap(0, t.line, t.col);
                        skip_lt_after_region_open = true;
                        first_lt_after_region_open_offset = t.text.len() as u32;
                        for _ in 0..(t.text.len() - 1) {
                            ctx.push(' ');
                        }
                        ctx.push('`');
                    }
                    Token::RegionClose(t) => {
                        add_sourcemap(0, t.line, t.col);
                        ctx.push('`');
                        ctx.push(';');
                        for _ in 0..(t.text.len() - 2) {
                            ctx.push(' ');
                        }
                    }
                    // FIXME: fix missing EOI
                    Token::LineTerminator(t) => {
                        add_sourcemap(t.col, t.line, t.col);
                        first_lt_after_region_open = false;
                        ctx.line();
                        ctx.push('\n');
                    }
                    Token::CommonWithLineBreak(t) => {
                        add_sourcemap(t.col, t.line, t.col);
                        first_lt_after_region_open = false;
                        ctx.line();
                        ctx.push_str(&t.text);
                    }
                    Token::Common(t) => {
                        add_sourcemap(t.col, t.line, t.col);
                        ctx.push_str(&t.text);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    Ok(())
}
