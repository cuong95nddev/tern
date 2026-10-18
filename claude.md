Discuss with me in Vietnamese
The code and comment allway in English
New UI (and any UI you rework) must go through the template render engine
(`crates/tern-app/src/template/`, built-ins in `templates.json`): structure and
look in a template, behavior attached in code by `frame`/`slot`. Do not build new
UI by hand with `div()`. See `docs/superpowers/specs/2026-10-16-ui-templates-design.md`.
