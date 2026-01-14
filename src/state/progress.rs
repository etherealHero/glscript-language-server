use async_lsp::ClientSocket;
use async_lsp::LanguageClient;
use async_lsp::lsp_types as lsp;

use crate::state::State;

impl State {
    pub async fn create_progress(&self, client: &mut ClientSocket) {
        if self.work_done_progress_present.load() {
            return;
        };

        let token = self.work_done_progress_token.get().unwrap().clone();
        let params = lsp::WorkDoneProgressCreateParams {
            token: token.clone(),
        };

        if let Err(err) = client.work_done_progress_create(params).await {
            tracing::error!("{err}");
        } else {
            self.work_done_progress_present.store(true);
            client
                .progress(lsp::ProgressParams {
                    token: token.clone(),
                    value: lsp::ProgressParamsValue::WorkDone(lsp::WorkDoneProgress::Begin(
                        lsp::WorkDoneProgressBegin {
                            title: "glscript".to_string(),
                            ..lsp::WorkDoneProgressBegin::default()
                        },
                    )),
                })
                .unwrap();
        }
    }

    pub fn send_progress(
        &self,
        client: &mut ClientSocket,
        idx_and_size: (usize, usize),
        msg: &str,
    ) {
        if self.work_done_progress_present.load() {
            let (idx, size) = idx_and_size;
            let percentage = Some((idx as f32 / 100.0 * size as f32) as u32);
            let message = match (idx, size) == (0, 0) {
                true => msg.to_string(),
                false => format!("{idx}/{size} {msg}"),
            };
            let _ = client.progress(lsp::ProgressParams {
                token: self.work_done_progress_token.get().unwrap().clone().clone(),
                value: lsp::ProgressParamsValue::WorkDone(lsp::WorkDoneProgress::Report(
                    lsp::WorkDoneProgressReport {
                        cancellable: None,
                        message: message.into(),
                        percentage,
                    },
                )),
            });
        }
    }

    pub fn destroy_progress(&self, client: &mut ClientSocket) {
        if self.work_done_progress_present.load() {
            self.work_done_progress_present.store(false);
            let _ = client.progress(lsp::ProgressParams {
                token: self.work_done_progress_token.get().unwrap().clone(),
                value: lsp::ProgressParamsValue::WorkDone(lsp::WorkDoneProgress::End(
                    lsp::WorkDoneProgressEnd::default(),
                )),
            });
        }
    }
}
