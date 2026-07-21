use super::spawn::SpawnInitialInput;
use super::*;
use crate::CodexThread;
use crate::agent::agent_status_from_event;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::workflow::WorkflowStateItem;
use codex_workflows::WorkflowDispatchMarker;
use codex_workflows::WorkflowSnapshot;

fn persisted_workflow_dispatch_marker(
    items: &[RolloutItem],
    root_thread_id: ThreadId,
    agent_thread_id: ThreadId,
) -> Option<(usize, WorkflowDispatchMarker)> {
    let mut found = None;
    for (index, item) in items.iter().enumerate() {
        let RolloutItem::SessionMeta(meta_line) = item else {
            continue;
        };
        let SessionSource::Custom(source) = &meta_line.meta.source else {
            continue;
        };
        let marker = match WorkflowDispatchMarker::decode(source) {
            Ok(Some(marker)) => marker,
            Ok(None) => continue,
            Err(_) => return None,
        };
        let marker_agent_id = marker
            .agent_id()
            .and_then(|agent_id| ThreadId::try_from(agent_id.as_str()).ok());
        if meta_line.meta.id != root_thread_id
            || meta_line.meta.parent_thread_id != Some(root_thread_id)
            || marker_agent_id != Some(agent_thread_id)
            || found.is_some()
        {
            return None;
        }
        found = Some((index, marker));
    }
    found
}

