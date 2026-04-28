use super::{OcrEngine, OcrResult};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{debug, info, warn};

const TESSDATA_URL: &str =
    "https://github.com/tesseract-ocr/tessdata_fast/raw/main/eng.traineddata";

/// Tesseract OCR engine that shells out to the `tesseract` CLI binary.
///
/// Avoids native FFI complexity (libtesseract linking varies wildly by OS).
/// Requires the user to have Tesseract installed:
///   - Windows: <https://github.com/UB-Mannheim/tesseract/wiki>
///   - macOS:   `brew install tesseract`
///   - Linux:   `apt install tesseract-ocr` (or distro equivalent)
///
/// Traineddata is lazy-downloaded into the app data dir on first use, so
/// the bundled language file size doesn't bloat installers.
pub struct TesseractCliEngine {
    binary: PathBuf,
    tessdata_dir: PathBuf,
    language: String,
}

impl TesseractCliEngine {
    pub fn new(tessdata_dir: PathBuf) -> Result<Self> {
        let binary = which_tesseract().context(
            "tesseract CLI not found on PATH; install from \
             https://github.com/UB-Mannheim/tesseract/wiki (Windows), \
             `brew install tesseract` (macOS), or your package manager (Linux)",
        )?;
        Ok(Self {
            binary,
            tessdata_dir,
            language: "eng".to_string(),
        })
    }

    pub async fn ensure_traineddata(&self) -> Result<()> {
        let path = self
            .tessdata_dir
            .join(format!("{}.traineddata", self.language));
        if path.exists() {
            debug!("traineddata already present at {}", path.display());
            return Ok(());
        }

        info!(
            "downloading {}.traineddata to {}",
            self.language,
            path.display()
        );
        std::fs::create_dir_all(&self.tessdata_dir)
            .with_context(|| format!("creating tessdata dir at {}", self.tessdata_dir.display()))?;

        let bytes = reqwest::get(TESSDATA_URL)
            .await
            .context("downloading traineddata")?
            .error_for_status()
            .context("traineddata download returned non-2xx")?
            .bytes()
            .await
            .context("reading traineddata response body")?;

        std::fs::write(&path, &bytes)
            .with_context(|| format!("writing traineddata to {}", path.display()))?;
        info!(
            "downloaded traineddata ({:.1} MB)",
            bytes.len() as f64 / 1_048_576.0
        );
        Ok(())
    }
}

#[async_trait]
impl OcrEngine for TesseractCliEngine {
    async fn extract(&self, image_png: &[u8]) -> Result<OcrResult> {
        self.ensure_traineddata().await?;

        let temp_dir = std::env::temp_dir();
        let id = uuid::Uuid::new_v4();
        let input = temp_dir.join(format!("agentscout-ocr-{id}.png"));
        let output_base = temp_dir.join(format!("agentscout-ocr-{id}"));
        let output_txt = temp_dir.join(format!("agentscout-ocr-{id}.txt"));

        std::fs::write(&input, image_png)
            .with_context(|| format!("writing OCR input to {}", input.display()))?;

        let result = Command::new(&self.binary)
            .arg(&input)
            .arg(&output_base)
            .arg("-l")
            .arg(&self.language)
            .arg("--tessdata-dir")
            .arg(&self.tessdata_dir)
            .arg("--psm")
            .arg("6") // assume a uniform block of text
            .output()
            .await
            .context("invoking tesseract CLI")?;

        // Always clean up input file
        let _ = std::fs::remove_file(&input);

        if !result.status.success() {
            let _ = std::fs::remove_file(&output_txt);
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(anyhow!(
                "tesseract exited with status {:?}: {}",
                result.status.code(),
                stderr.trim()
            ));
        }

        let text = std::fs::read_to_string(&output_txt)
            .with_context(|| format!("reading tesseract output at {}", output_txt.display()))?;
        let _ = std::fs::remove_file(&output_txt);

        // CLI output doesn't surface per-word confidence cheaply; v1 leaves
        // confidence at a coarse heuristic. Phase 2 can shell out a second
        // call with `tsv` config for word-level confidence if needed.
        let confidence = if text.trim().is_empty() { 0.0 } else { 0.85 };

        Ok(OcrResult {
            text: text.trim().to_string(),
            confidence,
            engine: self.name().to_string(),
        })
    }

    fn name(&self) -> &str {
        "tesseract-cli"
    }
}

fn which_tesseract() -> Result<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![
            "tesseract.exe",
            "C:\\Program Files\\Tesseract-OCR\\tesseract.exe",
            "C:\\Program Files (x86)\\Tesseract-OCR\\tesseract.exe",
        ]
    } else {
        vec![
            "tesseract",
            "/usr/local/bin/tesseract",
            "/opt/homebrew/bin/tesseract",
            "/usr/bin/tesseract",
        ]
    };

    for cand in &candidates {
        let path = Path::new(cand);
        if path.is_absolute() && path.exists() {
            return Ok(path.to_path_buf());
        }
        // Fall back to PATH lookup via simple name
        if !path.is_absolute() {
            if let Ok(found) = which(cand) {
                return Ok(found);
            }
        }
    }

    Err(anyhow!(
        "tesseract not found in PATH or common install locations"
    ))
}

fn which(name: &str) -> Result<PathBuf> {
    let path_var = std::env::var_os("PATH").context("PATH env var missing")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        // On Windows, also check with .exe suffix if not already present
        if cfg!(windows) && !name.ends_with(".exe") {
            let with_ext = dir.join(format!("{name}.exe"));
            if with_ext.is_file() {
                return Ok(with_ext);
            }
        }
    }
    warn!("'{}' not found on PATH", name);
    Err(anyhow!("'{}' not found on PATH", name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_returns_err_for_nonexistent_binary() {
        let result = which("definitely-not-a-real-binary-xyz123");
        assert!(result.is_err());
    }

    // Note: a previous "engine_construction_fails_gracefully_when_tesseract_missing"
    // test cleared PATH and asserted construction errored. It was unreliable
    // across environments — Linux runners ship `/usr/bin/tesseract` which the
    // absolute-path fallback in `which_tesseract` finds even with empty PATH.
    // The underlying primitive is covered by `which_returns_err_for_nonexistent_binary`.
}
