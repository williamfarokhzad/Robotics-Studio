//! Compact account entry point shared by desktop and browser hosts.
//! OpenAI credentials are only handled by the hosted account service.
use crate::{
    theme::Tokens,
    ui_kit::{Container, Surface},
};
use egui::{Color32, Rect, Sense, pos2, vec2};
const ACCOUNT_URL: &str = "https://robotics-studio.onrender.com/account";
const MCP_URL: &str = "https://robotics-studio-mcp.onrender.com/";

pub(crate) fn button(ui: &mut egui::Ui, rect: Rect) {
    let id = egui::Id::new("studio_chatgpt_account");
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "ChatGPT account"));
    crate::scenario::publish_target("titlebar:chatgpt", rect);
    let t = Tokens::get();
    ui.painter().rect_filled(
        rect.shrink(2.0),
        t.radius_sm,
        if response.hovered() || response.has_focus() { t.app_bar_hover_button } else { t.app_bar },
    );
    // Approved provider mark, using the official light/dark variants.
    let icon = rect.shrink(6.0);
    let dark = crate::theme::is_dark();
    let texture_id = id.with(("logo", dark));
    let texture = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(texture_id)).or_else(|| {
        let bytes: &[u8] = if dark {
            include_bytes!("../../../assets/branding/chatgpt/logo-white.png")
        } else {
            include_bytes!("../../../assets/branding/chatgpt/logo-black.png")
        };
        let image = image::load_from_memory(bytes).ok()?.into_rgba8();
        let image = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw());
        let texture = ui.ctx().load_texture("ChatGPT official mark", image, egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.insert_temp(texture_id, texture.clone()));
        Some(texture)
    });
    if let Some(texture) = texture {
        ui.painter().image(texture.id(), icon, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    let clicked = response.on_hover_text("ChatGPT account").clicked();
    if clicked {
        ui.ctx().data_mut(|d| {
            let open = d.get_temp::<bool>(id).unwrap_or(false);
            d.insert_temp(id, !open);
        });
    }
    let open = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    if !open {
        return;
    }
    let width = 280.0_f32.min(ui.ctx().content_rect().width() - 16.0).max(120.0);
    let x = (rect.right() - width).max(8.0);
    let area = Container::new(ui.ctx(), Surface::Popup)
        .area(id.with("popup"))
        .fixed_pos(pos2(x, rect.bottom() + 8.0))
        .constrain(true)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            crate::ui_kit::Menu::frame().show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.spacing_mut().item_spacing = vec2(8.0, 12.0);
                ui.heading("Your ChatGPT account");
                ui.label("Sign in securely in your browser. Keep creating in Studio.");
                if ui.add_sized([ui.available_width(), 36.0], egui::Button::new("Continue with ChatGPT")).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(ACCOUNT_URL));
                }
                ui.small("Account sign-in and CAD access are separate. AI features are not enabled yet.");
                ui.separator();
                if ui.button("Connect Studio to ChatGPT").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(MCP_URL));
                }
                ui.small("Your existing designs remain available without signing in.");
            });
        });
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) || (!clicked && area.response.clicked_elsewhere()) {
        ui.ctx().data_mut(|d| d.insert_temp(id, false));
    }
}
