use async_lsp::lsp_types::{self as lsp, Url as Uri};
use derive_more::Constructor;
use sourcemap::SourceMap;
use std::{collections::HashSet, sync::Arc};

use crate::parser::{LineCol, Token};
use crate::state::State;
use crate::types::{DocumentLinkStatement, Source, SourceHash, SourceMapBuilder, SourcePattern};

pub const BUILD_FILE_EXT: &str = ".emitted";

#[derive(Debug, Constructor)]
pub struct Build {
    pub content: String,
    pub uri: Uri,

    source_map: SourceMap,
    tokens_count: usize,
}

/// Forwarding
impl Build {
    pub fn sources(&self) -> HashSet<Source> {
        self.source_map
            .sources()
            .map(|s| Source::new(s.into()))
            .collect()
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
            if t.get_source() != Some(pos_source) {
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
            Some(t) if t.get_source().is_none() => None,
            Some(t) => {
                let line = t.get_src_line();
                let character = t.get_src_col() + (pos.character - t.get_dst_col());
                let source = t.get_source().expect("forward back token must have source");
                Some((
                    lsp::Position::new(line, character),
                    Source::new(source.into()),
                ))
            }
            _ => None,
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

type PatternSources = HashSet<SourceHash>;

#[derive(Constructor)]
pub struct BuildOptionsBuilder<'a> {
    options: BuildOptions<'a>,
}

impl<'a> BuildOptionsBuilder<'a> {
    pub fn init(uri: &'a Uri, st: &'a State) -> Self {
        Self::new(BuildOptions::new(st, uri, None, None, None, true))
    }

    pub fn with_previous_build(mut self, pb: Arc<Build>) -> Self {
        self.options.pb = Some(pb);
        self
    }

    pub fn with_source_pattern(mut self, pat: SourcePattern<'a>) -> Self {
        self.options.pat = Some(pat);
        self
    }

    pub fn transpile_mode(mut self) -> Self {
        self.options.resolve_deps = false;
        self
    }

    pub fn target(&self) -> &'a Uri {
        self.options.uri
    }
}

#[derive(Constructor)]
struct BuildOptions<'a> {
    st: &'a State,
    uri: &'a Uri,
    pb: Option<Arc<Build>>,
    pat: Option<SourcePattern<'a>>,
    pat_sources: Option<PatternSources>,
    resolve_deps: bool,
}

impl Build {
    pub fn create(mut opt_builder: BuildOptionsBuilder) -> anyhow::Result<Self> {
        if opt_builder.options.pat.is_none() {
            return Build::emit(&opt_builder.options).map(|s| s.0);
        }

        let pat_source = opt_builder.options.pat.as_ref().unwrap().source;
        let (_, pat_sources) = Build::emit(&opt_builder.options)?;

        if !pat_sources.as_ref().unwrap().contains(&pat_source) {
            let msg = "build does not contain desired source pattern";
            return Err(anyhow::Error::msg(msg));
        }

        opt_builder.options.pat_sources = pat_sources;
        opt_builder.options.pat = None;

        let (build_with_tree_shaking, _) = Build::emit(&opt_builder.options)?;
        let sources = &build_with_tree_shaking.sources();

        debug_assert!(sources.iter().any(|s| SourceHash::new(s) == pat_source));

        Ok(build_with_tree_shaking)
    }

