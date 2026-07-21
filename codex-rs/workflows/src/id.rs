use std::fmt;

use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowId(Uuid);

impl WorkflowId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for WorkflowId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkflowId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowTaskId(String);

impl WorkflowTaskId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for WorkflowTaskId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for WorkflowTaskId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for WorkflowTaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowAgentId(String);

impl WorkflowAgentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for WorkflowAgentId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for WorkflowAgentId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for WorkflowAgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
