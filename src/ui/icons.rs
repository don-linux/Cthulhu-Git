//! Lucide interface icons (`assets/icons/`, ISC license). `build.rs` renders
//! each SVG white; drawing tints it with a palette color, so the icons follow
//! the theme.

use eframe::egui::{
    Button, Color32, ColorImage, Context, CursorIcon, Frame, Id, Image, Painter, Pos2, Rect,
    Response, RichText, TextStyle, TextureHandle, TextureOptions, Tooltip, Ui, Vec2,
};

/// Side of the textures written by `build.rs` (`UI_ICON_SIZE`).
const TEXTURE_SIZE: usize = 64;

/// Side an icon is drawn at, in points.
const ICON_SIZE: f32 = 18.0;

/// Space between an icon button and its hover label. egui's own tooltip sits
/// 4px away, close enough to read as part of the bar it came from.
const HOVER_TIP_GAP: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    PanelLeft,
    PanelRight,
    House,
    GitBranch,
    Terminal,
    Settings,
    ArrowLeft,
    CloudDownload,
    ArrowDownToLine,
    CircleUserRound,
    KeyRound,
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
            Self::CloudDownload => "cloud-download",
            Self::ArrowDownToLine => "arrow-down-to-line",
            Self::CircleUserRound => "circle-user-round",
            Self::KeyRound => "key-round",
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
            Self::CloudDownload => {
                include_bytes!(concat!(env!("OUT_DIR"), "/icons/cloud-download.rgba"))
            }
            Self::ArrowDownToLine => {
                include_bytes!(concat!(env!("OUT_DIR"), "/icons/arrow-down-to-line.rgba"))
            }
            Self::CircleUserRound => {
                include_bytes!(concat!(env!("OUT_DIR"), "/icons/circle-user-round.rgba"))
            }
            Self::KeyRound => include_bytes!(concat!(env!("OUT_DIR"), "/icons/key-round.rgba")),
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
///
/// The rect is the hovered size from the first frame, so the bar around it
/// does not grow when the pointer arrives.
pub fn icon_button(ui: &mut Ui, icon: Icon, tint: Color32, hover_text: &str) -> Response {
    let response = ui.add(
        Button::image(icon.image(ui.ctx(), tint))
            .frame_when_inactive(false)
            .min_size(icon_button_min_size(ui)),
    );
    show_hover_tip(ui, &response, hover_text);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

pub fn icon(ui: &mut Ui, icon: Icon, tint: Color32) -> Response {
    ui.add(icon.image(ui.ctx(), tint))
}

/// Draws `icon` into `rect`, tinted. Used where the icon sits inside a painted
/// row rather than as its own widget.
pub fn paint_icon(painter: &Painter, ctx: &Context, rect: Rect, icon: Icon, tint: Color32) {
    let texture = icon.texture(ctx);
    painter.image(
        texture.id(),
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        tint,
    );
}

/// Icon with a word beside it. The whole control is one click, and the frame
/// only shows while hovered or pressed. A disabled control keeps the word and
/// does not take the click.
pub fn icon_text_button(
    ui: &mut Ui,
    icon: Icon,
    text: &str,
    tint: Color32,
    hover_text: &str,
    enabled: bool,
) -> Response {
    let mut response = ui.add_enabled(
        enabled,
        Button::image_and_text(icon.image(ui.ctx(), tint), RichText::new(text).color(tint))
            .frame_when_inactive(false)
            .min_size(icon_text_min_size(ui, text)),
    );
    show_hover_tip(ui, &response, hover_text);
    if enabled {
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    response
}

/// Width [`icon_text_button`] occupies, so a neighbor can stop before it.
pub fn icon_text_button_width(ui: &Ui, text: &str) -> f32 {
    icon_text_min_size(ui, text).x
}

/// Hovered outer size of an icon button.
///
/// egui's full frame totals `button_padding` per side. With
/// `frame_when_inactive(false)` the inactive path drops the stroke after
/// `button_style` already subtracted it from `inner_margin`. A 1px inactive
/// stroke then makes the button, and the bar around it, grow on hover.
fn icon_button_min_size(ui: &Ui) -> Vec2 {
    Vec2::splat(ICON_SIZE) + ui.spacing().button_padding * 2.0
}

/// Same floor as [`icon_button_min_size`], plus the label.
///
/// The extra inactive stroke on the width is the amount hover would add back
/// if the measured label is a hair narrower than the button's atoms. Fetch
/// and Pull then stay put instead of pushing the branch name.
fn icon_text_min_size(ui: &Ui, text: &str) -> Vec2 {
    let font = TextStyle::Button.resolve(ui.style());
    let text_size = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
        .rect
        .size();
    let content = Vec2::new(
        ICON_SIZE + ui.spacing().icon_spacing + text_size.x,
        ICON_SIZE.max(text_size.y),
    );
    let padding = ui.spacing().button_padding;
    let stroke = ui.visuals().widgets.inactive.bg_stroke.width;
    Vec2::new(
        content.x + padding.x * 2.0 + stroke * 2.0,
        content.y + padding.y * 2.0,
    )
}

fn show_hover_tip(ui: &Ui, response: &Response, text: &str) {
    let mut tip = Tooltip::for_enabled(response).gap(HOVER_TIP_GAP);
    tip.popup = tip.popup.frame(hover_tip_frame(ui));
    tip.show(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(text);
    });
}

fn hover_tip_frame(ui: &Ui) -> Frame {
    // `Frame::popup` fills with `window_fill`, and this theme sets that to the
    // same surface as the bars. The field background keeps the label separate.
    Frame::popup(ui.style()).fill(ui.visuals().extreme_bg_color)
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
            Icon::CloudDownload,
            Icon::ArrowDownToLine,
            Icon::CircleUserRound,
            Icon::KeyRound,
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
