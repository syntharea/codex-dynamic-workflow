use std::collections::BTreeMap;
use std::io;

use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::SavedWorkflowDefinition;
use codex_workflows::WORKFLOW_SCHEMA_VERSION;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowValidationError;
use serde_json::Value;
use thiserror::Error;

use crate::config::Config;

use super::saved_fs::SavedWorkflowRoot;

const SAVED_WORKFLOW_MAX_BYTES: u64 = 256 * 1024;
const SAVED_WORKFLOW_MAX_COUNT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SavedWorkflowProjectTrust {
    Trusted,
    Untrusted,
}

#[derive(Debug)]
pub(crate) struct SavedWorkflowStore {
    global_root: SavedWorkflowRoot,
    project_root: Option<SavedWorkflowRoot>,
    limits: WorkflowLimits,
}

impl SavedWorkflowStore {
    pub(crate) fn from_config(
        config: &Config,
        limits: WorkflowLimits,
    ) -> Result<Self, SavedWorkflowStoreError> {
        if let Some(project_root) = config.git_checkout_root.as_ref() {
            let trust = if config.git_checkout_root_is_trusted {
                SavedWorkflowProjectTrust::Trusted
            } else {
                SavedWorkflowProjectTrust::Untrusted
            };
            Self::for_project(&config.codex_home, project_root, trust, limits)
        } else {
            Self::global(&config.codex_home, limits)
        }
    }

    pub(crate) fn global(
        codex_home: &AbsolutePathBuf,
        limits: WorkflowLimits,
    ) -> Result<Self, SavedWorkflowStoreError> {
        Ok(Self {
            global_root: SavedWorkflowRoot::new(codex_home, ["workflows"])?,
            project_root: None,
            limits,
        })
    }

    pub(crate) fn for_project(
        codex_home: &AbsolutePathBuf,
        project_root: &AbsolutePathBuf,
        trust: SavedWorkflowProjectTrust,
        limits: WorkflowLimits,
    ) -> Result<Self, SavedWorkflowStoreError> {
        let mut store = Self::global(codex_home, limits)?;
        store.project_root = match trust {
            SavedWorkflowProjectTrust::Trusted => Some(SavedWorkflowRoot::new(
                project_root,
                [".codex", "workflows"],
            )?),
            SavedWorkflowProjectTrust::Untrusted => None,
        };
        Ok(store)
    }

    pub(crate) fn read(
        &self,
        name: &str,
    ) -> Result<Option<SavedWorkflowDefinition>, SavedWorkflowStoreError> {
        validate_name(name)?;
        if let Some(project_root) = self.project_root.as_ref()
            && let Some(saved) = self.read_from_root(project_root, name)?
        {
            return Ok(Some(saved));
        }
        self.read_from_root(&self.global_root, name)
    }

    pub(crate) fn list(&self) -> Result<Vec<SavedWorkflowDefinition>, SavedWorkflowStoreError> {
        let mut effective = BTreeMap::new();
        for name in self.global_root.names(SAVED_WORKFLOW_MAX_COUNT)? {
            effective.insert(name, &self.global_root);
        }
        if let Some(project_root) = self.project_root.as_ref() {
            for name in project_root.names(SAVED_WORKFLOW_MAX_COUNT)? {
                effective.insert(name, project_root);
                if effective.len() > SAVED_WORKFLOW_MAX_COUNT {
                    return Err(SavedWorkflowStoreError::TooManyWorkflows {
                        max_count: SAVED_WORKFLOW_MAX_COUNT,
                    });
                }
            }
        }

        effective
            .into_iter()
            .map(|(name, root)| {
                self.read_from_root(root, &name)?.ok_or_else(|| {
                    io_error(
                        &workflow_path(root.path(), &name),
                        io::Error::new(io::ErrorKind::NotFound, "saved workflow file disappeared"),
                    )
                })
            })
            .collect()
    }

    pub(crate) fn save(
        &self,
        saved: &SavedWorkflowDefinition,
    ) -> Result<(), SavedWorkflowStoreError> {
        validate_name(&saved.name)?;
        let path = workflow_path(self.global_root.path(), &saved.name);
        self.validate_saved(&path, &saved.name, saved)?;
        let contents = serde_json::to_vec_pretty(saved).map_err(|source| {
            SavedWorkflowStoreError::Serialize {
                name: saved.name.clone(),
                source,
            }
        })?;
        let actual_bytes = contents.len() as u64;
        if actual_bytes > SAVED_WORKFLOW_MAX_BYTES {
            return Err(SavedWorkflowStoreError::FileTooLarge {
                path,
                actual_bytes,
                max_bytes: SAVED_WORKFLOW_MAX_BYTES,
            });
        }

        self.global_root
            .save(&saved.name, &contents, SAVED_WORKFLOW_MAX_COUNT)
    }

    pub(crate) fn delete(&self, name: &str) -> Result<bool, SavedWorkflowStoreError> {
        validate_name(name)?;
        self.global_root.delete(name)
    }

