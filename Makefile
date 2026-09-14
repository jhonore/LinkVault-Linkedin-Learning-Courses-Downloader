# LinkedVault -- root entry point.
#
# This Makefile is the canonical root entry point. The root package.json is a
# thin shim forwarding to these targets, so existing `npm run ...` habits and
# every command already written in README.md / CONTRIBUTING.md / AGENTS.md keep
# working unchanged.
#
# INVARIANT: every target here delegates to the apps/desktop scripts through
# $(NPM), never to a root-level npm script. Calling a root script would recurse
# back through the shim into make.
#
# Written for GNU Make 3.81 -- the make that ships with macOS. No .ONESHELL,
# no .RECIPEPREFIX, no $(file ...): all of those need 3.82+.
#
# There is no Cargo workspace, so every cargo call must carry --manifest-path.

DESKTOP  := apps/desktop
MANIFEST := $(DESKTOP)/src-tauri/Cargo.toml
NPM      := npm --prefix $(DESKTOP)
CARGO    := cargo

# On macOS the app resolves its archive to <exe_dir>/LinkVaultData, which under
# `tauri dev` lands inside src-tauri/target/debug -- so `make clean` would wipe
# linkvault.sqlite3. Point it somewhere durable instead. Gated on Darwin so
# Windows keeps its portable-install layout untouched.
# Scoped to the targets that actually launch the app: exporting it globally would
# also reach `cargo-test`, and the storage tests drive this same variable
# process-wide, so a value inherited from the environment can change what they
# resolve.
UNAME_S := $(shell uname -s 2>/dev/null)
ifeq ($(UNAME_S),Darwin)
LINKVAULT_DATA_DIR ?= $(HOME)/Library/Application Support/LinkedVault
RUN_ENV := LINKVAULT_DATA_DIR="$(LINKVAULT_DATA_DIR)"
LOG_DIR := $(LINKVAULT_DATA_DIR)/logs
else
RUN_ENV :=
# Portable-install layout: the archive sits next to the dev executable.
LOG_DIR := $(DESKTOP)/src-tauri/target/debug/LinkVaultData/logs
endif

.DEFAULT_GOAL := help

help: ## Show this help
	@echo ''
	@echo 'LinkedVault -- run these from the repo root.'
	@echo ''
	@awk 'BEGIN{FS=":.*?## "} /^[a-zA-Z][a-zA-Z0-9_-]*:.*?## /{printf "  \033[36m%-30s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ''
	@echo 'On Windows cmd/PowerShell set LINKVAULT_DEBUG_LOG=1 yourself, then run make dev.'
	@echo ''
	@echo 'Pass-through forms:'
	@echo '  make tauri ARGS="build --bundle none"   # the Tauri CLI'
	@echo '  make npm SCRIPT=verify:newspaper-clippings   # any apps/desktop script'
	@echo ''

install: ## Install frontend dependencies (npm ci, same as CI)
	npm ci --prefix $(DESKTOP)

doctor: ## Check the local toolchain (node, rust, xcode, chrome)
	@printf 'node     : '; node -v 2>/dev/null || echo 'MISSING'
	@printf 'npm      : '; npm -v 2>/dev/null || echo 'MISSING'
	@printf 'rustc    : '; rustc --version 2>/dev/null || echo 'MISSING'
	@printf 'cargo    : '; cargo --version 2>/dev/null || echo 'MISSING'
	@printf 'make     : '; $(MAKE) --version 2>/dev/null | head -1
	@printf 'xcode    : '; xcode-select -p 2>/dev/null || echo 'MISSING -- run: xcode-select --install'
	@printf 'chrome   : '; test -d '/Applications/Google Chrome.app' && echo 'found' || echo 'absent (verify-visual and *-browser need it, or set PLAYWRIGHT_CHANNEL)'
	@printf 'data dir : '; echo '$(if $(LINKVAULT_DATA_DIR),$(LINKVAULT_DATA_DIR),<exe dir>/LinkVaultData (default))'
	@node -e 'var v=process.versions.node.split(".").map(Number); if(v[0]<22||(v[0]===22&&v[1]<6)) console.log("  ! node >= 22.6 required by the --experimental-strip-types verify scripts");' 2>/dev/null || true
	@echo '  MSRV for the Rust crate is 1.77.2.'

