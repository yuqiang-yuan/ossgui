use gpui_kit::{Action, actions};
use schemars::JsonSchema;
use serde::Deserialize;

actions!(
    ossgui,
    [
        QuitAction,
        AboutAction,
        CopyAction,
        CutAction,
        PasteAction,
        DeleteAction,
        DownloadAction,
        OpenFilesForUploadAction,
        OpenFolderForUploadAction,
    ]
);

#[derive(Debug, Clone, PartialEq, Action, Deserialize, JsonSchema)]
pub struct DeleteAccountAction {
    pub id: String,
}
