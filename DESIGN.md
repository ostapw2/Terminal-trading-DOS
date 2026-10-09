# DOS Design System

> Single source of truth for visual rules.  Every layout, theme, and
> component must conform — across **three runtimes** (Python prototype,
> Rust production runtime, DOS/Pascal legacy target).

## 0. Multi-runtime architecture

```
dos/
├── shared/                              ← canonical contract (lang-agnostic)
│   ├── tokens.toml                      ← colors, glyphs, heights, spacing
│   └── codegen.py                       ← emits per-runtime token files
├── python/                              ← interactive prototype + reference UI
│   └── src/dos/{themes,components,examples,design,...}
├── rust/                                ← production runtime (single binary)
│   └── src/{tokens.rs,widgets/,bin/}
└── legacy/                              ← DOS / FreeDOS / DOSBox build
    ├── tokens.inc        (generated)
    ├── main.pas
    └── Makefile
```

**Source of truth:** `shared/tokens.toml`.  Every runtime gets its tokens
from a generator — no two files are independently maintained.  See §2.

**`make sync-tokens` regenerates:**
* `python/src/dos/design/tokens_generated.py`
* `rust/src/tokens.rs`
* `legacy/tokens.inc`

The hand-written TCSS files (`python/src/dos/themes/*.tcss`) currently still
mirror the values manually — `tests/test_design_invariants.py` enforces
parity.

## 1. Philosophy

1. **Volkov Commander (1992-2002) aesthetic.**  Double-line modal frames,
   bordered 3-row buttons, pull-down menu bar, panel views, status line,
   F-key bar.  Mouse + keyboard equal citizens.
2. **Unicode box-drawing + block-shadow chars are part of the contract.**
   Single-line (`─│┌┐└┘├┤┬┴┼`), double-line (`═║╔╗╚╝╠╣╦╩╬`), and the block
   ladder (`█▀▄▌▐░▒▓`) are required to reproduce the Volkov look.  These
   characters render identically on every modern monospace font (CP437
   compatibility predates Unicode).
3. **Form glyphs stay ASCII.**  `[X] (*) [ON ]` etc. — these are part of
   the typographic identity of the project and look right in any font.
4. **Two themes, one widget set.**  `dos-classic` (CGA blue/yellow/cyan) and
   `dos-modern` (subtle dark) share every component — only the color palette
   differs.
5. **Visual fidelity across runtimes.**  Same screen on Python+Textual and
   Rust+Ratatui MUST look identical.  The Pascal target stays in the legacy/
   tree as an artifact and may differ.

## 2. Color tokens

Canonical tokens live in **`shared/tokens.toml`**.  Mirrors are generated:

| Runtime | File | Form | Maintenance |
|---|---|---|---|
| Canonical | `shared/tokens.toml` | TOML tables | hand-written |
| Python TCSS (Textual CSS) | `python/src/dos/themes/*.tcss` | `$var: rgb(...)` | hand-written, parity-tested |
| Python constants | `python/src/dos/design/tokens.py` | `Palette` dataclass | hand-written, parity-tested |
| Python generated | `python/src/dos/design/tokens_generated.py` | `GenPalette` const | **generated** |
| Rust | `rust/src/tokens.rs` | `pub const Palette` | **generated** |
| Pascal | `legacy/tokens.inc` | `const CL_*` (CGA indices) | **generated** |

Run `make sync-tokens` after editing `shared/tokens.toml`.

Test `python/tests/test_design_invariants.py::test_palette_colors_appear_in_tcss`
enforces TCSS-vs-Python parity.

### `dos-classic` (DOS 1992)

| Role | Token (TCSS) | RGB | CGA index |
|---|---|---|---|
| Screen bg | `$dos-blue` | `0,0,170` | 1 |
| Header / F-bar label / panel sub-bar | `$dos-cyan` | `0,170,170` | 3 |
| Default fg | `$dos-white` | `255,255,255` | 15 |
| Highlight / active border / primary button | `$dos-yellow` | `255,255,85` | 14 |
| Hover / inactive switch | `$dos-light-cyan` | `85,255,255` | 11 |
| Cursor (active) bg | `$dos-white` | `255,255,255` | 15 |
| Cursor (active) fg | `$dos-blue` | `0,0,170` | 1 |
| Cursor (inactive) bg | `$dos-cyan` | `0,170,170` | 3 |
| Muted / disabled | `$dos-grey` | `170,170,170` | 7 |
| Error | `$dos-red` | `170,0,0` | 4 |
| Modal dim | `$dos-blue 60%` | (alpha overlay) | — |

