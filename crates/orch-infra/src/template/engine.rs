use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use orch_core::template::{DEFAULT_RENDER_TIMEOUT_MS, PromptContext, TemplateEngine};
use orch_core::{Error, Result};

/// Liquid-backed [`TemplateEngine`].
///
/// `liquid` is synchronous, so the render runs on the blocking pool and is
/// raced against the timeout. As in the TS engine the timeout only abandons the
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
        let mut globals = liquid::to_object(context)
            .map_err(|e| Error::other(format!("serializing the prompt context: {e}")))?;
        let parser = Arc::clone(&self.parser);
        let template = template.to_owned();
        let job = tokio::task::spawn_blocking(move || {
            let parsed = parser
                .parse(&template)
                .map_err(|e| Error::other(e.to_string()))?;
            render_lenient(&parsed, &mut globals)
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

/// Upper bound on distinct undefined names a single template may use.
const MAX_UNDEFINED_NAMES: usize = 64;

/// liquidjs runs with `strictVariables: false`: an undefined variable renders
/// as empty. `liquid` errors instead, so on an "unknown variable" or "unknown
/// index" error bind the offending name to `nil` and render again. This covers
/// undefined root variables and undefined direct properties of a root
/// variable; deeper chains in a user template still fail. The default
/// templates never get here — the context builder defines every variable —
/// this serves user-supplied `config.prompt.*` templates.
fn render_lenient(template: &liquid::Template, globals: &mut liquid::Object) -> Result<String> {
    for _ in 0..=MAX_UNDEFINED_NAMES {
        let message = match template.render(&*globals) {
            Ok(out) => return Ok(out),
            Err(err) => err.to_string(),
        };
        if !bind_undefined(&message, globals) {
            return Err(Error::other(message));
        }
    }
    Err(Error::other(
        "template references too many undefined variables",
    ))
}

/// What a liquid "unknown ..." error complained about.
#[derive(Debug, PartialEq, Eq)]
enum Undefined<'a> {
    Variable(&'a str),
    Index { variable: &'a str, index: &'a str },
}

fn field<'a>(message: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = message.split_once(key)?;
    let value = rest.lines().next()?.trim();
    (!value.is_empty()).then_some(value)
}

fn parse_undefined(message: &str) -> Option<Undefined<'_>> {
    if message.contains("Unknown variable") {
        let name = field(message, "requested variable=")?;
        return (name != "nil").then_some(Undefined::Variable(name));
    }
    if message.contains("Unknown index") {
        return Some(Undefined::Index {
            variable: field(message, "variable=")?,
            index: field(message, "requested index=")?,
        });
    }
    None
}

/// Returns whether the error was one we could fix by binding `nil`.
fn bind_undefined(message: &str, globals: &mut liquid::Object) -> bool {
    use liquid::model::{Object, Value};
    match parse_undefined(message) {
        Some(Undefined::Variable(name)) if !globals.contains_key(name) => {
            globals.insert(name.to_owned().into(), Value::Nil);
            true
        }
        Some(Undefined::Index { variable, index }) => match globals.get_mut(variable) {
            Some(Value::Object(obj)) if !obj.contains_key(index) => {
                obj.insert(index.to_owned().into(), Value::Nil);
                true
            }
            Some(slot @ Value::Nil) => {
                let mut obj = Object::new();
                obj.insert(index.to_owned().into(), Value::Nil);
                *slot = Value::Object(obj);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Undefined, parse_undefined};

    fn render_err(template: &str, globals: &liquid::Object) -> String {
        liquid::ParserBuilder::with_stdlib()
            .build()
            .unwrap()
            .parse(template)
            .unwrap()
            .render(globals)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn parses_the_liquid_unknown_variable_message() {
        let err = render_err("{{ nope }}", &liquid::Object::new());
        assert_eq!(parse_undefined(&err), Some(Undefined::Variable("nope")));
    }

    #[test]
    fn parses_the_liquid_unknown_index_message() {
        let mut globals = liquid::Object::new();
        globals.insert(
            "task".into(),
            liquid::model::Value::Object(liquid::Object::new()),
        );
        let err = render_err("{{ task.missing }}", &globals);
        assert_eq!(
            parse_undefined(&err),
            Some(Undefined::Index {
                variable: "task",
                index: "missing"
            })
        );
    }
}
