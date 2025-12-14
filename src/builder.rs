use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;

use sourcemap::{SourceMap, SourceMapBuilder, Token};

use crate::parser::Rule;
use crate::proxy::PROXY_WORKSPACE;
use crate::state::{DocumentHash, DocumentIdentifier, Source, SourcePath, State};

pub const BUILD_FILE: &'static str = "build.js.emitted";

#[cfg(debug_assertions)]
pub const BUILD_SOURCEMAP_FILE: &'static str = "build.js.emitted.map";

#[derive(Clone, Debug)]
pub struct Build {
    pub emit_text: String,
    pub emit_uri: Uri,
    pub emit_hash: u64,
    source_map: SourceMap,

    #[deprecated]
    project: SourcePath,
}

impl Build {
    pub fn sources(&self) -> HashSet<Source> {
        self.source_map.sources().map(String::from).collect()
    }

    pub fn forward_src_position(
        &self,
        pos: &lsp::Position,
        pos_source: &Source,
    ) -> Option<lsp::Position> {
        let mut token: Option<Token> = None;

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

    pub fn forward_build_position(&self, pos: &lsp::Position) -> Option<(lsp::Position, Uri)> {
        match self.source_map.lookup_token(pos.line, pos.character) {
            Some(t) if t.get_source().is_none() => return None,
            None => return None,
            Some(t) => {
                let line = t.get_src_line();
                let character = t.get_src_col() + (pos.character - t.get_dst_col());
                let source = t.get_source().expect("forward back token must have source");
                let source_uri = // TODO: return Source
                    Uri::from_file_path(self.project.join(source)).expect("valid source");
                Some((lsp::Position::new(line, character), source_uri))
            }
        }
    }

    pub fn forward_build_range(&self, range: &lsp::Range) -> Option<(lsp::Range, Uri)> {
        let source_start_pos = self.forward_build_position(&range.start);
        let source_end_pos = self.forward_build_position(&range.end);
        match (source_start_pos, source_end_pos) {
            (Some((start, source)), Some((end, _))) => Some((lsp::Range::new(start, end), source)),
            _ => None,
        }
    }

    #[tracing::instrument(
        skip(state, uri, emit_buf_capacity),
        // fields(file = %uri.as_str().split("/").last().unwrap_or_default())
    )]
    pub fn new(state: &State, uri: &Uri, emit_buf_capacity: Option<usize>) -> anyhow::Result<Self> {
        let mut smb = SourceMapBuilder::new(None);
        let visited_sources = &mut HashSet::<DocumentHash>::with_capacity(100);
        let emit_hasher = &mut DefaultHasher::new();
        let project = state.get_project();
        let global_doc = &state.get_global_doc();
        let emit_text = &mut String::with_capacity(emit_buf_capacity.unwrap_or_default());
        let _ = Self::emit(
            state,
            uri,
            global_doc,
            &mut smb,
            &mut 0,
            visited_sources,
            emit_hasher,
            emit_text,
        )?;
        let source_map = smb.into_sourcemap();

        #[cfg(debug_assertions)]
        {
            use std::fs;

            let mut sm_json = Vec::new();
            let _ = source_map.to_writer(&mut sm_json);
            let emitted_source_map = String::from_utf8(sm_json)?;
            let _ = fs::write(project.join(BUILD_SOURCEMAP_FILE), emitted_source_map);
            let build = format!("{emit_text}\n//# sourceMappingURL=/{BUILD_SOURCEMAP_FILE}");
            let _ = fs::write(project.join(BUILD_FILE), build);
        }

        // FIXME: change to <project.join(PROXY_WORKSPACE)>/<source_path>/<source_hash.js>
        //                                                  ^^^^^^^^^^^^^ add subdirs like source file
        //          instead <project.join(PROXY_WORKSPACE)>/<source_hash.js>
        let identifier = state.get_doc(uri)?.ident.to_string();
        let emit_path = project
            .join(PROXY_WORKSPACE)
            .join(format!("{identifier}.js"));
        let emit_uri = Uri::from_file_path(emit_path).unwrap();

        identifier.hash(emit_hasher);

        let b = Self {
            emit_uri,
            emit_text: emit_text.to_owned(),
            emit_hash: emit_hasher.finish(),
            project: project.to_owned(),
            source_map,
        };

        Ok(b)
    }
}

impl Build {
    // TODO: rewrite with EmitConfig