dev: ## Run the desktop app (Tauri dev window)
	$(RUN_ENV) $(NPM) run tauri -- dev

dev-debug: ## Run the app with the diagnostic session log enabled
	$(RUN_ENV) LINKVAULT_DEBUG_LOG=1 $(NPM) run tauri -- dev

logs: ## Show the newest diagnostic session log
	@log=$$(ls -1t "$(LOG_DIR)"/session-*.jsonl 2>/dev/null | head -1); \
	if [ -z "$$log" ]; then echo 'no session log yet -- run: make dev-debug'; \
	else echo "== $$log"; cat "$$log"; fi

logs-path: ## Print the path of the newest diagnostic session log
	@ls -1t "$(LOG_DIR)"/session-*.jsonl 2>/dev/null | head -1

web-dev: ## Run the Vite frontend only, no native window (127.0.0.1:1420)
	$(NPM) run dev

build: ## Type-check and build the frontend (tsc && vite build)
	$(NPM) run build

preview: ## Serve the built frontend (127.0.0.1:1420)
	$(NPM) run preview

tauri: ## Tauri CLI passthrough -- see `make help` for the ARGS= form
	$(RUN_ENV) $(NPM) run tauri -- $(ARGS)

npm: ## Run any apps/desktop script -- see `make help` for the SCRIPT= form
	@test -n "$(SCRIPT)" || { echo 'usage: make npm SCRIPT=<script-name>'; exit 1; }
	$(NPM) run $(SCRIPT)

cargo-test: ## Run the Rust test suite
	$(CARGO) test --manifest-path $(MANIFEST)

cargo-clippy: ## Run clippy (pre-existing warnings are tolerated, do not add new ones)
	$(CARGO) clippy --manifest-path $(MANIFEST) --all-targets

test: cargo-test ## Alias for cargo-test

clippy: cargo-clippy ## Alias for cargo-clippy

verify-architecture: ## Check provider/module layout
	$(NPM) run verify:architecture

verify-no-any: ## Check no explicit `any` in project TypeScript
	$(NPM) run verify:no-any

verify-ui: ## Check newspaper reader / responsive layout
	$(NPM) run verify:ui

verify-youtube-ui: ## Check the YouTube UI surface
	$(NPM) run verify:youtube-ui

verify-linkedin-artifact-row: ## Check the LinkedIn artifact row
	$(NPM) run verify:linkedin-artifact-row

verify-persistence: ## Check the DB writer, migrations and clipping durability
	$(NPM) run verify:persistence

verify-tauri-smoke: ## Rust lib smoke test
	$(NPM) run verify:tauri-smoke

verify-release-manifest: ## Check the version is in sync across the 3 manifests
	$(NPM) run verify:release-manifest

verify-visual: ## [needs Chrome + a running `make web-dev` in LINKVAULT_PREVIEW_URL]
	$(NPM) run verify:visual

verify-persistence-baseline: ## [Windows-recorded baselines -- expected to fail on macOS]
	$(NPM) run verify:persistence-baseline

verify-release: ## [Windows-only: verify-release.mjs hardcodes npm.cmd]
	$(NPM) run verify:release

verify-installer: ## [Windows-only: asserts on NSIS bundle artifacts]
	$(NPM) run verify:installer

clean: ## Remove build output (dist + cargo target). Keeps node_modules and your archive.
	rm -rf $(DESKTOP)/dist
	$(CARGO) clean --manifest-path $(MANIFEST)

distclean: clean ## clean, plus remove node_modules
	rm -rf $(DESKTOP)/node_modules

.PHONY: help install doctor dev dev-debug logs logs-path web-dev build preview tauri npm \
        cargo-test cargo-clippy test clippy \
        verify-architecture verify-no-any verify-ui verify-youtube-ui \
        verify-linkedin-artifact-row verify-persistence verify-tauri-smoke \
        verify-release-manifest verify-visual verify-persistence-baseline \
        verify-release verify-installer clean distclean