    fn emit(opt: &BuildOptions) -> anyhow::Result<(Self, Option<PatternSources>)> {
        let doc = opt.st.get_doc(opt.uri)?;
        let (mut initial_buf, sources_cap, tokens_cap) = {
            match &opt.pb {
                Some(b) => (
                    String::with_capacity(b.content.len()),
                    b.sources().len(),
                    b.tokens_count,
                ),
                None => (String::new(), 0, 0),
            }
        };

        if opt.resolve_deps {
            initial_buf.push_str("/** DO NOT EDIT THIS FILE. Build of '");
            initial_buf.push_str(&doc.source);
            initial_buf.push_str("' with sourcemaps ");
            initial_buf.push_str("https://evanw.github.io/source-map-visualization/ ");
            initial_buf.push_str("by glscript-language-server */\n");
        }

        let default_doc = &opt.st.get_default_doc();
        let new_ctx = || {
            let visited = HashSet::<SourceHash>::with_capacity(sources_cap);
            let (p, s, i) = (opt.pat.clone(), opt.pat_sources.clone(), opt.resolve_deps);
            Context::new(opt.st, default_doc, visited, p, s, i, false)
        };

        {
            let builder = SourceMapBuilder::with_capacity(tokens_cap, sources_cap);
            let dst_line = if opt.resolve_deps { 1 } else { 0 };
            let mut emit_state = Emit::WithSourceMapBuilderAndDstLine(builder, dst_line);
            Emit::prepare_par_iter(&mut emit_state, &mut new_ctx(), opt.uri);
            emit_state.finish(opt.st);
        }

        let sourcemap_task = || {
            let builder = SourceMapBuilder::with_capacity(tokens_cap, sources_cap);
            let dst_line = if opt.resolve_deps { 1 } else { 0 };
            let mut emit_sourcemap_state = Emit::WithSourceMapBuilderAndDstLine(builder, dst_line);
            Emit::sourcemap(&mut emit_sourcemap_state, &mut new_ctx(), opt.uri);
            match emit_sourcemap_state.finish(opt.st) {
                EmitResult::TokensCountAndSourceMap(count, sm) => (count, sm),
                _ => unreachable!(),
            }
        };
        let content_task = || {
            let pat_sources = opt.pat.as_ref().map(|_| HashSet::default());
            let mut emit_st = Emit::WithDstContent(initial_buf, pat_sources);
            Emit::content(&mut emit_st, &mut new_ctx(), opt.uri);
            match emit_st.finish(opt.st) {
                EmitResult::Content(content, pat_sources) => (content, pat_sources),
                _ => unreachable!(),
            }
        };

        let ((tokens_count, source_map), (content, pattern_sources)) =
            rayon::join(sourcemap_task, content_task); // TODO: rebuild only sourcemap on dep_hash eq prev

        #[cfg(debug_assertions)]
        emit_on_disk(opt, &doc, &source_map, &content)?;

        let emit_uri = match opt.resolve_deps {
            true => doc.bundle_uri.as_ref().clone(),
            false => doc.transpile_uri.as_ref().clone(),
        };

        let build = Build::new(content, emit_uri, source_map, tokens_count);

        Ok((build, pattern_sources))
    }
}

#[cfg(debug_assertions)]
fn emit_on_disk(
    opt: &BuildOptions<'_>,
    doc: &crate::types::Document,
    source_map: &SourceMap,
    content: &String,
) -> Result<(), anyhow::Error> {
    use crate::proxy::PROXY_WORKSPACE;
    use base64::prelude::{BASE64_STANDARD, Engine as _};

    let mut sm_json = Vec::new();
    let _ = source_map.to_writer(&mut sm_json);
    let sm_base64 = BASE64_STANDARD.encode(&sm_json);
    let build = format!(
        "{}\n//# sourceMappingURL=data:application/json;base64,{}",
        &content, sm_base64
    );
    let debug_source = match opt.resolve_deps {
        true => doc.source.to_string() + BUILD_FILE_EXT,
        false => doc.source.to_string() + ".transpiled" + BUILD_FILE_EXT,
    };
    let proxy_ws = opt.st.get_project().join(PROXY_WORKSPACE);
    let debug_filepath = proxy_ws.join("./debug").join(debug_source);
    let mut sourcemap_file = debug_filepath.clone();
    sourcemap_file.add_extension("map");
    std::fs::create_dir_all(debug_filepath.parent().unwrap()).unwrap();
    std::fs::write(debug_filepath.clone(), build).unwrap();
    std::fs::write(sourcemap_file, String::from_utf8(sm_json)?).unwrap();
    Ok(())
}

#[derive(Constructor)]
struct Context<'a> {
    proxy_state: &'a State,
    defult_document: &'a Uri,
    visited_sources: HashSet<SourceHash>,
    pat: Option<SourcePattern<'a>>,
    pat_sources: Option<PatternSources>,
    resolve_deps: bool,
    is_default_context: bool,
}

enum Emit {
    WithSourceMapBuilderAndDstLine(SourceMapBuilder, u32),
    WithDstContent(String, Option<PatternSources>),
}

enum EmitResult {
    TokensCountAndSourceMap(usize, sourcemap::SourceMap),
    Content(String, Option<PatternSources>),
}

impl Emit {
    fn prepare_par_iter(st: &mut Emit, ctx: &mut Context, target: &Uri) {
        let d = match ctx.proxy_state.get_doc(target) {
            Ok(doc) => doc,
            Err(_) => return,
        };
        let (path, tokens) = (&d.path, d.parse.compressed_tokens.iter());
        match ctx.visited_sources.contains(&d.source_hash) {
            true => return,
            false => ctx.visited_sources.insert(d.source_hash),
        };
        Emit::prepare_par_iter(st, ctx, ctx.defult_document);
        st.line_break(); // < DocumentDeclarationStatement
        st.line_break(); // <
        let mut lt_ro_skip = false;
        for t in tokens {
            match t {
                Token::IncludePath(t) => {
                    let dep_path = ctx.proxy_state.path_resolver(path, t.lit);
                    let dep_uri = ctx.proxy_state.path_to_uri(&dep_path);
                    let doc_uri = if let Ok(uri) = dep_uri {
                        match ctx.proxy_state.get_doc(&uri).is_ok() {
                            true => Some(uri),
                            false => None,
                        }
                    } else {
                        None
                    };

                    st.line_break(); // < DocumentLinkStatement
                    st.line_break(); // <

                    if let Some(target) = doc_uri {
                        Emit::prepare_par_iter(st, ctx, &target);
                    }

                    st.line_break(); // traling statements after include path on current line
                }
                Token::RegionOpen(_) => lt_ro_skip = true,
                Token::LineTerminator(_) if lt_ro_skip => lt_ro_skip = false,
                Token::LineTerminator(_) | Token::CommonWithLineEnding(_) => st.line_break(),
                _ => {}
            }
        }
    }

