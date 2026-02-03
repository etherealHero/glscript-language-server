use async_lsp::lsp_types::{SemanticTokens, request as R};
use async_lsp::{LanguageServer, lsp_types as lsp};
use derive_more::Constructor;
use rayon::prelude::*;

use crate::proxy::language_server::{Error, forward_build_range};
use crate::proxy::{Proxy, ResFut};
use crate::try_ensure_transpile;

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

        let data = encode(source_tokens.collect());
        tracing::info!("semantic_tokens({})", data.len());
        let semantic_tokens = SemanticTokens { result_id, data };
        Ok(Some(SR::Tokens(semantic_tokens)))
    })
}

/// each pos has one line because semantic token cannot be multiline
type StartPosWithEndCharacter = (lsp::Position, u32);

#[derive(Constructor)]
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
