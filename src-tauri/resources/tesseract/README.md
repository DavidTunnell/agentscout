# Bundled Tesseract OCR (Windows only)

This directory is populated by the GitHub Actions Windows release build —
`release.yml` runs `choco install tesseract`, then copies `tesseract.exe`
and its DLL dependencies here. Tauri's bundle config (`tauri.conf.json`'s
`bundle.resources`) picks them up and ships them in the MSI under
`<install_dir>\resources\tesseract\`.

At runtime, `src/ocr/tesseract_cli.rs::which_tesseract()` checks this
location first before falling back to `PATH` and common install dirs.

This directory is **not** populated on macOS or Linux installer builds:
- macOS: users install via `brew install tesseract`.
- Linux: the .deb declares `tesseract-ocr` as a dependency so `apt`
  pulls it in automatically.

The actual binaries are gitignored — see `.gitignore` next to this
README — to keep the repo small. Dev builds without an MSI install
should rely on the local-PATH fallback.
