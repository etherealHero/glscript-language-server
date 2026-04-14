use std::str::FromStr;

use async_lsp::lsp_types::{Url as Uri, request as R};
use async_lsp::{LanguageServer, lsp_types as lsp};
use tokio::time::{Duration, timeout};

use crate::proxy::language_server::{DefRes, definition_params};
use crate::proxy::{Canonicalize, DECL_FILE_EXT, Proxy, ResFut, ResReqProxy};
use crate::proxy::{Error, forward_build_range};

use crate::state::State;
use crate::types::{SCRIPT_IDENTIFIER_PREFIX, Source};
use crate::{try_ensure_bundle, try_forward_text_document_position_params};

pub fn proxy_hover_with_decl_info(
    this: &mut Proxy,
    mut params: lsp::HoverParams,
) -> ResFut<R::HoverRequest> {
    let mut service = this.server();
    let uri = &params.text_document_position_params.text_document.uri;
    let pos = &params.text_document_position_params.position;
    let bundle = try_ensure_bundle!(this, uri, params, hover);

    // TODO: send cancel req on timeout
    let decl_req = this.definition(definition_params(uri.clone(), pos.to_owned()));
    let state = this.state.clone();
    let req_source = state.get_doc(uri).unwrap().source.clone();
    let req_uri = uri.clone();

    Box::pin(async move {
        let doc_pos = &mut params.text_document_position_params;
        try_forward_text_document_position_params!(state, bundle, doc_pos);

        let Some(hover) = service.hover(params).await.map_err(Error::internal)? else {
            return Ok(None);
        };

        let (stripped, hover) = strip_module_hash(hover);
        let mut hover = forward_md_links(hover, &state).map_err(|_| Error::forward_failed())?;

        if let Some(mut r) = hover.range
            && !forward_build_range(&mut r, &bundle).is_ok_and(|source| source == *req_source)
        {
            hover.range = None
        }

        let decl: ResReqProxy<R::GotoDefinition> = timeout(Duration::from_millis(200), decl_req)
            .await
            .unwrap_or(Ok(None));

        if matches!(decl, Ok(Some(DefRes::Link(ref l))) if l.is_empty()) {
            let msg = "⚠ No definiion available for this item.";
            return Ok(Some(prepend_hover(hover, msg)));
        }

        if let Ok(Some(DefRes::Link(ref l))) = decl {
            let res_uri = &l.first().unwrap().target_uri;
            let is_local = || req_uri.try_canonicalize() == res_uri.try_canonicalize();

            if stripped || is_local() {
                return Ok(Some(hover));
            }

            let path = state.uri_to_path(res_uri).unwrap();
            let root = state.get_project();

            hover = match path.to_str().unwrap().ends_with(DECL_FILE_EXT) {
                true => prepend_hover(hover, "Built-in symbol"),
                false => {
                    if let Ok(source) = Source::from_path(&path, root) {
                        let prettify = |s: &Source, with_pos: bool| {
                            let def_pos = if with_pos {
                                let start = l.first().unwrap().target_range.start;
                                format!("#L{},{}", start.line + 1, start.character + 1) // zero-based
                            } else {
                                "".to_string()
                            };

                            let path_to_md_link = |p: std::path::PathBuf| {
                                Some(format!(
                                    "[{}]({}{def_pos})",
                                    p.file_stem().unwrap().to_str().unwrap(),
                                    Uri::from_file_path(p.clone()).unwrap().as_str()
                                ))
                            };

                            Uri::from_file_path(root.join(s.as_str()))
                                .ok()
                                .and_then(|uri| state.uri_to_path(&uri).ok())
                                .and_then(path_to_md_link)
                                .unwrap_or(s.as_str().to_string())
                        };

                        let stack = bundle.sources_with_include_stack.get(&source).unwrap();
                        let stack_last_idx = stack.len() - 1;

                        let stack = stack
                            .iter()
                            .enumerate()
                            .map(|e| prettify(e.1, e.0 == stack_last_idx))
                            .collect::<Vec<String>>()
                            .join(" -> ");

                        prepend_hover(hover, &format!("Included from this file -> {stack}"))
                    } else {
                        let raw_source = path.strip_prefix(root).unwrap_or(&path).display();

                        prepend_hover(hover, &format!("Included from this file -> {raw_source}"))
                    }
                }
            };
        }

        Ok(Some(hover))
    })
}

