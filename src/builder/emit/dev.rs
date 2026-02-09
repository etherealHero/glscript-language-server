#[cfg(debug_assertions)]
pub fn emit_on_disk(
    opt: &crate::builder::options_builder::BuildOptions<'_>,
    doc: &crate::types::Document,
    source_map: &sourcemap::SourceMap,
    content: &String,
) -> Result<(), anyhow::Error> {
    use crate::{builder::EMIT_FILE_EXT, proxy::PROXY_WORKSPACE};
    use base64::prelude::{BASE64_STANDARD, Engine as _};

    let mut sm_json = Vec::new();
    let _ = source_map.to_writer(&mut sm_json);
    let sm_base64 = BASE64_STANDARD.encode(&sm_json);
    let build = format!(
        "{}\n//# sourceMappingURL=data:application/json;base64,{}",
        &content, sm_base64
    );
    let debug_source = match opt.resolve_deps {
        true => doc.source.to_string() + EMIT_FILE_EXT,
        false => doc.source.to_string() + ".transpiled" + EMIT_FILE_EXT,
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