### `dos-modern` (variable names prefixed `$dm_` to avoid clash with Textual built-ins like `$panel`, `$accent`)

| Role | Token | RGB |
|---|---|---|
| Screen bg | `$dm_bg` | `20,22,28` |
| Surface | `$dm_panel` | `28,32,40` |
| Text | `$dm_text` | `220,225,235` |
| Accent (focus / primary) | `$dm_accent` | `98,152,255` |
| Accent dim (inactive) | `$dm_accent_soft` | `58,92,160` |
| Muted | `$dm_muted` | `140,148,165` |

> **Pitfall.**  Do **not** name a TCSS variable `$panel`, `$accent`,
> `$primary`, `$background`, `$foreground`, `$boost`, `$surface`,
> `$success`, `$warning`, `$error` — Textual uses these for its built-in CSS
> (notably `hatch: right $panel` in the maximized-screen rule).  Redefining
> them as a color when Textual expects a percentage breaks parsing
> globally.  Use a project prefix (`$dos-` / `$dm_`).

## 3. Borders

There is exactly one allowed border style: **`ascii`**.

```css
FilePanel       { border: ascii $dos-white;  }
FilePanel.-focused { border: ascii $dos-yellow; }
ModalDialog > Container { border: ascii $dos-white; }
RadioSet        { border: ascii $dos-white; }
SelectOverlay   { border: ascii $dos-white; }
```

Forbidden in the codebase:

* `border: double` (uses `╔═╗║`)
* `border: solid` (uses `┌─┐│`)
* `border: round` (uses `╭─╮│`)
* `border: heavy` / `dashed` / `tall` / `wide` / `inner` / `outer` /
  `thick` / `block` / `panel` / `tab` / `hkey` / `vkey`
* Any inline Unicode box-drawing in `Static` content

Form controls have **no border** at all — colors alone signal state.

## 4. Heights

Every form widget is exactly **1 row** tall.  `border: tall` (which Textual
applies to Button/Input/Switch by default) adds 2 half-block rows; we
override with `border: none; height: 1`.

| Widget | Height |
|---|---|
| MenuBar / StatusLine / FunctionBar | 1 |
| Button / Input / Select / Checkbox / RadioButton / Switch | 1 |
| RadioSet | `auto` (3 buttons + 2 border = 5) |
| Modal Container | `auto` with `max-height: 90%` |
| FilePanel | `1fr` |

## 5. Spacing

Margin **does not** participate in `height: auto` calculations on the last
child of a Container — Textual rounds it down and the buttons end up clipped
on the bottom border.  Use **explicit `Static("")` spacer widgets** instead.

```python
yield Label("...", classes="form-help")
yield _spacer()                           # 1-row gap
yield Label("-- Section --", classes="form-section")
```

The helper:

```python
def _spacer() -> Static:
    s = Static("")
    s.styles.height = 1
    return s
```

Lives in `widget_gallery.py`.  Move to `dos/components/utils.py` if reused.

## 6. ASCII glyph contract

| Element | Glyph |
|---|---|
| Border corner | `+` |
| Border horizontal | `-` |
| Border vertical | `|` |
| Checkbox on / off | `[X]` / `[ ]` |
| Radio on / off | `(*)` / `( )` |
| Switch on / off | `[ON ]` / `[off]` |
| Select dropdown | `v` (closed), `^` (open) |
| Scrollbar thumb / track | `#` / ` ` |
| Directory prefix | `/` |
| Directory size cell | `[DIR]` |
| Parent-dir row | `/..` + `UP--DIR` |
| Section rule | `-- Title --` |
| Truncation suffix | `~` |

Implemented via:

* `dos/components/ascii_widgets.py` — `AsciiCheckbox`, `AsciiRadioButton`,
  `AsciiSwitch`, `AsciiSelect`
* `dos/__init__.py` — monkey-patches `ScrollBarRender.VERTICAL_BARS` and
  `HORIZONTAL_BARS` before any widget is constructed

## 7. File structure

