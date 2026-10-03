use openraid::theme::THEMES;
use ratatui::style::Color;

fn luminance(color: Color) -> f64 {
    let Color::Rgb(red, green, blue) = color else {
        panic!("built-in palettes must use explicit RGB colors: {color:?}");
    };
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

fn contrast(foreground: Color, background: Color) -> f64 {
    let foreground = luminance(foreground);
    let background = luminance(background);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

#[test]
fn built_in_themes_keep_text_statuses_and_selection_readable() {
    let mut failures = Vec::new();
    for theme in &THEMES {
        let palette = theme.palette;
        for (canvas_name, canvas) in [
            ("background", palette.background),
            ("surface", palette.surface),
        ] {
            for (role, foreground) in [
                ("text", palette.text),
                ("muted", palette.muted),
                ("accent", palette.accent),
                ("success", palette.success),
                ("warning", palette.warning),
                ("error", palette.error),
            ] {
                let ratio = contrast(foreground, canvas);
                if ratio < 4.5 {
                    failures.push(format!(
                        "{} {role}/{canvas_name}: {ratio:.2}:1 (minimum 4.5:1)",
                        theme.id
                    ));
                }
            }
        }
        let ratio = contrast(palette.selection_text, palette.selection);
        if ratio < 4.5 {
            failures.push(format!(
                "{} selection_text/selection: {ratio:.2}:1 (minimum 4.5:1)",
                theme.id
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