    // TODO: optimize performance
    // 1 find effected IO data
    // 2 save snapshot before update
    // 3 if state of target uri has similar Input snapshot, then fast effect IO for traverse emit
    // 0 maybe save doc version (selfhosted)

    // emit_hash save to state.doc
    // remove build in state on emit_hash changed
    // else update build like doc rope with recalc sourcemap

    // TODO: REFACTOR add param buf, remove Result<String>
    fn emit(
        st: &State,
        uri: &Uri,
        g_uri: &Uri,
        smb: &mut SourceMapBuilder,
        dst_line: &mut u32,
        visited: &mut HashSet<DocumentHash>,
        hasher: &mut impl Hasher,
        writer: &mut String,
    ) -> anyhow::Result<()> {
        let d = st.get_doc(uri)?;
        let (source, path, tokens, decl_stmt) = (&d.source, &d.path, d.tokens.iter(), &d.decl_stmt);

        match visited.contains(&d.hash) {
            true => return Ok(()), // must be empty for emit global doc
            false => visited.insert(d.hash),
        };

        let _ = Self::emit(st, g_uri, g_uri, smb, dst_line, visited, hasher, writer);

        // TODO: ? append context prefix with root entry uri if definition failed with other builds
        writer.push_str(decl_stmt);

        hasher.write_u64(*d.hash);

        smb.add(*dst_line, 0, 0, 0, Some(source), None, false);
        *dst_line += 1; // prepend_module end breakline

        let mut skip_lt_after_region_open = false;
        let mut first_lt_after_region_open = false;
        let mut first_lt_after_region_open_offset = 0;

        for t in tokens {
            match t.rule {
                Rule::IncludeToken => {}
                Rule::IncludePath => {
                    let (line, col) = (t.line, t.col);
                    let dep_lit = t.text.trim_matches(|c| ['\'', '"', '<', '>'].contains(&c));
                    let dep_path = st.path_resolver(&path, dep_lit);
                    let dep_uri = &Uri::from_file_path(dep_path.as_path()).unwrap();
                    let dep_link = st
                        .get_doc(dep_uri)
                        .and_then(|d| Ok(d.link_stmt.clone()))
                        .unwrap_or_else(|_| {
                            let dep_ident = &DocumentIdentifier::new(&dep_lit.to_string());
                            Arc::new(dep_ident.into())
                        });

                    let (prefix, suffix) = (dep_link.left_offset, dep_link.right_offset);

                    dep_link.hash(hasher);
                    line.hash(hasher);
                    col.hash(hasher);

                    writer.push_str(&dep_link);

                    *dst_line += 1;
                    smb.add(*dst_line, prefix, line, col, Some(source), None, false);
                    smb.add(*dst_line, suffix, 0, 0, None, None, false);
                    *dst_line += 2;

                    let dep_build =
                        Self::emit(st, dep_uri, g_uri, smb, dst_line, visited, hasher, writer);

                    // TODO: if err ? should emit blk ?
                    if dep_build.is_ok() {
                        for _ in 0..(col as usize + t.text.len()) {
                            writer.push(' ');
                        }
                    }
                }
                _ => {
                    let mut add_sourcemap = |dst_col: u32| {
                        let source = Some(source.as_str());
                        let dst_col = match first_lt_after_region_open {
                            true => dst_col + first_lt_after_region_open_offset,
                            false => dst_col,
                        };
                        smb.add(*dst_line, dst_col, t.line, t.col, source, None, false)
                    };

                    match t.rule {
                        Rule::LineTerminator if skip_lt_after_region_open => {
                            skip_lt_after_region_open = false;
                            first_lt_after_region_open = true;
                        }
                        Rule::RegionOpen => {
                            add_sourcemap(0);
                            skip_lt_after_region_open = true;
                            first_lt_after_region_open_offset = t.text.len() as u32;
                            for _ in 0..(t.text.len() - 1) {
                                writer.push(' ');
                            }
                            writer.push_str("`");
                        }
                        Rule::RegionClose => {
                            add_sourcemap(0);
                            writer.push_str("`;");
                            for _ in 0..(t.text.len() - 2) {
                                writer.push(' ');
                            }
                        }
                        // FIXME: fix missing EOI
                        Rule::LineTerminator => {
                            add_sourcemap(t.col);
                            first_lt_after_region_open = false;
                            *dst_line += 1;
                            writer.push_str("\n");
                        }
                        _ => {
                            add_sourcemap(t.col);
                            writer.push_str(t.text.as_str());
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
