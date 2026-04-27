pub mod tesseract_cli;
pub mod thumbnail;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use tesseract_cli::TesseractCliEngine;
pub use thumbnail::{generate_thumbnail, ThumbnailFormat};

#[async_trait]
pub trait OcrEngine: Send + Sync {
    async fn extract(&self, image_png: &[u8]) -> Result<OcrResult>;
    fn name(&self) -> &str;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrResult {
    pub text: String,
    pub confidence: f32,
    pub engine: String,
}

impl OcrResult {
    pub fn token_count(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// In-memory engine that returns fixed text. For tests and as a graceful
/// fallback when no real OCR backend is available on the system.
pub struct MockEngine {
    text: String,
}

impl MockEngine {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[async_trait]
impl OcrEngine for MockEngine {
    async fn extract(&self, _image_png: &[u8]) -> Result<OcrResult> {
        Ok(OcrResult {
            text: self.text.clone(),
            confidence: 1.0,
            engine: self.name().to_string(),
        })
    }

    fn name(&self) -> &str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_engine_returns_configured_text() {
        let eng = MockEngine::new("hello world");
        let r = eng.extract(b"").await.unwrap();
        assert_eq!(r.text, "hello world");
        assert_eq!(r.token_count(), 2);
        assert_eq!(r.engine, "mock");
    }
}
