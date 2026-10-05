//! Curated terminal palettes shared by the console, menus, and setup screens.
//!
//! Colors describe UI roles rather than individual widgets. In particular,
//! selected text has its own foreground so both light and dark palettes work.

use ratatui::style::Color;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const DEFAULT_THEME_ID: &str = "openraid";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Color,
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub border: Color,
    pub selection: Color,
    pub selection_text: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub dark: bool,
    pub palette: Palette,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The default comes first; remaining palettes cover cool, warm, and light UI.
/// Familiar palettes are adapted for readable status text and terminal borders.
pub static THEMES: [Theme; 12] = [
    Theme {
        id: "openraid",
        name: "Openraid",
        description: "Deep slate, icy text, and calm sea-green status colors.",
        dark: true,
        palette: Palette {
            background: rgb(0x152638),
            surface: rgb(0x1e3248),
            text: rgb(0xdce7ef),
            muted: rgb(0x8fa3b8),
            accent: rgb(0xa8b8ff),
            success: rgb(0x79c9bb),
            warning: rgb(0xe8ba78),
            error: rgb(0xe88798),
            border: rgb(0x71869f),
            selection: rgb(0x304257),
            selection_text: rgb(0xf1f5fa),
        },
    },
    Theme {
        id: "tokyo-night",
        name: "Tokyo Night",
        description: "Ink-blue panels with crisp periwinkle and mint accents.",
        dark: true,
        palette: Palette {
            background: rgb(0x1a1b26),
            surface: rgb(0x24283b),
            text: rgb(0xc0caf5),
            muted: rgb(0x9aa5ce),
            accent: rgb(0x7aa2f7),
            success: rgb(0x9ece6a),
            warning: rgb(0xe0af68),
            error: rgb(0xf7768e),
            border: rgb(0x737fa8),
            selection: rgb(0x343b58),
            selection_text: rgb(0xdce4ff),
        },
    },
    Theme {
        id: "catppuccin-mocha",
        name: "Catppuccin Mocha",
        description: "Soft charcoal with lavender, peach, and pastel greens.",
        dark: true,
        palette: Palette {
            background: rgb(0x1e1e2e),
            surface: rgb(0x313244),
            text: rgb(0xcdd6f4),
            muted: rgb(0xa6adc8),
            accent: rgb(0xcba6f7),
            success: rgb(0xa6e3a1),
            warning: rgb(0xf9e2af),
            error: rgb(0xf38ba8),
            border: rgb(0x7f849c),
            selection: rgb(0x45475a),
            selection_text: rgb(0xcdd6f4),
        },
    },
    Theme {
        id: "nord",
        name: "Nord",
        description: "Arctic charcoal and restrained frost-blue highlights.",
        dark: true,
        palette: Palette {
            background: rgb(0x2e3440),
            surface: rgb(0x3b4252),
            text: rgb(0xeceff4),
            muted: rgb(0xb7c2d5),
            accent: rgb(0x88c0d0),
            success: rgb(0xa3be8c),
            warning: rgb(0xebcb8b),
            error: rgb(0xe89a9f),
            border: rgb(0x8290a8),
            selection: rgb(0x4c566a),
            selection_text: rgb(0xeceff4),
        },
    },
    Theme {
        id: "dracula",
        name: "Dracula",
        description: "Classic violet nights with bright pink and green cues.",
        dark: true,
        palette: Palette {
            background: rgb(0x282a36),
            surface: rgb(0x343746),
            text: rgb(0xf8f8f2),
            muted: rgb(0xa5abc7),
            accent: rgb(0xbd93f9),
            success: rgb(0x50fa7b),
            warning: rgb(0xf1fa8c),
            error: rgb(0xff8b92),
            border: rgb(0x858da8),
            selection: rgb(0x44475a),
            selection_text: rgb(0xf8f8f2),
        },
    },
    Theme {
        id: "gruvbox-dark",
        name: "Gruvbox Dark",
        description: "Warm espresso with golden text and earthy status colors.",
        dark: true,
        palette: Palette {
            background: rgb(0x282828),
            surface: rgb(0x3c3836),
            text: rgb(0xebdbb2),
            muted: rgb(0xbdae93),
            accent: rgb(0x8fb2a2),
            success: rgb(0xb8bb26),
            warning: rgb(0xfabd2f),
            error: rgb(0xfb8c79),
            border: rgb(0x928374),
            selection: rgb(0x504945),
            selection_text: rgb(0xfbf1c7),
        },
    },
    Theme {
        id: "rose-pine",
        name: "Rosé Pine",
        description: "Dusty violet, pale rose, and soft pine-green highlights.",
        dark: true,
        palette: Palette {
            background: rgb(0x191724),
            surface: rgb(0x26233a),
            text: rgb(0xe0def4),
            muted: rgb(0xa7a2bd),
            accent: rgb(0xc4a7e7),
            success: rgb(0x9ccfd8),
            warning: rgb(0xf6c177),
            error: rgb(0xeb6f92),
            border: rgb(0x817c9c),
            selection: rgb(0x403d52),
            selection_text: rgb(0xe0def4),
        },
    },
    Theme {
        id: "solarized-dark",
        name: "Solarized Dark",
        description: "Deep teal and balanced amber, cyan, and green accents.",
        dark: true,
        palette: Palette {
            background: rgb(0x002b36),
            surface: rgb(0x073642),
            text: rgb(0xc4d4d4),
            muted: rgb(0x93abab),
            accent: rgb(0x65b9e8),
            success: rgb(0xb4c75a),
            warning: rgb(0xe2b34c),
            error: rgb(0xf28b82),
            border: rgb(0x65878c),
            selection: rgb(0x164956),
            selection_text: rgb(0xe7eeea),
        },
    },
    Theme {
        id: "catppuccin-latte",
        name: "Catppuccin Latte",
        description: "Cool porcelain, dark ink, and rich lavender accents.",
        dark: false,
        palette: Palette {
            background: rgb(0xeff1f5),
            surface: rgb(0xe6e9ef),
            text: rgb(0x4c4f69),
            muted: rgb(0x62667d),
            accent: rgb(0x8232e6),
            success: rgb(0x34752b),
            warning: rgb(0x8b5710),
            error: rgb(0xc90b34),
            border: rgb(0x85899c),
            selection: rgb(0xccd0da),
            selection_text: rgb(0x303348),
        },
    },
    Theme {
        id: "paper",
        name: "Paper",
        description: "Clean off-white with blue ink and clear, quiet contrast.",
        dark: false,
        palette: Palette {
            background: rgb(0xf7f6f2),
            surface: rgb(0xebeef2),
            text: rgb(0x27323f),
            muted: rgb(0x5b6673),
            accent: rgb(0x254ca1),
            success: rgb(0x246347),
            warning: rgb(0x86521d),
            error: rgb(0xa92f45),
            border: rgb(0x85909e),
            selection: rgb(0xdbe4f2),
            selection_text: rgb(0x20334f),
        },
    },
    Theme {
        id: "dark",
        name: "Dark",
        description: "Neutral charcoal panels with soft periwinkle highlights.",
        dark: true,
        palette: Palette {
            background: rgb(0x101116),
            surface: rgb(0x181a21),
            text: rgb(0xe9ebf0),
            muted: rgb(0xa0a5b5),
            accent: rgb(0xa6b5ff),
            success: rgb(0x85dfc2),
            warning: rgb(0xefcd87),
            error: rgb(0xee9696),
            border: rgb(0x30333e),
            selection: rgb(0x292d40),
            selection_text: rgb(0xe9ebf0),
        },
    },
    Theme {
        id: "light",
        name: "Light",
        description: "Soft gray canvases, white panels, and indigo highlights.",
        dark: false,
        palette: Palette {
            background: rgb(0xf1f3f8),
            surface: rgb(0xffffff),
            text: rgb(0x222638),
            muted: rgb(0x616779),
            accent: rgb(0x4358b6),
            success: rgb(0x18795f),
            warning: rgb(0x8b5710),
            error: rgb(0xa92f45),
            border: rgb(0xd9dce6),
            selection: rgb(0xdfe4f7),
            selection_text: rgb(0x222638),
        },
    },
];

