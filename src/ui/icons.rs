//! Lucide interface icons (`assets/icons/`, ISC license). `build.rs` renders
//! each SVG white; drawing tints it with a palette color, so the icons follow
//! the theme.

use eframe::egui::{
    Button, Color32, ColorImage, Context, CursorIcon, Id, Image, Response, TextureHandle,
    TextureOptions, Ui, Vec2,
};

/// Side of the textures written by `build.rs` (`UI_ICON_SIZE`).
const TEXTURE_SIZE: usize = 64;

/// Side an icon is drawn at, in points.
const ICON_SIZE: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    PanelLeft,
    PanelRight,
    House,
    GitBranch,
    Terminal,
    Settings,
    ArrowLeft,
}

impl Icon {
    /// The file name under `assets/icons/`, without `.svg`.
    fn name(self) -> &'static str {
        match self {
            Self::PanelLeft => "panel-left",
            Self::PanelRight => "panel-right",
            Self::House => "house",
            Self::GitBranch => "git-branch",
            Self::Terminal => "terminal",
            Self::Settings => "settings",
            Self::ArrowLeft => "arrow-left",
        }
    }

    fn rgba(self) -> &'static [u8] {
        match self {
            Self::PanelLeft => include_bytes!(concat!(env!("OUT_DIR"), "/icons/panel-left.rgba")),
            Self::PanelRight => include_bytes!(concat!(env!("OUT_DIR"), "/icons/panel-right.rgba")),
            Self::House => include_bytes!(concat!(env!("OUT_DIR"), "/icons/house.rgba")),
            Self::GitBranch => include_bytes!(concat!(env!("OUT_DIR"), "/icons/git-branch.rgba")),
            Self::Terminal => include_bytes!(concat!(env!("OUT_DIR"), "/icons/terminal.rgba")),
            Self::Settings => include_bytes!(concat!(env!("OUT_DIR"), "/icons/settings.rgba")),
            Self::ArrowLeft => include_bytes!(concat!(env!("OUT_DIR"), "/icons/arrow-left.rgba")),
        }
    }

    /// Uploaded to the GPU on first use and kept for the life of the context.
    fn texture(self, ctx: &Context) -> TextureHandle {
        let id = Id::new(("cthulhu-git-icon", self.name()));
        if let Some(texture) = ctx.data(|data| data.get_temp::<TextureHandle>(id)) {
            return texture;
        }
        let image = ColorImage::from_rgba_unmultiplied([TEXTURE_SIZE, TEXTURE_SIZE], self.rgba());
        let texture = ctx.load_texture(
            format!("icon-{}", self.name()),
            image,
            TextureOptions::LINEAR,
        );
        ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
        texture
    }

    fn image(self, ctx: &Context, tint: Color32) -> Image<'static> {
        Image::new(&self.texture(ctx))
            .fit_to_exact_size(Vec2::splat(ICON_SIZE))
            .tint(tint)
    }
}

/// An icon that only shows a frame while hovered or pressed.
pub fn icon_button(ui: &mut Ui, icon: Icon, tint: Color32, hover_text: &str) -> Response {
    ui.add(Button::image(icon.image(ui.ctx(), tint)).frame_when_inactive(false))
        .on_hover_text(hover_text)
        .on_hover_cursor(CursorIcon::PointingHand)
}

pub fn icon(ui: &mut Ui, icon: Icon, tint: Color32) -> Response {
    ui.add(icon.image(ui.ctx(), tint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_icons_have_the_texture_size() {
        for icon in [
            Icon::PanelLeft,
            Icon::PanelRight,
            Icon::House,
            Icon::GitBranch,
            Icon::Terminal,
            Icon::Settings,
            Icon::ArrowLeft,
        ] {
            assert_eq!(
                icon.rgba().len(),
                TEXTURE_SIZE * TEXTURE_SIZE * 4,
                "{}",
                icon.name()
            );
        }
    }
}
