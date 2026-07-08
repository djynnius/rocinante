use std::collections::BTreeMap;
use std::sync::Arc;

use rocinante_providers::ToolSchema;

use super::traits::Tool;

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// The standard toolset for a main agent.
    pub fn core() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(super::ReadTool));
        r.register(Arc::new(super::WriteTool));
        r.register(Arc::new(super::EditTool));
        r.register(Arc::new(super::BashTool));
        r.register(Arc::new(super::GrepTool));
        r.register(Arc::new(super::GlobTool));
        r
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        // Case-insensitive lookup: local models drift on casing.
        self.tools.get(name).or_else(|| {
            self.tools
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    /// Restrict to a named subset (subagent profiles).
    pub fn subset(&self, names: &[String]) -> Self {
        let tools = self
            .tools
            .iter()
            .filter(|(name, _)| names.iter().any(|n| n.eq_ignore_ascii_case(name)))
            .map(|(name, tool)| (*name, Arc::clone(tool)))
            .collect();
        Self { tools }
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.schema(),
            })
            .collect()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }
}