    fn finish(self, state: &State) -> EmitResult {
        match self {
            Emit::WithDstContent(dst_content, pattern_sources) => {
                EmitResult::Content(dst_content, pattern_sources)
            }
            Emit::WithSourceMapBuilderAndDstLine(b, _) => {
                EmitResult::TokensCountAndSourceMap(b.tokens.len(), b.into_sourcemap(state))
            }
        }
    }
}

/// SourceMap
impl Emit {
    fn add_source(&mut self, source: Arc<Source>) -> u32 {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(builder, _) => builder.add_source_with_id(source),
            _ => unreachable!(),
        }
    }

    fn add_token(&mut self, dst_col: u32, src_line: u32, src_col: u32, src_id: u32) {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(builder, dst_line) => {
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

    fn line_break(&mut self) {
        match self {
            Emit::WithSourceMapBuilderAndDstLine(_, dst_line) => *dst_line += 1,
            _ => unreachable!(),
        };
    }

    fn sourcemap(st: &mut Emit, ctx: &mut Context, target: &Uri) {
        let d = match ctx.proxy_state.get_doc(target) {
            Ok(doc) => doc,
            Err(_) => return,
        };
        let (source, path, tokens) = (&d.source, &d.path, d.parse.compressed_tokens.iter());
        let src_id = st.add_source(source.clone());

        match ctx.visited_sources.contains(&d.source_hash) {
            true => return,
            false => ctx.visited_sources.insert(d.source_hash),
        };

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
                    if !ctx.resolve_deps {
                        let real_col = t.line_col.col - 1;
                        let pos = &((t.line_col.line, real_col).into());
                        add_map(real_col, pos, st, lt_ro, lt_ro_offset);
                        continue;
                    }

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
    }
}

#[derive(Default)]
struct PatternMatched {
    literal: bool,
    source: bool,
}

/// Destination content
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

    fn content(st: &mut Emit, ctx: &mut Context, target: &Uri) -> Option<PatternMatched> {
        if target == ctx.defult_document {
            ctx.is_default_context = true;
        }

        let d = match ctx.proxy_state.get_doc(target) {
            Ok(doc) => doc,
            Err(_) => return None,
        };
        let mut matched = ctx.pat.as_ref().map(|p| PatternMatched {
            source: p.source == d.source_hash,
            literal: p.source == d.source_hash,
        });
        let (path, tokens, decl_stmt) = (&d.path, d.parse.compressed_tokens.iter(), &d.decl_stmt);

        match ctx.visited_sources.contains(&d.source_hash) {
            true => return None,
            false => ctx.visited_sources.insert(d.source_hash),
        };

        let c = |h: &HashSet<_>| h.contains(&d.source_hash);
        if ctx.pat_sources.as_ref().map(c) == Some(false) {
            st.push_str(&format!("\n/** skip resolve {} */\n", d.source));
            return None;
        }

        if ctx.resolve_deps {
            if let Some(dep_match) = Emit::content(st, ctx, ctx.defult_document) {
                let current_matched = matched.as_ref().unwrap();
                matched = Some(PatternMatched {
                    literal: current_matched.literal || dep_match.literal,
                    source: current_matched.source || dep_match.source,
                });
            }

            st.push_str(decl_stmt);
        }

        let mut lt_ro_skip = false;
        for t in tokens {
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

                    let dep_path = ctx.proxy_state.path_resolver(path, t.lit);
                    if let Ok(dep_uri) = ctx.proxy_state.path_to_uri(&dep_path) {
                        if let Ok(dep_doc) = ctx.proxy_state.get_doc(&dep_uri) {
                            st.push_str(&dep_doc.link_stmt);

                            if let Some(dep_match) = Emit::content(st, ctx, &dep_uri) {
                                let current_matched = matched.as_ref().unwrap();
                                matched = Some(PatternMatched {
                                    literal: current_matched.literal || dep_match.literal,
                                    source: current_matched.source || dep_match.source,
                                });
                            }
                        } else {
                            st.push_str(&DocumentLinkStatement::undefined())
                        };
                    } else {
                        st.push_str(&DocumentLinkStatement::undefined());
                    };

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
