# Untitled UI components

These are the MIT-licensed, open-source React components from
[untitleduico/react](https://github.com/untitleduico/react), installed using the
official `untitledui` CLI. Only the components used by OpenRaid and their local
dependencies are retained. The upstream license is in `LICENSE`.
`desktop/public/licenses/untitled-ui.txt` includes it in the frontend assets
embedded by desktop builds.

The theme tokens are from upstream commit
`4702dc0ea8d140c3491a85670c7b4fab47b722da`. `../untitled.css` maps them to the
desktop appearance palette, including body-portalled overlays. Default runtime
themes receive the neutral desktop mapping in `../appearance.ts`; other Rust
theme palettes are preserved.

Local adaptations:
- `base/select/combobox.tsx`: dropdown arrow for opening the full list, an empty
  results message, shrinkable input content for long option labels, and a
  group focus ring instead of the native inner input outline.
- `base/input/label.tsx`: the decorative required asterisk is hidden from
  assistive technology (the input carries its required state).
- `base/buttons/button.tsx`: the button type accepts a React 19 DOM ref, used
  to return keyboard focus to the sidebar toggle after collapsing navigation.
- Unused avatar add/company helpers and the barrel re-export were removed;
  the avatar keeps only helpers needed by the select components. The unused
  `contrastBorder` prop was also removed.
- `../ui.tsx`: shared button/selector adapters for existing application controls.

Keep the agent, board, transcript and session virtualizers: these are
application data views, not replacement form controls. No frontend dependency
is added to the terminal Rust package.
