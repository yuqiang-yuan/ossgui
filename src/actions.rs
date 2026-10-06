use gpui_kit::{Action, actions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

actions!(ossgui, [QuitAction, AboutAction, CopyAction, CutAction, PasteAction, DeleteAction]);