fn prepend_hover(mut hover: lsp::Hover, msg: &str) -> lsp::Hover {
    type H = lsp::HoverContents;
    type S = lsp::MarkedString;

    match &mut hover.contents {
        H::Scalar(S::String(s)) => {
            let mut new = msg.to_string();
            new.push_str("\n\n");
            new.push_str(s.clone().as_str());
            *s = new;
        }
        H::Scalar(S::LanguageString(s)) => s.value = format!("{msg}\n\n{}", s.value),
        H::Array(ms) => ms.insert(0, S::String(msg.to_string())),
        H::Markup(m) => m.value = format!("{msg}\n\n{}", m.value),
    };

    hover
}

fn strip_module_hash(mut hover: lsp::Hover) -> (bool, lsp::Hover) {
    type H = lsp::HoverContents;
    type S = lsp::MarkedString;

    let mut any_matched = false;
    let ident_prefix = regex::escape(SCRIPT_IDENTIFIER_PREFIX);
    let re = regex::Regex::new(&format!(r"{ident_prefix}\w+")).unwrap();
    let strings: Vec<&mut String> = match &mut hover.contents {
        H::Scalar(S::String(s)) => vec![s],
        H::Scalar(S::LanguageString(s)) => vec![&mut s.value],
        H::Array(items) => items
            .iter_mut()
            .map(|item| match item {
                S::String(s) => s,
                S::LanguageString(ls) => &mut ls.value,
            })
            .collect(),
        H::Markup(m) => vec![&mut m.value],
    };

    for s in strings {
        let cow = re.replace_all(s, "ScriptFile");
        any_matched |= matches!(cow, std::borrow::Cow::Owned(_));
        if let std::borrow::Cow::Owned(v) = cow {
            *s = v;
        }
    }

    (any_matched, hover)
}

fn forward_md_links(mut hover: lsp::Hover, st: &State) -> anyhow::Result<lsp::Hover> {
    type H = lsp::HoverContents;
    type S = lsp::MarkedString;

    let strings: Vec<&mut String> = match &mut hover.contents {
        H::Scalar(S::String(s)) => vec![s],
        H::Scalar(S::LanguageString(s)) => vec![&mut s.value],
        H::Array(items) => items
            .iter_mut()
            .map(|item| match item {
                S::String(s) => s,
                S::LanguageString(ls) => &mut ls.value,
            })
            .collect(),
        H::Markup(m) => vec![&mut m.value],
    };

    let re = regex::Regex::new(r"(file:///.+?\..+?\.js)#L(\d+)(%2C|,)(\d+)").unwrap();
    let project = st.get_project();

    for s in strings {
        let cow = re.replace_all(s, |caps: &regex::Captures| {
            let emit_uri_literal = caps.get(1).unwrap().as_str();
            let line_str = caps.get(2).unwrap().as_str();
            let sep = caps.get(3).unwrap().as_str();
            let col_str = caps.get(4).unwrap().as_str();

            let line = line_str.parse::<u32>().unwrap_or(1).saturating_sub(1); // LSP lines 0-based
            let character = col_str.parse::<u32>().unwrap_or(0);

            match Uri::from_str(emit_uri_literal) {
                Ok(emit_uri) => {
                    if let Some(any_build) = st.get_any_build_by_emit_uri(&emit_uri) {
                        let pos = lsp::Position::new(line, character);

                        if let Some((source_pos, source)) = any_build.forward_build_position(&pos) {
                            let path = project.join(source.as_str());
                            if let Ok(source_uri) = st.path_to_uri(&path) {
                                return format!(
                                    "{}#L{}{}{}",
                                    source_uri.as_str(),
                                    source_pos.line + 1,
                                    sep,
                                    source_pos.character
                                );
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("Failed to parse emit URI from hover: {}", e),
            }

            caps.get(0).unwrap().as_str().to_string()
        });

        if let std::borrow::Cow::Owned(v) = cow {
            *s = v;
        }
    }

    Ok(hover)
}
