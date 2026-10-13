//! A template engine that renders a constant.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::Result;
use crate::template::{PromptContext, TemplateEngine};
use crate::test_support::faults::Faults;
use crate::test_support::recorder::Recorder;

/// What the TS `createMockTemplateEngine` returns from every `render`.
pub const RENDERED_PROMPT: &str = "rendered prompt";

/// A recorded render: the template source and the context it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeRenderCall {
    pub template: String,
    pub context: Box<PromptContext>,
}

#[derive(Debug, Clone)]
pub struct FakeTemplateEngine {
    output: Arc<Mutex<String>>,
    pub calls: Recorder<FakeRenderCall>,
    pub faults: Faults,
}

impl Default for FakeTemplateEngine {
    fn default() -> Self {
        FakeTemplateEngine::new()
    }
}

impl FakeTemplateEngine {
    pub fn new() -> Self {
        FakeTemplateEngine {
            output: Arc::new(Mutex::new(RENDERED_PROMPT.to_owned())),
            calls: Recorder::new(),
            faults: Faults::new(),
        }
    }

    pub fn returning(output: impl Into<String>) -> Self {
        let engine = FakeTemplateEngine::new();
        engine.set_output(output);
        engine
    }

    pub fn set_output(&self, output: impl Into<String>) {
        *self.output.lock().unwrap_or_else(|e| e.into_inner()) = output.into();
    }

    /// The contexts every render was given, in order.
    pub fn contexts(&self) -> Vec<PromptContext> {
        self.calls
            .calls()
            .into_iter()
            .map(|call| *call.context)
            .collect()
    }
}

#[async_trait]
impl TemplateEngine for FakeTemplateEngine {
    async fn render(&self, template: &str, context: &PromptContext) -> Result<String> {
        self.calls.record(FakeRenderCall {
            template: template.to_owned(),
            context: Box::new(context.clone()),
        });
        self.faults.check()?;
        Ok(self
            .output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{AgentContext, ProjectContext, TaskContext};

    fn context() -> PromptContext {
        PromptContext {
            project: ProjectContext {
                name: "proj".into(),
                description: None,
            },
            task: TaskContext {
                id: "tsk_test1".into(),
                title: "Test task".into(),
                description: "desc".into(),
                priority: 3,
                labels: vec![],
                scope: None,
                is_autonomous: false,
                goal_id: None,
            },
            agent: AgentContext {
                id: "agt_test1".into(),
                name: "TestAgent".into(),
                role: None,
            },
            agents: vec![],
            attempt: None,
            workspace_path: "/tmp/ws".into(),
            retry: None,
            feedback: None,
            shared_context: None,
            messages: None,
            goal: None,
        }
    }

    #[tokio::test]
    async fn returns_the_ts_mock_constant() {
        let engine = FakeTemplateEngine::new();
        assert_eq!(
            engine
                .render("# {{ task.title }}", &context())
                .await
                .unwrap(),
            "rendered prompt"
        );
    }

    #[tokio::test]
    async fn the_output_is_configurable() {
        let engine = FakeTemplateEngine::returning("SYSTEM");
        assert_eq!(
            engine.render("whatever", &context()).await.unwrap(),
            "SYSTEM"
        );
        engine.set_output("USER");
        assert_eq!(engine.render("whatever", &context()).await.unwrap(), "USER");
    }

    #[tokio::test]
    async fn records_the_template_and_the_context() {
        let engine = FakeTemplateEngine::new();
        engine.render("tmpl", &context()).await.unwrap();

        let call = engine.calls.first().expect("one call");
        assert_eq!(call.template, "tmpl");
        assert_eq!(call.context.task.id, "tsk_test1");
        assert_eq!(engine.contexts().len(), 1);
    }

    #[tokio::test]
    async fn faults_fail_the_render() {
        let engine = FakeTemplateEngine::new();
        engine
            .faults
            .fail_once("Template render timed out after 5000ms");
        assert_eq!(
            engine
                .render("tmpl", &context())
                .await
                .unwrap_err()
                .to_string(),
            "Template render timed out after 5000ms"
        );
        assert_eq!(
            engine.render("tmpl", &context()).await.unwrap(),
            "rendered prompt"
        );
    }

    #[tokio::test]
    async fn works_as_a_trait_object() {
        let engine: Arc<dyn TemplateEngine> = Arc::new(FakeTemplateEngine::new());
        assert_eq!(
            engine.render("t", &context()).await.unwrap(),
            "rendered prompt"
        );
    }
}
