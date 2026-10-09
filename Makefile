.PHONY: all sync-tokens python rust legacy test clean help

all: sync-tokens rust

# python/ and legacy/ are archives (design reference); build them explicitly.

# ── codegen ───────────────────────────────────────────────────────────
sync-tokens:
	python3 shared/codegen.py

# ── per-runtime build ────────────────────────────────────────────────
python:
	cd python && uv sync && uv run pytest -q

rust:
	cd rust && cargo build --release

legacy:
	cd legacy && $(MAKE) all

# ── tests ────────────────────────────────────────────────────────────
test:
	cd rust && cargo test

# ── housekeeping ─────────────────────────────────────────────────────
clean:
	cd python && rm -rf .pytest_cache .venv
	cd rust   && cargo clean 2>/dev/null || true
	cd legacy && $(MAKE) clean

help:
	@echo "Top-level targets:"
	@echo "  make sync-tokens — regenerate per-runtime token files from shared/tokens.toml"
	@echo "  make python      — (archive) install deps, run pytest in python/"
	@echo "  make rust        — cargo build --release in rust/"
	@echo "  make legacy      — (archive) build DOS .EXE in legacy/"
	@echo "  make test        — cargo test in rust/"
	@echo "  make clean       — remove build outputs"
	@echo "  make all         — sync-tokens, then build the rust runtime"
	@echo ""
	@echo "Toolchains needed: see INSTALL.md."