```
dos/
├── DESIGN.md                  # this doc
├── RESEARCH.md                # initial architecture research
├── INSTALL.md                 # toolchain setup per runtime
├── Makefile                   # make python|rust|legacy|sync-tokens|test
├── shared/                    # CANONICAL — lang-agnostic
│   ├── tokens.toml            # palettes, glyphs, heights, spacing
│   └── codegen.py             # emits per-runtime token files
├── python/                    # PROTOTYPE — Textual reference UI + tests
│   ├── pyproject.toml
│   ├── src/dos/
│   │   ├── __init__.py        # ScrollBarRender ASCII patch
│   │   ├── design/{__init__,tokens,tokens_generated}.py
│   │   ├── themes/{dos_classic,dos_modern}.tcss
│   │   ├── components/        # AsciiCheckbox, FilePanel, ... (see §8)
│   │   └── examples/commander.py
│   └── tests/test_design_invariants.py
├── rust/                      # PRODUCTION RUNTIME — Ratatui, single binary
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── tokens.rs          (generated)
│       ├── widgets/{function_bar,menu_bar,modal,status_line,two_panel}.rs
│       └── bin/commander.rs
└── legacy/                    # DOS / FreeDOS — Free Pascal + Free Vision
    ├── Makefile
    ├── tokens.inc             (generated)
    └── main.pas
```

## 8. Cookbook

> All recipes assume `make sync-tokens` is run after any edit to
> `shared/tokens.toml`.

### 8.0  Cross-runtime change of a token

```bash
$EDITOR shared/tokens.toml          # change canonical value
make sync-tokens                    # regenerates Rust + Pascal + Python_generated
# Hand-edit the corresponding $vars in python/src/dos/themes/*.tcss
make python                         # parity test catches mismatches
```

### 8.1  Add a new form widget (e.g. a number input)

1. **Subclass an existing Textual widget.**  Place in
   `dos/components/ascii_widgets.py` if it has Unicode characters to replace,
   else in its own file in `dos/components/`.
2. **Add CSS** to **both** themes.  Required: `border: none; height: 1;
   padding: 0 1; margin: 0 1 0 0;` plus colors:
   ```css
   NumberInput {
       background: $dos-black;
       color: $dos-light-cyan;
       border: none;
       height: 1;
       padding: 0 1;
   }
   NumberInput:focus { color: $dos-yellow; }
   ```
3. **Export** from `dos/components/__init__.py`.
4. **Add to `WidgetGallery`** so the widget has a visual demo and is covered
   by the layout test.
5. **Run audit** (`pytest tests/test_design_invariants.py`) — no
   non-ASCII characters allowed.

### 8.2  Add a new modal screen — Python

```python
# python/src/dos/components/my_dialog.py
from dos.components.modal_dialog import ModalDialog
from textual.app import ComposeResult
from textual.widgets import Label, Button

class MyDialog(ModalDialog):
    def __init__(self) -> None:
        super().__init__(title=" My Dialog ")

    def compose_body(self) -> ComposeResult:
        yield Label("Body text", classes="form-help")
        yield Button("OK", variant="primary")
```

Wire into `commander.py`:

```python
F_KEYS = [..., FKey(2, "MyDlg", "show_my_dialog"), ...]
def _dispatch(self, action: str) -> None:
    if action == "show_my_dialog":
        self.push_screen(MyDialog())
```

### 8.2.1  Port that modal to Rust

```rust
// In commander.rs add to App state:
//   modal: Option<ModalKind>,
//   focused_btn: usize,
// where:
#[derive(Clone, Copy, PartialEq)]
enum ModalKind { Help, Forms, MyDlg }

// Compose body inside terminal.draw closure, AFTER the main panels:
last_modal_layout = match app.modal {
    Some(ModalKind::MyDlg) => Some(modal::render(frame, area, &my_dlg(app.focused_btn), palette)),
    None => None,
    // ... other modals
};

fn my_dlg(focused: usize) -> Modal<'static> {
    Modal {
        title: "My Dialog",
        body: &[
            "Body text line 1.",
            "",
            "More body if needed.",
        ],
        buttons: &[
            modal::Button { label: "OK",     primary: true  },
            modal::Button { label: "Cancel", primary: false },
        ],
        focused_button: focused,
    }
}
```

Key handler must short-circuit to modal when one is open:

```rust
fn handle_key(app: &mut App, code: KeyCode) {
    if app.modal.is_some() {
        match code {
            KeyCode::Esc | KeyCode::Enter => { app.modal = None; }
            KeyCode::Tab | KeyCode::Right => { /* cycle focused_btn */ }
            // ...
        }
        return;
    }
    // ... normal handling
}
```

