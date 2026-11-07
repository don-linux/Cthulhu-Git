# Fonts

Map of the bundled faces. Update this file when you change them.

The app does not read fonts from the operating system. Each face below is
compiled into the binary from `assets/fonts/`. A license file sits next to
every face and has to stay there: the licenses require the notice to travel
with the font.

## How it works

```text
settings.json "ui_font": "inter"
  -> fonts::resolve_ui            unknown or missing id -> ubuntu-sans
  -> FontService::sync            egui proportional family

settings.json "terminal_font": "meslo-lgs-nf"
  -> fonts::resolve_terminal      unknown or missing id -> built-in monospace
  -> FontService::sync            egui family "terminal", measured one pass later
```

Hashes and diffs stay on the built-in monospace. Choosing Obel Mono for the
interface or the terminal does not change them. Choosing it for the terminal
does not change the interface, and the other way around.

egui treats strong text as a brighter color, not a heavier weight, so only
the Regular cut of each family is bundled.

## Where things are

| File | Symbol | What it controls |
| ---- | ------ | ---------------- |
| `src/ui/fonts.rs` | `UI_FACES` | Interface catalog, in page order |
| `src/ui/fonts.rs` | `TERMINAL_FACES` | Terminal catalog, in page order |
| `src/ui/fonts.rs` | `DEFAULT_UI_ID` | Ubuntu Sans |
| `src/ui/fonts.rs` | `FontService::sync` | Installs both choices with one `set_fonts` |
| `src/ui/settings.rs` | Appearance page | Interface choice |
| `src/ui/settings.rs` | Terminal page | Terminal choice, plus the one-line preview |
| `src/settings.rs` | `ui_font`, `terminal_font` | Saved ids |

## Interface

| Id | Name | File | License |
| -- | ---- | ---- | ------- |
| `ubuntu-sans` | Ubuntu Sans | `assets/fonts/ubuntu-sans/UbuntuSans-Regular.ttf` | Ubuntu Font Licence 1.0 |
| `obel-mono` | Obel Mono | `assets/fonts/obel-mono/ObelMono-Regular.ttf` | SIL OFL 1.1 |
| `aspekta` | Aspekta | `assets/fonts/aspekta/Aspekta-400.ttf` | SIL OFL 1.1 |
| `inter` | Inter | `assets/fonts/inter/Inter-Regular.ttf` | SIL OFL 1.1 |

`null` uses `ubuntu-sans`.

## Terminal

| Id | Name | File | License |
| -- | ---- | ---- | ------- |
| `obel-mono` | Obel Mono | same file as the interface face | SIL OFL 1.1 |
| `meslo-lgs-nf` | Meslo LGS NF | `assets/fonts/meslo-lgs-nf/MesloLGS-NF-Regular.ttf` | Apache 2.0 |
| `departure-mono` | Departure Mono | `assets/fonts/departure-mono/DepartureMono-Regular.otf` | SIL OFL 1.1 |

`null` uses the built-in monospace. Departure Mono is drawn at 11 pt so its
pixels sit on the grid. Meslo LGS NF is the face that includes prompt icons.

## Add a face

1. Put the unmodified Regular file and its license text in a new directory
   under `assets/fonts/<id>/`. Do not rename the font inside the file.
2. Add a `Face` to `UI_FACES` or `TERMINAL_FACES` in `src/ui/fonts.rs`.
   The `id` is what settings store. A monospaced face that is already bundled
   may use that same id in the other catalog; share one `include_bytes!`.
3. Add a row to the table above.
