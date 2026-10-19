use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use orch_core::template::{DEFAULT_RENDER_TIMEOUT_MS, PromptContext, TemplateEngine};
use orch_core::{Error, Result};

use super::lenient;

/// Liquid-backed [`TemplateEngine`].
///
/// `liquid` is synchronous, so the render runs on the blocking pool and is
/// raced against the timeout. The timeout only abandons the
/// result; it cannot interrupt the render itself.
///
/// Known divergence (ARCHITECTURE §4.3): an unknown filter fails at parse time
/// where liquidjs passes the value through. Only reachable through
/// user-supplied `config.prompt.*` templates.
pub struct LiquidTemplateEngine {
    parser: Arc<liquid::Parser>,
    render_timeout_ms: u64,
}

impl LiquidTemplateEngine {
    pub fn new() -> Self {
        Self::with_render_timeout_ms(DEFAULT_RENDER_TIMEOUT_MS)
    }

    /// `0` disables the timeout.
    pub fn with_render_timeout_ms(render_timeout_ms: u64) -> Self {
        let parser = liquid::ParserBuilder::with_stdlib()
            .build()
            .expect("the liquid stdlib parser always builds");
        Self {
            parser: Arc::new(parser),
            render_timeout_ms,
        }
    }
}

impl Default for LiquidTemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TemplateEngine for LiquidTemplateEngine {
    async fn render(&self, template: &str, context: &PromptContext) -> Result<String> {
        let globals = liquid::to_object(context)
            .map_err(|e| Error::other(format!("serializing the prompt context: {e}")))?;
        let parser = Arc::clone(&self.parser);
        let template = template.to_owned();
        let job = tokio::task::spawn_blocking(move || {
            let parsed = parser
                .parse(&template)
                .map_err(|e| Error::other(e.to_string()))?;
            parsed
                .render(&lenient::Object::root(&globals))
                .map_err(|e| Error::other(e.to_string()))
        });

        let joined = if self.render_timeout_ms == 0 {
            job.await
        } else {
            let limit = Duration::from_millis(self.render_timeout_ms);
            match tokio::time::timeout(limit, job).await {
                Ok(joined) => joined,
                Err(_) => {
                    tracing::warn!(
                        timeout_ms = self.render_timeout_ms,
                        "template render timed out"
                    );
                    return Err(Error::other(format!(
                        "Template render timed out after {}ms",
                        self.render_timeout_ms
                    )));
                }
            }
        };
        joined.map_err(|e| Error::other(format!("template render task failed: {e}")))?
    }
}
