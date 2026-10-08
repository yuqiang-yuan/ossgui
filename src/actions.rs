use gpui_kit::actions;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

actions!(ossgui, [QuitAction, AboutAction, CopyAction, CutAction, PasteAction, DeleteAction]);
