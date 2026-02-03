use async_lsp::lsp_types::request as R;
use async_lsp::{LanguageServer, lsp_types as lsp};
use tokio::time::{Duration, timeout};

use crate::proxy::language_server::{DefRes, Error, definition_params, forward_build_range};
use crate::proxy::{Canonicalize, DECL_FILE_EXT, Proxy, ResFut, ResReqProxy};
use crate::types::SCRIPT_IDENTIFIER_PREFIX;
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

        let hover = service.hover(params).await.map_err(Error::internal)?;
        if hover.is_none() {
            return Ok(None);
        }

        let (stripped, mut hover) = strip_module_hash(hover.unwrap());

        if let Some(mut r) = hover.range
            && !forward_build_range(&mut r, &bundle).is_ok_and(|source| source == *req_source)
        {
            hover.range = None
        }

        // TODO: skip awaiting decl on empty hover. ^^^ Check hover.is_none()
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
            let source = path.strip_prefix(root).unwrap_or(&path).display();

            hover = match path.to_str().unwrap().ends_with(DECL_FILE_EXT) {
                true => prepend_hover(hover, "Built-in symbol"),
                false => match state.get_default_sources().contains(&path) {
                    true => prepend_hover(hover, &format!("**Default** included by {source}")),
                    false => prepend_hover(hover, &format!("Included by {source}")),
                },
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