The `modal::render` function returns a `ModalLayout { area, button_rects }`
which the mouse handler uses to detect button clicks vs background-click
(the latter dismisses the modal).

### 8.3  Add a new theme variant

1. Copy `themes/dos_classic.tcss` → `themes/dos_amber.tcss`.
2. Replace color values; **rename** all `$dos-*` vars to `$amber_*` to avoid
   accidental override.
3. Add `DOS_AMBER` constant in `src/dos/__init__.py`.
4. Wire into `commander.py`'s `--theme amber` arg.
5. Optionally add a `Palette` to `design/tokens.py` for non-CSS code.

### 8.4  Verify changes

```bash
cd path/to/dos
uv sync
uv run pytest tests/test_design_invariants.py -v
uv run dos-commander                  # interactive verification
uv run dos-commander --theme modern
```

The invariant tests check:

1. CommanderApp starts without error in classic + modern.
2. WidgetGallery opens with all controls visible (no clipping).
3. Rendered output contains zero non-ASCII codepoints.
4. TCSS files parse without warnings.
5. `Palette` Python tokens match the corresponding TCSS `$var` definitions.

## 9. Pitfalls (the things that have already burned us)

1. **`Button("[ OK ]")` renders empty.**  Rich parses `[ OK ]` as a markup
   tag, fails to resolve it, drops the text.  Use `Button("OK")` and rely on
   bg color for the visual.
2. **`CSS_PATH` set on instance after `super().__init__()` is ignored.**
   Textual reads it at class init time.  Set `type(self).CSS_PATH = ...`
   *before* calling `super().__init__()`.
3. **TCSS variable named `$panel` clashes with Textual's built-in
   stylesheet** (`hatch: right $panel`).  The default rule expects a
   percentage and gets your color → `StylesheetParseError`.  Always
   namespace: `$dos-*`, `$dm_*`.
4. **Multiple widgets with `dock: bottom` share one row.**  Wrap them in a
   single `Vertical` container with `dock: bottom; height: N`, then dock the
   container, not the children.  See `#bottom-bar`.
5. **`height: auto` on a Container ignores margin-top of trailing children.**
   If your form's last button row uses `margin-top: 1`, it overflows the
   computed height by 1 row and gets clipped on the bottom border.  Use
   explicit `Static("")` spacers (height 1) instead of CSS margins.
6. **Border styles named like `tall`, `wide`, `inner`, `outer`, `panel`,
   `tab`, `hkey`, `vkey`, `block`, `thick`** all use Unicode block
   characters (`▀▄▌▐█`) that break the ASCII guarantee.  Use only `ascii` or
   `none`.
7. **Default Textual scrollbars use 8-step Unicode block gradient
   (`▁▂▃▄▅▆▇` and `▉▊▋▌▍▎▏`).**  Patched via monkey-patch in `dos/__init__.py`
   to `#`.
8. **`Select` widget hard-codes `▼` / `▲` arrows in its compose.**  Use
   `AsciiSelect` from `ascii_widgets.py` which post-mounts a query that
   replaces the arrow `Static` content with `v` / `^`.
9. **(Rust) `crossterm::EnableMouseCapture` does NOT enable
   motion-no-button reporting** — only button + drag.  The terminal will
   not report mouse position when the user is just hovering, so a
   visible cursor cannot follow the mouse.  Fix: write `\x1B[?1003h`
   directly to stdout *after* `EnableMouseCapture`, and `\x1B[?1003l`
   before `DisableMouseCapture` on shutdown.  See
   `rust/src/bin/commander.rs::main` for the canonical pattern.
10. **(Rust) Terminal does NOT render a visible mouse cursor** for SGR
   mouse events — apps draw it themselves.  Track the latest
   `MouseEvent.column/row` in app state and after the main render do
   `frame.buffer_mut().cell_mut((x, y)).modifier.insert(Modifier::REVERSED)`.
   This inverts fg/bg of the cell under the mouse — same NC-era
   "lighted block" feel without introducing any non-ASCII glyph.
11. **(Rust) Modal overlays must call `Clear` widget on their area
    before drawing the block**, otherwise the panels behind bleed
    through ratatui's transparent buffer.  See
    `rust/src/widgets/modal.rs::render`.
