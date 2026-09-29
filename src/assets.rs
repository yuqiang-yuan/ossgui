use gpui_kit::{AssetSource, SharedString};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "assets"]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(file) = AppAssets::get(path) {
            return Ok(Some(file.data));
        }

        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut files = AppAssets::iter()
            .filter(|f| f.starts_with(path))
            .map(SharedString::from)
            .collect::<Vec<_>>();
        files.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(files)
    }
}
