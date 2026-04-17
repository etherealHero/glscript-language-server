use async_lsp::lsp_types::Url as Uri;

use crate::builder::emit::{Context, Emit};
use crate::parser::Token;

impl Emit {
    /// called before parallel content & source_map emit tasks for sync sources
    #[cfg_attr(feature = "profiling", tracing::instrument(skip_all))]
    pub fn prepare_par_iter(ctx: &mut Context, target: &Uri) {
        Emit::_prepare_par_iter(ctx, target);
    }

    fn _prepare_par_iter(ctx: &mut Context, target: &Uri) {
        let Ok(d) = ctx.proxy_state.get_doc(target) else {
            return;
        };

        match ctx.visited_sources.contains(&d.source_hash) {
            false => ctx.visited_sources.insert(d.source_hash),
            true => return,
        };

        Emit::_prepare_par_iter(ctx, ctx.defult_document);

        for t in d.parse.compressed_tokens.iter() {
            if let Token::IncludePath(t) = t {
                let dep_path = ctx.proxy_state.path_resolver(&d.path, t.lit);
                if let Ok(target) = ctx.proxy_state.path_to_uri(&dep_path) {
                    Emit::_prepare_par_iter(ctx, &target);
                }
            }
        }
    }
}
