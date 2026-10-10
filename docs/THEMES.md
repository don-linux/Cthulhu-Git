# Themes

Map of the theme logic. Update this file when you change it.

## How it works

```text
settings.json "theme": "dark"
  -> theme::resolve(id)          unknown or missing id -> DEFAULT_THEME
  -> theme::apply(ctx, theme)    palette -> egui Visuals; theme stored in the egui context
  -> views: theme::current(ui.ctx()).palette.<role>
```

A theme is only data: a `Palette` of named color roles. `apply` is the only
code that touches `egui::Visuals`. Views never write `Color32` literals.

## Where things are

| File | Symbol | What it controls |
| ---- | ------ | ---------------- |
| `src/ui/theme/mod.rs` | `Palette` | The color roles every theme must fill in |
| `src/ui/theme/mod.rs` | `Theme` | `id` (saved in settings, never rename), `name`, `dark` (egui base style), `palette` |
| `src/ui/theme/mod.rs` | `THEMES` | Registry of every theme |
| `src/ui/theme/mod.rs` | `DEFAULT_THEME` | Used when settings have no theme or an unknown id |
| `src/ui/theme/mod.rs` | `by_id`, `resolve` | Id lookup, with fallback to the default |
| `src/ui/theme/mod.rs` | `apply` | Activates a theme; pins egui to the theme's dark/light base (ignores the OS preference) |
| `src/ui/theme/mod.rs` | `visuals` | The palette-to-egui mapping (widgets, panels, selection, strokes) |
| `src/ui/theme/mod.rs` | `current` | The active theme, for views |
| `src/ui/theme/dark.rs` | `THEME` | The dark theme (the only one today) |
| `src/ui/mod.rs` | `CthulhuApp::new` | Applies the saved theme at startup |
| `src/ui/widgets.rs`, `home.rs`, `repo_view.rs` | `theme::current(..).palette` | Where views read colors |

## Palette roles

| Role | Paints |
| ---- | ------ |
| `background` | Window and panels |
| `surface` | Cards, list rows, buttons at rest, top bar |
| `surface_hover` | Hovered buttons and rows |
| `surface_active` | Pressed buttons, open headers |
| `input_background` | Text fields, scroll wells |
| `border` | Card and widget outlines, separators |
| `text` | Main text |
| `text_muted` | Field labels, paths, hints, " - " between hash and summary |
| `accent` | Primary button, hover and focus outlines, links |
| `on_accent` | Text on `accent` |
| `selection` | Selected text background |
| `hash` | Commit hashes |
| `graph` | Commit-graph lanes. The same hues, faded, fill the branch-name chips. Cycled when there are more lanes than colors |
| `warning` | Detached HEAD |
| `error` | Error banner |

## Add a theme

1. Copy `src/ui/theme/dark.rs` to `src/ui/theme/<name>.rs` and change `id`, `name`, `dark` and the colors.
2. Declare it in `src/ui/theme/mod.rs` (`mod <name>;`) and add `<name>::THEME` to `THEMES`.
3. `cargo test` checks that ids are unique and the default is registered.

To make it the default, point `DEFAULT_THEME` at it. To switch at run time
(future picker): `theme::apply(ctx, theme)`, then save `settings.theme =
Some(theme.id.to_owned())` (see `docs/SETTINGS.md`).

## Add a color role

1. Add the field to `Palette` with a doc comment saying what it paints.
2. Give it a value in every theme file.
3. Use it in `visuals` (if egui should use it) or in a view via `theme::current(..).palette`.
4. Add it to the table above.

## Rules

- No `Color32` literals outside `src/ui/theme/`.
- Only `apply` / `visuals` modify `egui::Visuals`.
- A published theme `id` is never renamed: it is stored in users' settings.
