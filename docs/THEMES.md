# Terminal themes

OpenRaid includes ten themes for the interactive console. Open `/themes` to
browse the catalog and choose a palette that suits your terminal and lighting.

## Browse and apply

1. Enter `/themes` in the console, choose it from the command menu, or press
   `Ctrl+X`, then `y`.
2. Type a theme name, ID, description, or `dark` / `light` to filter the list.
   Use `Backspace` to edit the search, `Ctrl+U` to clear it, and the arrow keys
   to move through matching themes. `Home` / `End` jump to the first / last
   match; `PageUp` / `PageDown` move through the list in larger steps.
3. Compare the preview's ordinary text, secondary text, focused rows, and status
   colors. The active-theme marker identifies the theme currently in use;
   highlighting another row previews that candidate locally.
4. Press `Enter` to apply the highlighted theme and save your choice. Press
   `Esc` to close the picker without changing the active theme.

The preview does not change your saved preference. On wider terminals, the
catalog and preview appear side by side; on narrower terminals, the picker uses
a compact layout. Enlarge a very small terminal window to see the preview.

To apply a known theme directly, enter its ID after the command:

```text
/themes nord
/themes catppuccin-latte
/themes openraid
```

`/theme` is an alias for `/themes`. An unknown ID leaves the active theme
unchanged and shows a notice directing you to the picker.

## Choose a palette

Eight themes use dark backgrounds; two use light backgrounds. **Openraid** is
the default and retains the console's original blue-gray palette.

| Theme | ID | Background | Character |
| --- | --- | --- | --- |
| Openraid | `openraid` | Dark | Blue-gray with soft lavender and sea-green accents |
| Tokyo Night | `tokyo-night` | Dark | Inky blue with crisp blue and violet accents |
| Catppuccin Mocha | `catppuccin-mocha` | Dark | Soft charcoal with pastel accents |
| Nord | `nord` | Dark | Cool slate with restrained frost-blue accents |
| Dracula | `dracula` | Dark | Charcoal with vivid purple, pink, and green accents |
| Gruvbox Dark | `gruvbox-dark` | Dark | Warm charcoal with earthy gold and green accents |
| Rosé Pine | `rose-pine` | Dark | Muted purple with rose and pine accents |
| Solarized Dark | `solarized-dark` | Dark | Deep teal with balanced blue and yellow accents |
| Catppuccin Latte | `catppuccin-latte` | Light | Pale background with clear, colorful accents |
| Paper | `paper` | Light | A clean reading surface with understated accents |

Pick a dark theme for a dim room or a light theme for a bright workspace. The
palette colors the setup screens, dashboard, command menus, and text selection,
so the interface keeps a consistent appearance throughout a session.

## Saved preference

The theme is a **global appearance preference**, shared across workspaces and
sessions. Your next launch restores it, including the guided setup screens.

OpenRaid stores the preference in `openraid/theme.json` under the first
applicable location:

| Setting or platform | Preference path |
| --- | --- |
| `OPENRAID_THEME_FILE` is set | The exact path in that variable |
| `XDG_CONFIG_HOME` is set | `$XDG_CONFIG_HOME/openraid/theme.json` |
| Windows with `APPDATA` | `%APPDATA%\openraid\theme.json` |
| Otherwise | `~/.config/openraid/theme.json` |

On Windows, the final home-directory fallback uses `USERPROFILE` when `HOME` is
not set. A missing preference file, malformed JSON, or an unrecognized stored
theme falls back to **Openraid**. To return to the default, choose Openraid in
`/themes`.

The file contains a single theme ID, for example:

```json
{
  "theme": "nord"
}
```

If OpenRaid cannot save the preference, the chosen theme still applies to the
current console and a notice explains that it is session-only. Check that the
preference directory is writable to retain the choice on your next launch.

## Terminal appearance

For the intended colors, use a terminal with RGB/true-color support. Your
terminal controls the font, font size, and line spacing; OpenRaid themes control
the console's colors. If the interface feels too dense, increase the terminal's
font size or window dimensions.

Labels, selection markers, and status text remain useful alongside the colors.
Use the theme preview to compare ordinary text, secondary text, focused rows,
and status colors before choosing a palette.
