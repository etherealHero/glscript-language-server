use async_lsp::lsp_types::{SemanticTokens, request as R};
use async_lsp::{LanguageServer, lsp_types as lsp};
use derive_more::Constructor;
use rayon::prelude::*;

use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::state::State;
use crate::try_ensure_transpile;
use crate::types::Document;

// TODO: add %param str injection, mono-highlight regions (with provided option)
/// wiki:
/// - <https://pygls.readthedocs.io/en/latest/protocol/howto/interpret-semantic-tokens.html>
/// - [`lsp::SemanticTokens`] on prop `data`
#[tracing::instrument(skip_all)]
pub fn proxy_semantic_tokens_full(
    this: &mut Proxy,
    mut params: lsp::SemanticTokensParams,
) -> ResFut<R::SemanticTokensFullRequest> {
    let mut s = this.server();
    let uri = &params.text_document.uri;
    let transpile = try_ensure_transpile!(this, uri, params, semantic_tokens_full);
    let st = this.state.clone();
    let extra_tokens = extra_tokens(st.get_doc(uri).unwrap(), &st);

    params.text_document.uri = transpile.uri.clone();

    Box::pin(async move {
        let res = s.semantic_tokens_full(params).await;
        let res = res.map_err(Error::internal);

        type SR = lsp::SemanticTokensResult;
        let Ok(Some(SR::Tokens(SemanticTokens { result_id, data }))) = res else {
            return Err(Error::forward_failed());
        };

        let tokens = decode(data);
        let source_tokens = tokens.into_par_iter().filter_map(|t| {
            let end = lsp::Position::new(t.range.0.line, t.range.1);
            let mut range = lsp::Range::new(t.range.0, end);
            forward_build_range(&mut range, &transpile).ok()?;
            let range = (range.start, range.end.character);
            let token = AbsoluteSemanticToken::new(range, t.token_type, t.token_modifiers_bitset);
            Some(token)
        });
        let source_tokens = source_tokens.collect();
        let source_tokens = enrich_tokens(source_tokens, extra_tokens);

        // tracing::info!("source_tokens: {:#?}", source_tokens);
        // tracing::info!("token_types: {:#?}", st.get_token_types_capabilities());

        let data = encode(source_tokens);
        let semantic_tokens = SemanticTokens { result_id, data };
        Ok(Some(SR::Tokens(semantic_tokens)))
    })
}

fn extra_tokens(doc: Document, st: &State) -> Vec<AbsoluteSemanticToken> {
    let Some(token_types) = st.get_token_types_capabilities() else {
        return vec![];
    };

    let Some(id) = token_types
        .iter()
        .enumerate()
        .find(|(_, t)| **t == lsp::SemanticTokenType::PARAMETER)
        .map(|e| e.0 as u32)
    else {
        return vec![];
    };

    doc.parse
        .str_lit_injections
        .iter()
        .map(|t| AbsoluteSemanticToken::new((lsp::Position::new(t.line, t.col), t.col + 2), id, 0))
        .collect()
}

fn enrich_tokens(
    mut this: Vec<AbsoluteSemanticToken>,
    other: Vec<AbsoluteSemanticToken>,
) -> Vec<AbsoluteSemanticToken> {
    let non_intersect_other = other.into_par_iter().filter_map(|o| {
        let r_o = lsp::Range::new(o.range.0, lsp::Position::new(o.range.0.line, o.range.1));
        let intersect = this.iter().any(|t| {
            let r_a = lsp::Range::new(t.range.0, lsp::Position::new(t.range.0.line, t.range.1));
            r_o.start <= r_a.end && r_a.start <= r_o.end
        });
        if intersect { None } else { Some(o) }
    });

    this.append(&mut non_intersect_other.collect::<Vec<_>>());
    this.sort_by(|a, b| a.range.0.cmp(&b.range.0));
    this
}

/// each pos has one line because semantic token cannot be multiline
type StartPosWithEndCharacter = (lsp::Position, u32); // start pos & end_character

#[derive(Constructor, Debug)]
struct AbsoluteSemanticToken {
    range: StartPosWithEndCharacter,
    token_type: u32,
    token_modifiers_bitset: u32,
}

fn decode(tokens: Vec<lsp::SemanticToken>) -> Vec<AbsoluteSemanticToken> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut cur_line: u32 = 0;
    let mut cur_char: u32 = 0;

    for t in tokens {
        cur_line += t.delta_line;

        match t.delta_line == 0 {
            true => cur_char += t.delta_start,
            false => cur_char = t.delta_start,
        }

        let start = lsp::Position::new(cur_line, cur_char);
        let end_character = cur_char + t.length;

        result.push(AbsoluteSemanticToken::new(
            (start, end_character),
            t.token_type,
            t.token_modifiers_bitset,
        ));
    }

    result
}

fn encode(tokens: Vec<AbsoluteSemanticToken>) -> Vec<lsp::SemanticToken> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut prev_line: u32 = 0;
    let mut prev_char: u32 = 0;

    for t in tokens {
        let start = &t.range.0;
        let end_character = t.range.1;

        let delta_line = start.line - prev_line;
        let delta_start = match delta_line == 0 {
            true => start.character - prev_char,
            false => start.character,
        };

        let length = end_character - start.character;

        result.push(lsp::SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: t.token_type,
            token_modifiers_bitset: t.token_modifiers_bitset,
        });

        prev_line = start.line;
        prev_char = start.character;
    }

    result
}
