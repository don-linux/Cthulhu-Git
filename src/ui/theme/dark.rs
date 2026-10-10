use eframe::egui::Color32;

use super::{Palette, Theme};

pub const THEME: Theme = Theme {
    id: "dark",
    name: "Dark",
    dark: true,
    palette: Palette {
        background: Color32::from_rgb(0x0e, 0x11, 0x16),
        surface: Color32::from_rgb(0x16, 0x1b, 0x22),
        surface_hover: Color32::from_rgb(0x1f, 0x26, 0x30),
        surface_active: Color32::from_rgb(0x28, 0x31, 0x40),
        input_background: Color32::from_rgb(0x0a, 0x0d, 0x11),
        border: Color32::from_rgb(0x2a, 0x31, 0x3c),
        text: Color32::from_rgb(0xd7, 0xdc, 0xe2),
        text_muted: Color32::from_rgb(0x8b, 0x94, 0x9e),
        accent: Color32::from_rgb(0x3f, 0xb6, 0xa8),
        on_accent: Color32::from_rgb(0x0a, 0x0d, 0x11),
        selection: Color32::from_rgb(0x1f, 0x4e, 0x4a),
        hash: Color32::from_rgb(0xc7, 0x92, 0xea),
        graph: [
            Color32::from_rgb(0x3f, 0xb6, 0xa8),
            Color32::from_rgb(0xc7, 0x92, 0xea),
            Color32::from_rgb(0xe3, 0xb3, 0x41),
            Color32::from_rgb(0x6c, 0xb6, 0xff),
            Color32::from_rgb(0xf0, 0x88, 0x3e),
            Color32::from_rgb(0xf7, 0x78, 0xba),
            Color32::from_rgb(0x7e, 0xe7, 0x87),
            Color32::from_rgb(0xfb, 0x71, 0x85),
        ],
        warning: Color32::from_rgb(0xe3, 0xb3, 0x41),
        error: Color32::from_rgb(0xf4, 0x70, 0x67),
    },
};