static ACTIVE_THEME: AtomicUsize = AtomicUsize::new(0);

/// Find a stable ID, display name, or a common shorthand (case-insensitive).
pub fn find_theme(id: &str) -> Option<&'static Theme> {
    let normalized = id.trim().to_lowercase();
    let canonical = match normalized.as_str() {
        "default" => DEFAULT_THEME_ID,
        "tokyonight" => "tokyo-night",
        "catppuccin" | "mocha" => "catppuccin-mocha",
        "latte" => "catppuccin-latte",
        "gruvbox" => "gruvbox-dark",
        "rosepine" | "rose pine" => "rose-pine",
        "solarized" => "solarized-dark",
        _ => normalized.as_str(),
    };
    THEMES
        .iter()
        .find(|theme| theme.id == canonical || theme.name.to_lowercase() == canonical)
}

/// Current process-wide palette, also used by setup before a console exists.
pub fn current_theme() -> &'static Theme {
    &THEMES[ACTIVE_THEME.load(Ordering::Relaxed)]
}

pub fn current_palette() -> Palette {
    current_theme().palette
}

/// Apply a known palette. Unknown names leave the existing palette untouched.
/// Saving the user's preference is handled separately from live preview.
pub fn set_theme(id: &str) -> bool {
    let Some(theme) = find_theme(id) else {
        return false;
    };
    let index = THEMES
        .iter()
        .position(|entry| entry.id == theme.id)
        .unwrap();
    ACTIVE_THEME.store(index, Ordering::Relaxed);
    true
}