    fn read_from_root(
        &self,
        root: &SavedWorkflowRoot,
        name: &str,
    ) -> Result<Option<SavedWorkflowDefinition>, SavedWorkflowStoreError> {
        let Some(contents) = root.read(name, SAVED_WORKFLOW_MAX_BYTES)? else {
            return Ok(None);
        };
        self.parse_file(root.path(), name, &contents).map(Some)
    }

    fn parse_file(
        &self,
        root: &AbsolutePathBuf,
        expected_name: &str,
        contents: &[u8],
    ) -> Result<SavedWorkflowDefinition, SavedWorkflowStoreError> {
        let path = workflow_path(root, expected_name);
        let value: Value = serde_json::from_slice(contents)
            .map_err(|source| invalid_json_error(&path, source.to_string()))?;
        let schema_version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| invalid_json_error(&path, "schema_version must be a u32".to_string()))?;
        if schema_version != WORKFLOW_SCHEMA_VERSION {
            return Err(SavedWorkflowStoreError::UnsupportedSchemaVersion {
                path,
                found: schema_version,
            });
        }
        if let Some(definition_schema_version) = value
            .get("definition")
            .and_then(|definition| definition.get("schema_version"))
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            && definition_schema_version != WORKFLOW_SCHEMA_VERSION
        {
            return Err(SavedWorkflowStoreError::InvalidDefinition {
                path,
                source: WorkflowValidationError::UnsupportedSchemaVersion(
                    definition_schema_version,
                ),
            });
        }

        let saved = serde_json::from_value(value)
            .map_err(|source| invalid_json_error(&path, source.to_string()))?;
        self.validate_saved(&path, expected_name, &saved)?;
        Ok(saved)
    }

    fn validate_saved(
        &self,
        path: &AbsolutePathBuf,
        expected_name: &str,
        saved: &SavedWorkflowDefinition,
    ) -> Result<(), SavedWorkflowStoreError> {
        if saved.schema_version != WORKFLOW_SCHEMA_VERSION {
            return Err(SavedWorkflowStoreError::UnsupportedSchemaVersion {
                path: path.clone(),
                found: saved.schema_version,
            });
        }
        if saved.name != expected_name {
            return Err(SavedWorkflowStoreError::NameMismatch {
                path: path.clone(),
                expected: expected_name.to_string(),
                actual: saved.name.clone(),
            });
        }
        saved.definition.validate(self.limits).map_err(|source| {
            SavedWorkflowStoreError::InvalidDefinition {
                path: path.clone(),
                source,
            }
        })
    }
}

pub(super) fn validate_name(name: &str) -> Result<(), SavedWorkflowStoreError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SavedWorkflowStoreError::InvalidName {
            name: name.to_string(),
        });
    }
    Ok(())
}

pub(super) fn workflow_path(root: &AbsolutePathBuf, name: &str) -> AbsolutePathBuf {
    root.join(format!("{name}.json"))
}

fn invalid_json_error(path: &AbsolutePathBuf, message: String) -> SavedWorkflowStoreError {
    SavedWorkflowStoreError::InvalidJson {
        path: path.clone(),
        message,
    }
}

pub(super) fn io_error(path: &AbsolutePathBuf, source: io::Error) -> SavedWorkflowStoreError {
    SavedWorkflowStoreError::Io {
        path: path.clone(),
        source,
    }
}

#[derive(Debug, Error)]
pub(crate) enum SavedWorkflowStoreError {
    #[error("invalid saved workflow name {name:?}")]
    InvalidName { name: String },
    #[error("saved workflow root is a symlink: {}", path.display())]
    SymlinkRoot { path: AbsolutePathBuf },
    #[error("saved workflow file is a symlink: {}", path.display())]
    SymlinkFile { path: AbsolutePathBuf },
    #[error(
        "saved workflow file {} is {actual_bytes} bytes, exceeding the {max_bytes}-byte limit",
        path.display()
    )]
    FileTooLarge {
        path: AbsolutePathBuf,
        actual_bytes: u64,
        max_bytes: u64,
    },
    #[error("saved workflow list exceeds the limit of {max_count} entries")]
    TooManyWorkflows { max_count: usize },
    #[error("saved workflow file {} contains invalid JSON: {message}", path.display())]
    InvalidJson {
        path: AbsolutePathBuf,
        message: String,
    },
    #[error(
        "saved workflow file {} uses unsupported schema version {found}",
        path.display()
    )]
    UnsupportedSchemaVersion { path: AbsolutePathBuf, found: u32 },
    #[error(
        "saved workflow file {} declares name {actual:?}; expected {expected:?}",
        path.display()
    )]
    NameMismatch {
        path: AbsolutePathBuf,
        expected: String,
        actual: String,
    },
    #[error("saved workflow definition in {} is invalid: {source}", path.display())]
    InvalidDefinition {
        path: AbsolutePathBuf,
        #[source]
        source: WorkflowValidationError,
    },
    #[error("failed to serialize saved workflow {name:?}: {source}")]
    Serialize {
        name: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("saved workflow I/O failed at {}: {source}", path.display())]
    Io {
        path: AbsolutePathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
#[path = "saved_tests.rs"]
mod tests;
