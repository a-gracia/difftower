use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

// Lucide icons beyond the default set that gpui-kit bundles.
gpui_kit::assets::icon_assets!(ExtraIcons, [Shield, ShieldAlert, ShieldCheck, GitCompare, Columns2, Server]);

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