impl AgentControl {
    pub(super) async fn persist_workflow_dispatch_marker(
        &self,
        state: &Arc<ThreadManagerState>,
        child_thread: &CodexThread,
        child_thread_id: ThreadId,
        session_source: Option<&SessionSource>,
        workflow_dispatch: &WorkflowDispatchMarker,
    ) -> CodexResult<()> {
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id, ..
        })) = session_source
        else {
            return Err(CodexErr::Fatal(
                "workflow dispatch requires a thread-spawn session source".to_string(),
            ));
        };
        if child_thread.config_snapshot().await.ephemeral {
            return Err(CodexErr::Fatal(
                "workflow dispatch requires durable agent persistence".to_string(),
            ));
        }
        let agent_graph_store = state.agent_graph_store().ok_or_else(|| {
            CodexErr::Fatal("workflow dispatch graph persistence is unavailable".to_string())
        })?;
        agent_graph_store
            .upsert_thread_spawn_edge(
                *parent_thread_id,
                child_thread_id,
                codex_agent_graph_store::ThreadSpawnEdgeStatus::Open,
            )
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!(
                    "failed to persist workflow dispatch graph edge: {error}"
                ))
            })?;
        let marker = workflow_dispatch
            .clone()
            .bind_agent(codex_workflows::WorkflowAgentId::from(
                child_thread_id.to_string(),
            ))
            .map_err(|error| CodexErr::Fatal(error.to_string()))?;
        let encoded = marker
            .encode()
            .map_err(|error| CodexErr::Fatal(error.to_string()))?;
        let marker_item = RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                session_id: (*parent_thread_id).into(),
                id: *parent_thread_id,
                parent_thread_id: Some(*parent_thread_id),
                source: SessionSource::Custom(encoded),
                ..Default::default()
            },
            git: None,
        });
        let persist_result = async {
            child_thread
                .session
                .persist_workflow_rollout_items_checked(&[marker_item])
                .await?;
            child_thread.flush_rollout().await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = persist_result {
            let _ = agent_graph_store
                .set_thread_spawn_edge_status(
                    child_thread_id,
                    codex_agent_graph_store::ThreadSpawnEdgeStatus::Closed,
                )
                .await;
            return Err(CodexErr::Fatal(format!(
                "failed to persist workflow dispatch marker: {error}"
            )));
        }
        Ok(())
    }

    pub(crate) async fn workflow_agent_outcomes(
        &self,
        root_thread_id: ThreadId,
        snapshot: &codex_workflows::WorkflowSnapshot,
    ) -> anyhow::Result<Vec<crate::workflows::WorkflowTerminalEvidence>> {
        let state = self.upgrade()?;
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return Ok(Vec::new());
        };
        let descendant_ids = agent_graph_store
            .list_thread_spawn_descendants(root_thread_id, /*status_filter*/ None)
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to read persisted workflow agent graph: {error}")
            })?;
        let mut binding_counts = std::collections::BTreeMap::new();
        for agent_id in snapshot
            .tasks
            .iter()
            .filter_map(|task| task.agent_id.as_ref())
        {
            *binding_counts.entry(agent_id).or_insert(0usize) += 1;
        }
        let mut outcomes = Vec::new();
        for task in &snapshot.tasks {
            let Some(agent_id) = task.agent_id.as_ref() else {
                continue;
            };
            if binding_counts.get(agent_id) != Some(&1) {
                continue;
            }
            let Ok(thread_id) = ThreadId::try_from(agent_id.as_str()) else {
                continue;
            };
            if !descendant_ids.contains(&thread_id) {
                continue;
            }
            let stored_thread = match state
                .read_stored_thread(ReadThreadParams {
                    thread_id,
                    include_archived: true,
                    include_history: true,
                })
                .await
            {
                Ok(stored_thread) => stored_thread,
                Err(CodexErr::ThreadNotFound(_)) => continue,
                Err(error) => return Err(anyhow::Error::new(error)),
            };
            let Some(history) = stored_thread.history else {
                continue;
            };
            let Some((marker_index, marker)) =
                persisted_workflow_dispatch_marker(&history.items, root_thread_id, thread_id)
            else {
                continue;
            };
            if marker.workflow_id() != snapshot.workflow_id
                || marker.task_id() != &task.task_id
                || marker.execution_attempt() != snapshot.execution_attempt
                || marker.task_attempt() != task.attempts
            {
                continue;
            }
            let Some(status) = history.items[marker_index + 1..]
                .iter()
                .rev()
                .find_map(|item| match item {
                    RolloutItem::EventMsg(event) => agent_status_from_event(event),
                    _ => None,
                })
            else {
                continue;
            };
            let outcome = match status {
                AgentStatus::Completed(summary) => {
                    crate::workflows::WorkflowTerminalEvidence::succeeded(
                        marker,
                        agent_id.clone(),
                        summary.unwrap_or_default(),
                    )
                }
                AgentStatus::Errored(summary) => {
                    crate::workflows::WorkflowTerminalEvidence::failed(
                        marker,
                        agent_id.clone(),
                        summary,
                        /*retryable*/ true,
                    )
                }
                AgentStatus::Interrupted
                | AgentStatus::Shutdown
                | AgentStatus::PendingInit
                | AgentStatus::Running
                | AgentStatus::NotFound => continue,
            };
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }

    pub(crate) async fn prepare_workflow_agent_with_metadata(
        &self,
        config: Config,
        session_source: Option<SessionSource>,
        options: SpawnAgentOptions,
        workflow_dispatch: WorkflowDispatchMarker,
    ) -> CodexResult<LiveAgent> {
        Box::pin(self.spawn_agent_internal(
            config,
            SpawnInitialInput::DeferredWorkflow,
            session_source,
            options,
            Some(workflow_dispatch),
        ))
        .await
    }

    pub(crate) async fn persist_workflow_dispatch_snapshot(
        &self,
        root_thread_id: ThreadId,
        snapshot: &WorkflowSnapshot,
    ) -> CodexResult<()> {
        let state = self.upgrade()?;
        let root_thread = state.get_thread(root_thread_id).await?;
        let snapshot_value = serde_json::to_value(snapshot).map_err(|error| {
            CodexErr::Fatal(format!(
                "failed to serialize workflow dispatch snapshot: {error}"
            ))
        })?;
        let state_item = WorkflowStateItem::new(snapshot.workflow_id.to_string(), snapshot_value)
            .map_err(|error| {
            CodexErr::Fatal(format!(
                "failed to encode workflow dispatch snapshot: {error}"
            ))
        })?;
        root_thread
            .session
            .persist_workflow_rollout_items_checked(&[RolloutItem::WorkflowState(state_item)])
            .await
            .map_err(|error| {
                CodexErr::Fatal(format!(
                    "failed to persist workflow dispatch snapshot: {error}"
                ))
            })?;
        root_thread.flush_rollout().await.map_err(|error| {
            CodexErr::Fatal(format!(
                "failed to flush workflow dispatch snapshot: {error}"
            ))
        })
    }

    pub(crate) async fn start_prepared_workflow_agent(
        &self,
        thread_id: ThreadId,
        initial_input: Vec<UserInput>,
    ) -> CodexResult<()> {
        let state = self.upgrade()?;
        state.get_thread(thread_id).await?;
        state.notify_thread_created(thread_id);
        self.send_input_after_capacity_check(thread_id, &state, initial_input)
            .await
            .map(|_| ())
    }
}
