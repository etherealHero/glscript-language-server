use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use async_lsp::lsp_types as lsp;
use async_lsp::lsp_types::Url as Uri;

use sourcemap::{SourceMap, SourceMapBuilder, Token};

use crate::parser::Rule;
use crate::proxy::PROXY_WORKSPACE;
use crate::state::{Source, SourcePath, State, ToSource, ToSourcePath};

pub const BUILD_FILE: &'static str = "build.js.emitted";

#[cfg(debug_assertions)]
pub const BUILD_SOURCEMAP_FILE: &'static str = "build.js.emitted.map";

const DECL_PREFIX: &'static str = "/** @typedef";
const LINK_PREFIX: &'static str = "/** {@link ";
pub const MODULE_PREFIX: &'static str = "$MODULE_";

#[derive(Clone, Debug)]
pub struct Build {
    pub emit_text: String,
    pub emit_uri: Uri,
    pub emit_hash: u64,
    project: SourcePath,
    source_map: SourceMap,
}

impl Build {
    pub fn sources(&self) -> HashSet<Source> {
        self.source_map.sources().map(String::from).collect()
    }

    pub fn forward_src_position(
        &self,
        pos: &lsp::Position,
        pos_source: &Uri,
    ) -> Option<lsp::Position> {
        let mut token: Option<Token> = None;
        let source = &pos_source.source_path().source(&self.project);

        if !self.sources().contains(source) {
            return None;
        }

        for t in self.source_map.tokens() {
            let token_source = t.get_source();
            if token_source.is_none() || source != token_source.expect("has source") {
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

    pub fn forward_src_range(&self, range: &lsp::Range, range_source: &Uri) -> Option<lsp::Range> {
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
                let source_uri =
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
        skip(state, uri),
        fields(file = %uri.as_str().split("/").last().unwrap_or_default())
    )]
    pub fn new(state: &State, uri: &Uri) -> anyhow::Result<Self> {
        let mut smb = SourceMapBuilder::new(None);
        let visited_sources = &mut HashSet::<Source>::with_capacity(100);
        let emit_hasher = &mut DefaultHasher::new();
        let project = state.get_project();
        let global_doc = state.get_global_doc();
        let emit_text = Self::emit(
            state,
            uri,
            global_doc.as_ref(),
            project,
            &mut smb,
            &mut 0,
            visited_sources,
            emit_hasher,
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
        let source_hash =
            state.source_to_hash(&state.source_path_to_source(&state.uri_to_source_path(uri)?)?);
        let source_path = project
            .join(PROXY_WORKSPACE)
            .join(format!("{source_hash}.js",));
        let emit_uri = Uri::from_file_path(source_path).expect("valid build uri");

        source_hash.hash(emit_hasher);

        let b = Self {
            emit_uri,
            emit_text,
            emit_hash: emit_hasher.finish(),
            project: project.to_owned(),
            source_map,
        };

        Ok(b)
    }
}

impl Build {
    // TODO: rewrite with EmitConfig
    fn emit(
        state: &State,
        uri: &Uri,
        global_doc: Option<&Uri>,
        project: &SourcePath,
        smb: &mut SourceMapBuilder,
        dst_line: &mut u32,
        visited: &mut HashSet<Source>,
        hasher: &mut impl Hasher,
    ) -> anyhow::Result<String> {
        let path = &state.uri_to_source_path(uri)?;
        let source = &state.source_path_to_source(path)?;

        assert!(path.is_file());

        match visited.contains(source) {
            true => return Ok("".into()), // must be empty for emit global doc
            false => visited.insert(source.clone()),
        };

        let global_text = if let Some(doc) = global_doc {
            let global = Some(doc);
            Self::emit(state, doc, global, project, smb, dst_line, visited, hasher)
        } else {
            Ok("".to_owned())
        };

        // TODO: ? append context prefix with root entry uri if definition failed with other builds
        let source_file_decl = state.source_to_hash(source);
        let mut emitted_source_file = String::from(global_text.unwrap_or_default());

        // /** @typedef {'%source_file_decl%'} */{};\n
        emitted_source_file.push_str(DECL_PREFIX);
        emitted_source_file.push_str(" {'");
        emitted_source_file.push_str(source.as_str());
        emitted_source_file.push_str("'} ");
        emitted_source_file.push_str(MODULE_PREFIX);
        emitted_source_file.push_str(source_file_decl.as_str());
        emitted_source_file.push_str(" */{};\n");

        source_file_decl.hash(hasher);
        smb.add(*dst_line, 0, 0, 0, Some(source), None, false);
        *dst_line += 1; // prepend_module end breakline

        let source_tokens = state.get_doc_tokens(uri);
        let source_tokens = source_tokens.iter();
        let mut skip_lt_after_region_open = false;
        let mut first_lt_after_region_open = false;
        let mut first_lt_after_region_open_offset = 0;

        for t in source_tokens {
            match t.rule {
                Rule::IncludeToken => {}
                Rule::IncludePath => {
                    let (line, col) = (t.line, t.col);
                    let dep_rpath = t.text.trim_matches(|c| ['\'', '"', '<', '>'].contains(&c));
                    let dep_path = Self::resolve_path(path, project, dep_rpath);
                    let dep_uri = &Uri::from_file_path(&dep_path).expect("valid dep_path");
                    let dep_source = match state.uri_to_source_path(dep_uri) {
                        Ok(dep_sp) => state.source_path_to_source(&dep_sp).expect("dep is source"),
                        _ => dep_rpath.to_string(),
                    };
                    let dep_decl = state.source_to_hash(&dep_source);
                    let prefix = LINK_PREFIX.len() as u32;
                    let suffix = prefix + MODULE_PREFIX.len() as u32 + dep_decl.len() as u32;

                    dep_decl.hash(hasher);
                    line.hash(hasher);
                    col.hash(hasher);

                    // \n/** {@link %dep_ident%} */{};\n\n
                    emitted_source_file.push_str("\n");
                    emitted_source_file.push_str(LINK_PREFIX);
                    emitted_source_file.push_str(MODULE_PREFIX);
                    emitted_source_file.push_str(&dep_decl);
                    emitted_source_file.push_str("} */{};\n\n");

                    *dst_line += 1;
                    smb.add(*dst_line, prefix, line, col, Some(source), None, false);
                    smb.add(*dst_line, suffix, 0, 0, None, None, false);
                    *dst_line += 2;

                    let dep_build = Self::emit(
                        state, dep_uri, global_doc, project, smb, dst_line, visited, hasher,
                    );

                    if let Ok(ref emitted_dep_text) = dep_build {
                        let blk = " ".repeat(col as usize + t.text.len());
                        emitted_source_file.push_str(emitted_dep_text);
                        emitted_source_file.push_str(blk.as_str());
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
                            let escaped_backtick = " ".repeat(t.text.len() - 1) + "`";
                            add_sourcemap(0);
                            skip_lt_after_region_open = true;
                            first_lt_after_region_open_offset = escaped_backtick.len() as u32;
                            emitted_source_file.push_str(escaped_backtick.as_str());
                        }
                        Rule::RegionClose => {
                            add_sourcemap(0);
                            emitted_source_file.push_str("`;");
                            emitted_source_file.push_str(" ".repeat(t.text.len() - 2).as_str());
                        }
                        Rule::LineTerminator | Rule::EOI => {
                            add_sourcemap(t.col);
                            first_lt_after_region_open = false;
                            *dst_line += 1;
                            emitted_source_file.push_str("\n");
                        }
                        _ => {
                            add_sourcemap(t.col);
                            emitted_source_file.push_str(t.text.as_str());
                        }
                    }
                }
            }
        }

        Ok(emitted_source_file)
    }

    #[inline]
    fn resolve_path(module_path: &Path, project_root: &Path, include_literal: &str) -> PathBuf {
        #[inline]
        fn is_relative_path(path: &str) -> bool {
            path.starts_with("./")
                || path.starts_with(".\\")
                || path.starts_with("../")
                || path.starts_with("..\\")
        }

        #[inline]
        fn normalize_path(path: &Path) -> PathBuf {
            let mut buf = PathBuf::new();
            for component in path.components() {
                match component {
                    std::path::Component::ParentDir => {
                        buf.pop().eq(&false).then(|| buf.push(".."));
                    }
                    std::path::Component::CurDir => {}
                    _ => buf.push(component.as_os_str()),
                }
            }
            buf
        }

        let path = include_literal.replace("\\\\", "/").replace("\\", "/");

        if is_relative_path(&path) {
            let module_dir = module_path.parent().unwrap_or(project_root);
            normalize_path(&module_dir.join(path))
        } else {
            normalize_path(&project_root.join(path))
        }
    }

    // TODO: move from lib & refactor state to
    // DashMap<Uri, Struct {source_path, source, source_hash(u64)?}>
    #[inline]
    #[allow(unused)]
    pub fn fnv_hash(text: &str) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET;

        for byte in text.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }

        hash
    }
}
