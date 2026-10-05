use openraid::theme::{find_theme, DEFAULT_THEME_ID, THEMES};
use ratatui::style::Color;

#[test]
fn dark_and_light_presets_match_the_desktop_previews() {
    for (id, name, dark, colors) in [
        (
            "dark",
            "Dark",
            true,
            [0x101116, 0x181a21, 0xe9ebf0, 0xa0a5b5, 0xa6b5ff, 0x30333e],
        ),
        (
            "light",
            "Light",
            false,
            [0xf1f3f8, 0xffffff, 0x222638, 0x616779, 0x4358b6, 0xd9dce6],
        ),
    ] {
        let theme = find_theme(id).expect("desktop preset must be in the shared catalog");
        assert_eq!(theme.name, name);
        assert_eq!(theme.dark, dark);
        assert_eq!(find_theme(&name.to_uppercase()), Some(theme));
        assert_eq!(THEMES.iter().filter(|item| item.id == id).count(), 1);
        for (actual, hex) in [
            theme.palette.background,
            theme.palette.surface,
            theme.palette.text,
            theme.palette.muted,
            theme.palette.accent,
            theme.palette.border,
        ]
        .into_iter()
        .zip(colors)
        {
            assert_eq!(
                actual,
                Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8),
                "{id} desktop preview color"
            );
        }
    }
}

#[test]
fn adding_desktop_presets_does_not_change_the_terminal_default() {
    assert_eq!(DEFAULT_THEME_ID, "openraid");
    assert_eq!(THEMES[0].id, DEFAULT_THEME_ID);
    assert_eq!(find_theme("default"), find_theme(DEFAULT_THEME_ID));
}
