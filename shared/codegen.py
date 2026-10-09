#!/usr/bin/env python3
"""Token codegen.

Reads `shared/tokens.toml` (canonical) and emits per-runtime constants:

    python/src/dos/design/tokens_generated.py
    rust/src/tokens.rs
    legacy/tokens.inc

Run via `make sync-tokens` from repo root, or directly:

    python3 shared/codegen.py

Idempotent — overwrites generated files only.  Hand-written files are not
touched.

Standard library only: works under any Python 3.11+ (tomllib).
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path
from textwrap import dedent

REPO    = Path(__file__).resolve().parent.parent
TOKENS  = REPO / "shared" / "tokens.toml"

OUT_PY  = REPO / "python" / "src" / "dos" / "design" / "tokens_generated.py"
OUT_RS  = REPO / "rust"   / "src" / "tokens.rs"
OUT_PAS = REPO / "legacy" / "tokens.inc"

GENERATED_BANNER_PY  = "# AUTO-GENERATED FROM shared/tokens.toml — DO NOT EDIT BY HAND.\n"
GENERATED_BANNER_RS  = "// AUTO-GENERATED FROM shared/tokens.toml — DO NOT EDIT BY HAND.\n"
GENERATED_BANNER_PAS = "{ AUTO-GENERATED FROM shared/tokens.toml — DO NOT EDIT BY HAND. }\n"


def load_tokens() -> dict:
    return tomllib.loads(TOKENS.read_text())


def emit_python(tokens: dict) -> str:
    out: list[str] = [
        GENERATED_BANNER_PY,
        '"""Generated design tokens.  Source: shared/tokens.toml."""\n',
        "from __future__ import annotations\n",
        "from dataclasses import dataclass\n",
        "\n",
        "@dataclass(frozen=True, slots=True)\n",
        "class GenPalette:\n",
    ]
    # All color fields exist in every palette by contract; collect from classic
    color_fields = list(tokens["palettes"]["classic"]["colors"].keys())
    for f in color_fields:
        out.append(f"    {f}: tuple[int, int, int]\n")
    out.append("    name: str = ''\n\n")

    for slug, pal in tokens["palettes"].items():
        const_name = slug.upper()
        colors = pal["colors"]
        out.append(f"{const_name} = GenPalette(\n")
        for f in color_fields:
            r, g, b = colors[f]
            out.append(f"    {f}=({r}, {g}, {b}),\n")
        out.append(f'    name="{pal["name"]}",\n')
        out.append(")\n\n")

    out.append("GLYPHS = {\n")
    for k, v in tokens["glyphs"].items():
        out.append(f"    {k!r}: {v!r},\n")
    out.append("}\n\n")

    out.append("HEIGHTS = {\n")
    for k, v in tokens["heights"].items():
        out.append(f"    {k!r}: {v},\n")
    out.append("}\n\n")

    out.append("SPACING = {\n")
    for k, v in tokens["spacing"].items():
        out.append(f"    {k!r}: {v},\n")
    out.append("}\n")

    return "".join(out)


def emit_rust(tokens: dict) -> str:
    out: list[str] = [
        GENERATED_BANNER_RS,
        "//! Generated design tokens.  Source: shared/tokens.toml.\n",
        "\n",
        "use ratatui::style::Color;\n\n",
        "pub struct Palette {\n",
    ]
    color_fields = list(tokens["palettes"]["classic"]["colors"].keys())
    for f in color_fields:
        out.append(f"    pub {f}: Color,\n")
    out.append("    pub name: &'static str,\n")
    out.append("}\n\n")

    for slug, pal in tokens["palettes"].items():
        const_name = slug.upper()
        colors = pal["colors"]
        out.append(f"pub const {const_name}: Palette = Palette {{\n")
        for f in color_fields:
            r, g, b = colors[f]
            out.append(f"    {f}: Color::Rgb({r}, {g}, {b}),\n")
        out.append(f'    name: "{pal["name"]}",\n')
        out.append("};\n\n")

    out.append("pub mod glyphs {\n")
    for k, v in tokens["glyphs"].items():
        escaped = v.replace("\\", "\\\\").replace('"', '\\"')
        out.append(f'    pub const {k.upper()}: &str = "{escaped}";\n')
    out.append("}\n\n")

    out.append("pub mod heights {\n")
    for k, v in tokens["heights"].items():
        out.append(f"    pub const {k.upper()}: u16 = {v};\n")
    out.append("}\n\n")

    out.append("pub mod spacing {\n")
    for k, v in tokens["spacing"].items():
        out.append(f"    pub const {k.upper()}: u16 = {v};\n")
    out.append("}\n")

    return "".join(out)


def emit_pascal(tokens: dict) -> str:
    """Free Pascal include file with palette as 16-color CGA indices.

    DOS only has 16 colors.  We map each palette field to the closest CGA
    index that the BIOS/VGA mode 03h supports.  The mapping is hand-curated
    in the tokens.toml comments; here we just emit constants the FP source
    will use.
    """
    # CGA color indices — these are what `crt` unit accepts.
    # 0=Black 1=Blue 2=Green 3=Cyan 4=Red 5=Magenta 6=Brown 7=LightGray
    # 8=DarkGray 9=LightBlue 10=LightGreen 11=LightCyan 12=LightRed
    # 13=LightMagenta 14=Yellow 15=White
    # We emit the CGA index constants used by classic palette only.
    cga_classic = {
        "bg":         1,    # Blue
        "panel":      3,    # Cyan
        "black":      0,    # Black
        "text":       15,   # White
        "muted":      7,    # LightGray
        "accent":     14,   # Yellow
        "accent_dim": 3,    # Cyan
        "light_cyan": 11,   # LightCyan
        "cursor_bg":  15,   # White
        "cursor_fg":  1,    # Blue
        "ok":         10,   # LightGreen
        "warn":       14,   # Yellow
        "error":      4,    # Red
    }
    out: list[str] = [
        GENERATED_BANNER_PAS,
        "{ DOS Classic palette — CGA color indices for crt unit. }\n",
        "\n",
        "const\n",
    ]
    for k, idx in cga_classic.items():
        out.append(f"  CL_{k.upper():12} = {idx};\n")

    out.append("\n")
    out.append("{ ASCII glyphs — same on every runtime by contract. }\n")
    out.append("const\n")
    for k, v in tokens["glyphs"].items():
        # Pascal string literal: '...' with '' for embedded apostrophe.
        v_escaped = v.replace("'", "''")
        out.append(f"  GL_{k.upper():15} = '{v_escaped}';\n")

    out.append("\n")
    out.append("{ Layout sizes. }\n")
    out.append("const\n")
    for k, v in tokens["heights"].items():
        out.append(f"  HT_{k.upper():15} = {v};\n")
    for k, v in tokens["spacing"].items():
        out.append(f"  SP_{k.upper():15} = {v};\n")

    return "".join(out)


def write_if_changed(path: Path, content: str) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists() and path.read_text() == content:
        return False
    path.write_text(content)
    return True


def main() -> int:
    tokens = load_tokens()

    artifacts = [
        (OUT_PY,  emit_python(tokens), "Python"),
        (OUT_RS,  emit_rust(tokens),   "Rust"),
        (OUT_PAS, emit_pascal(tokens), "Pascal"),
    ]
    for path, content, label in artifacts:
        # python/ and legacy/ are optional archives: a checkout without them
        # (the published tree) only gets the Rust tokens.
        if label != "Rust" and not (REPO / path.relative_to(REPO).parts[0]).is_dir():
            print(f"skipped   {label:8} (archive directory not present)")
            continue
        changed = write_if_changed(path, content)
        rel = path.relative_to(REPO)
        print(f"{'wrote ' if changed else 'unchanged'} {label:8} {rel}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
