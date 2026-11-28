//! Lucide interface icons (`assets/icons/`, ISC license). Each SVG is drawn
//! white; drawing tints it with a palette color, so the icons follow the
//! theme. `egui_extras` renders an SVG at the size and pixel density it is drawn
//! at, so the icons stay sharp at any display scale.
//!
//! [`app_logo`] is the application mark (`assets/icon.svg`), in color. It is
//! not one of the tinted interface icons.

use eframe::egui::emath::GuiRounding as _;
use eframe::egui::load::{SizeHint, TexturePoll};
use eframe::egui::{
    Button, Color32, Context, CursorIcon, Frame, Image, ImageSource, Painter, Pos2, Rect, Response,
    RichText, TextStyle, TextureOptions, Tooltip, Ui, Vec2, include_image,
};
use egui_extras::loaders::svg_loader::SvgLoader;

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
    Type,
    Settings,
    ArrowLeft,
    CloudDownload,
    ArrowDownToLine,
    CircleUserRound,
    KeyRound,
    Folder,
    GitHub,
    Info,
}

impl Icon {
    fn source(self) -> ImageSource<'static> {
        match self {
            Self::PanelLeft => include_image!("../../assets/icons/panel-left.svg"),
            Self::PanelRight => include_image!("../../assets/icons/panel-right.svg"),
            Self::House => include_image!("../../assets/icons/house.svg"),
            Self::GitBranch => include_image!("../../assets/icons/git-branch.svg"),
            Self::Terminal => include_image!("../../assets/icons/terminal.svg"),
            Self::Type => include_image!("../../assets/icons/type.svg"),
            Self::Settings => include_image!("../../assets/icons/settings.svg"),
            Self::ArrowLeft => include_image!("../../assets/icons/arrow-left.svg"),
            Self::CloudDownload => include_image!("../../assets/icons/cloud-download.svg"),
            Self::ArrowDownToLine => include_image!("../../assets/icons/arrow-down-to-line.svg"),
            Self::CircleUserRound => include_image!("../../assets/icons/circle-user-round.svg"),
            Self::KeyRound => include_image!("../../assets/icons/key-round.svg"),
            Self::Folder => include_image!("../../assets/icons/folder.svg"),
            Self::GitHub => include_image!("../../assets/icons/github.svg"),
            Self::Info => include_image!("../../assets/icons/info.svg"),
        }
    }

    fn image(self, ctx: &Context, tint: Color32) -> Image<'static> {
        install_svg_loader(ctx);
        Image::new(self.source())
            .fit_to_exact_size(Vec2::splat(ICON_SIZE))
            .tint(tint)
    }
}

/// Every drawing path installs `egui_extras`' SVG loader, so a `Ui` that did
/// not come through `CthulhuApp::new`, such as a test harness's, draws the
/// icons too.
fn install_svg_loader(ctx: &Context) {
    if !ctx.is_loader_installed(SvgLoader::ID) {
        egui_extras::install_image_loaders(ctx);
    }
}

/// An icon that only shows a frame while hovered or pressed.
///
/// The rect is the hovered size from the first frame, so the bar around it
/// does not grow when the pointer arrives.
pub fn icon_button(ui: &mut Ui, icon: Icon, tint: Color32, hover_text: &str) -> Response {
    icon_button_enabled(ui, icon, tint, hover_text, true)
}

/// Same as [`icon_button`]. A disabled control keeps the icon and does not take the click.
pub fn icon_button_enabled(
    ui: &mut Ui,
    icon: Icon,
    tint: Color32,
    hover_text: &str,
    enabled: bool,
) -> Response {
    let mut response = ui.add_enabled(
        enabled,
        Button::image(icon.image(ui.ctx(), tint))
            .frame_when_inactive(false)
            .min_size(icon_button_min_size(ui)),
    );
    // Nothing visible names an icon-only button, so the hover text does.
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(hover_text));
    show_hover_tip(ui, &response, hover_text);
    if enabled {
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    response
}

/// The tinted icon image, for a button built outside this module.
pub fn tinted(ui: &Ui, icon: Icon, tint: Color32) -> Image<'static> {
    icon.image(ui.ctx(), tint)
}

pub fn icon(ui: &mut Ui, icon: Icon, tint: Color32) -> Response {
    ui.add(icon.image(ui.ctx(), tint))
}

/// Draws `icon` into `rect`, tinted. Used where the icon sits inside a painted
/// row rather than as its own widget.
///
/// Rendered at the pixel size of `rect`, as `Image::paint_at` does, so it
/// stays sharp. The painter keeps the row's clip, which `paint_at` would not.
pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, tint: Color32) {
    let ctx = painter.ctx();
    install_svg_loader(ctx);
    let rect = rect.round_to_pixels(painter.pixels_per_point());
    let pixels = (rect.size() * painter.pixels_per_point()).round();
    let size_hint = SizeHint::Size {
        width: pixels.x as u32,
        height: pixels.y as u32,
        maintain_aspect_ratio: false,
    };
    if let Ok(TexturePoll::Ready { texture }) =
        icon.source()
            .load(ctx, TextureOptions::default(), size_hint)
    {
        painter.image(
            texture.id,
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            tint,
        );
    }
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

const LOGO: ImageSource<'static> = include_image!("../../assets/icon.svg");

/// Draws the application icon at `size` points on a side, unchanged.
pub fn app_logo(ui: &mut Ui, size: f32) -> Response {
    install_svg_loader(ui.ctx());
    ui.add(
        Image::new(LOGO)
            .fit_to_exact_size(Vec2::splat(size))
            .alt_text("Cthulhu-Git logo"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// resvg draws `currentColor` black, and a tint multiplies black into
    /// black, so the icons are drawn white for the tint to color them.
    #[test]
    fn no_svg_draws_with_current_color() {
        let icons = [
            Icon::PanelLeft,
            Icon::PanelRight,
            Icon::House,
            Icon::GitBranch,
            Icon::Terminal,
            Icon::Type,
            Icon::Settings,
            Icon::ArrowLeft,
            Icon::CloudDownload,
            Icon::ArrowDownToLine,
            Icon::CircleUserRound,
            Icon::KeyRound,
            Icon::Folder,
            Icon::GitHub,
            Icon::Info,
        ];
        for source in icons.map(Icon::source).into_iter().chain([LOGO]) {
            let ImageSource::Bytes { uri, bytes } = source else {
                panic!("{source:?} is not bundled");
            };
            let svg = std::str::from_utf8(&bytes).expect("UTF-8 SVG");
            assert!(!svg.contains("currentColor"), "{uri}");
        }
    }
}