12. **(Rust) `Block::title()` uses default style by default** — title
    text will render with terminal default fg/bg rather than the modal
    palette.  Always pass a styled `Span::styled(...)` rather than a
    raw string.

## 9.6  Allowed Unicode glyph sets

Volkov-style aesthetics need Unicode box-drawing and block-shadow chars.
The full set is enumerated below — anything else is forbidden.

**Borders (any widget):**

| Glyph set | Codepoints | Role |
|---|---|---|
| Single-line `─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼` | U+2500-U+253C | panels, sub-cards |
| Double-line `═ ║ ╔ ╗ ╚ ╝ ╠ ╣ ╦ ╩ ╬` | U+2550-U+256C | modals, top-level dialogs |
| Half-blocks `▀ ▄ ▌ ▐ █` | U+2580-U+2588 | shadows, button faces |
| Shading `░ ▒ ▓` | U+2591-U+2593 | drop-shadow texture |

**Charts only — extra glyphs:**

| Glyph | Codepoint | Role |
|---|---|---|
| `▁ ▂ ▃ ▄ ▅ ▆ ▇` | U+2581..U+2587 | 8-step ladder (volume bars + body bottoms) |
| `┄` | U+2504 | half-grid dotted |
| `▲ ▼` | U+25B2, U+25BC | bull/bear directional tip |

**Modal frames** use `BorderType::Double` (ratatui) → `╔═╗║╚╝`.
**Panels / sub-cards** use `BorderType::Plain` (ASCII `+ - |`) or
`BorderType::Rounded` for inset cards.

**Buttons** are 3-row bordered boxes (`widgets/button.rs`, `HEIGHT = 3`,
face row in the middle).  There is **no drop shadow**: the border style
switches with state (idle / hover / pressed / focused / primary).  A caller
must give a button a `Rect` at least 3 rows tall; shorter rects draw nothing.

**Other glyphs in use** (by widgets, outside the sets above):

| Glyph | Where |
|---|---|
| `▔ ◆ Σ Δ` | chart: line cap, footprint POC / total / delta; `◆` also marks the status-line `DEMO DATA` badge |
| `▏▎▍▌▋▊▉` | order book: depth bars (eighth blocks) |
| `● ○ ⚡` | status line: live / paper / WS indicators |
| `← ↑ → ↓ ↔ ± × · • … – — ≈ §` | labels, hints, key names |
| `⚠` | order book warnings |

Add a glyph only if it renders in every mainstream monospace font, and list
it here.

**Rationale.**  CP437 includes the half-blocks (`▀ ▄ █ ░ ▒ ▓`) — they
predate Unicode and rendered identically on 1985 IBM PC text mode and
2026 Kitty.  The eighth-block series (`▁..▇`) is U+2580 plane, in every
monospace font that ships with macOS, Windows, or any modern Linux for
the past 15 years.  No font on a remotely modern system fails to render
these.

## 9.5  Mouse cursor (Rust runtime only)

The terminal does not draw a cursor for SGR mouse events.  We render one:

1. **Enable hover tracking** — after `EnableMouseCapture`, write `\x1B[?1003h`
   manually (`crossterm` only enables button+drag, not motion).  Pair with
   `\x1B[?1003l` on shutdown.
2. **Track latest position** — `app.mouse: Option<(u16, u16)>` updated on
   every `Event::Mouse`.
3. **Render** — after the main `terminal.draw(...)` content, do:
   ```rust
   if let Some((mx, my)) = app.mouse {
       if let Some(cell) = frame.buffer_mut().cell_mut((mx, my)) {
           cell.modifier.insert(Modifier::REVERSED);
       }
   }
   ```

Result: a single inverted cell follows the mouse — pure ASCII, classic NC
look.  Works in iTerm2, Kitty, WezTerm, Alacritty, Ghostty, Windows
Terminal 1.18+.  Falls back to "cursor visible only on click/drag" in
older terminals (macOS Terminal.app, old GNOME Terminal).

## 10. Versioning

Bump `__version__` in `python/src/dos/__init__.py` when:

* Tokens are added, renamed, or recolored.
* Border policy changes (currently: ascii only).
* Component API breaks.

Bump this document's version note when it changes substantively.

---

If a future change feels like it would require breaking one of the
**Pitfalls** rules, stop and find a different solution — every rule above
came from a real failure.
