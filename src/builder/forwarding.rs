use async_lsp::lsp_types::{self as lsp};
use std::collections::HashSet;

use crate::builder::Build;
use crate::types::Source;

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
