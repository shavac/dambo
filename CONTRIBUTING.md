# Contributing to `dambo`

Thank you for your interest in contributing to **dambo**!

## Development Setup & Verification

Before opening a Pull Request, please run the full local verification suite from the repository root:

```bash
# 1. Check formatting
cargo fmt --all -- --check

# 2. Run strict Clippy lints
cargo clippy --workspace --all-targets -- -D warnings

# 3. Run all unit and integration tests
cargo test --workspace
```

For JS/JSON changes in `ui/` or `static/html/`, also verify syntax:

```bash
node --check ui/app.js
find static/html -name '*.js' -exec node --check {} +
```

## Plugin Development Conventions (`static/html/`)

All built-in and community plugins under `static/html/<plugin-id>/` must remain **100% pure browser, no-build web assets** compatible with both OBS Browser Source and desktop overlay windows:

- **Standard 4-File Layout** (plus optional `assets/`):
  ```text
  static/html/<plugin-id>/
  ├── index.html      # [REQUIRED] Entry HTML with <meta name="referrer" content="no-referrer"> and <base href="/<plugin-id>/">
  ├── plugin.json     # [RECOMMENDED] Metadata & OBS/Desktop overlay dimensions
  ├── plugin.css      # [OPTIONAL] Stylesheet (referenced as ./plugin.css)
  ├── plugin.js       # [OPTIONAL] Logic (referenced as ./plugin.js, loaded after synchronous /sdk/dambo.js)
  └── assets/         # [OPTIONAL] Static images, fonts, or audio (referenced as ./assets/<file>)
  ```
- **Strict Pure-Web Isolation**: Never use `window.__TAURI__`, Node.js APIs, `require()`, bare `npm` imports, or `.innerHTML` with user-supplied text.
- See [docs/PLUGIN_SPEC.en.md](docs/PLUGIN_SPEC.en.md) / [docs/PLUGIN_SPEC.md](docs/PLUGIN_SPEC.md) for full SDK contracts and self-verification steps.

## Pull Request Guidelines

- Keep PRs focused and small; avoid mixing unrelated refactors with feature/fix commits.
- Never commit runtime artifacts under `var/`, `etc/config.toml`, `*.local.toml`, or personal session/cookie files (copy `etc/config.sample.toml` to `etc/config.toml` for local configuration).
- Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in `dambo` shall be dual-licensed under `MIT OR Apache-2.0`, without any additional terms or conditions.
