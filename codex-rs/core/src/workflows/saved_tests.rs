use std::fs;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::SavedWorkflowDefinition;
use codex_workflows::WorkflowDefinition;
use codex_workflows::WorkflowLimits;
use codex_workflows::WorkflowTask;
use codex_workflows::WorkflowTaskId;
use codex_workflows::WorkflowTaskIntent;
use codex_workflows::WorkflowValidationError;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::SAVED_WORKFLOW_MAX_BYTES;
use super::SAVED_WORKFLOW_MAX_COUNT;
use super::SavedWorkflowProjectTrust;
use super::SavedWorkflowStore;
use super::SavedWorkflowStoreError;

#[test]
fn global_crud_and_atomic_replace() {
    let codex_home = TempDir::new().expect("create Codex home");
    let store = global_store(&codex_home, WorkflowLimits::default());

    let first = saved("demo", "First");
    store.save(&first).expect("save workflow");
    assert_eq!(store.read("demo").expect("read workflow"), Some(first));

    let workflow_path = codex_home.path().join("workflows/demo.json");
    #[cfg(unix)]
    let original_inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&workflow_path)
            .expect("workflow metadata")
            .ino()
    };

    let replacement = saved("demo", "Replacement");
    store.save(&replacement).expect("replace workflow");
    assert_eq!(
        store.read("demo").expect("read replacement"),
        Some(replacement)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let replacement_inode = fs::metadata(&workflow_path)
            .expect("replacement metadata")
            .ino();
        assert_ne!(replacement_inode, original_inode);
    }

    assert!(store.delete("demo").expect("delete workflow"));
    assert_eq!(store.read("demo").expect("read deleted workflow"), None);
}

#[test]
fn trusted_project_overrides_global_while_writes_remain_global() {
    let codex_home = TempDir::new().expect("create Codex home");
    let project = TempDir::new().expect("create project");
    let global = global_store(&codex_home, WorkflowLimits::default());
    global
        .save(&saved("shared", "Global"))
        .expect("save global workflow");
    write_saved(
        &project.path().join(".codex/workflows"),
        &saved("shared", "Project"),
    );

    let store = project_store(&codex_home, &project, SavedWorkflowProjectTrust::Trusted);
    assert_eq!(
        store.read("shared").expect("read effective workflow"),
        Some(saved("shared", "Project"))
    );
    assert_eq!(
        store.list().expect("list effective workflows"),
        vec![saved("shared", "Project")]
    );

    store
        .save(&saved("shared", "New Global"))
        .expect("replace global workflow");
    assert_eq!(
        global.read("shared").expect("read replaced global"),
        Some(saved("shared", "New Global"))
    );

    assert!(store.delete("shared").expect("delete global workflow"));
    assert_eq!(global.read("shared").expect("global was deleted"), None);
    assert_eq!(
        store.read("shared").expect("project still exists"),
        Some(saved("shared", "Project"))
    );
}

#[test]
fn untrusted_project_store_is_completely_ignored() {
    let codex_home = TempDir::new().expect("create Codex home");
    let project = TempDir::new().expect("create project");
    let global = global_store(&codex_home, WorkflowLimits::default());
    global
        .save(&saved("shared", "Global"))
        .expect("save global workflow");
    write_saved(
        &project.path().join(".codex/workflows"),
        &saved("shared", "Project"),
    );
    fs::write(
        project.path().join(".codex/workflows/corrupt.json"),
        "not json",
    )
    .expect("write corrupt project workflow");

    let store = project_store(&codex_home, &project, SavedWorkflowProjectTrust::Untrusted);
    assert_eq!(
        store.list().expect("list ignores project"),
        vec![saved("shared", "Global")]
    );
}

#[test]
fn rejects_invalid_names_without_cleaning_them() {
    let codex_home = TempDir::new().expect("create Codex home");
    let store = global_store(&codex_home, WorkflowLimits::default());

    for name in ["", ".", "..", "../escape", "a/b", r"a\b", "a.b", "a b", "é"] {
        assert!(matches!(
            store.save(&saved(name, "Invalid")),
            Err(SavedWorkflowStoreError::InvalidName { name: rejected }) if rejected == name
        ));
    }
    assert!(store.read("../escape").is_err());
    assert!(store.delete("../escape").is_err());

    assert!(!codex_home.path().join("workflows").exists());
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_workflows_root() {
    use std::os::unix::fs::symlink;

    let codex_home = TempDir::new().expect("create Codex home");
    let outside = TempDir::new().expect("create outside directory");
    symlink(outside.path(), codex_home.path().join("workflows")).expect("symlink workflows root");
    let store = global_store(&codex_home, WorkflowLimits::default());

    assert!(matches!(
        store.save(&saved("safe", "Safe")),
        Err(SavedWorkflowStoreError::SymlinkRoot { .. })
    ));
    assert_eq!(
        fs::read_dir(outside.path()).expect("read outside").count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_codex_home_anchor() {
    use std::os::unix::fs::symlink;

    let parent = TempDir::new().expect("create parent");
    let outside = TempDir::new().expect("create outside directory");
    let codex_home = parent.path().join("codex-home");
    symlink(outside.path(), &codex_home).expect("symlink Codex home");

    assert!(matches!(
        SavedWorkflowStore::global(&absolute(&codex_home), WorkflowLimits::default()),
        Err(SavedWorkflowStoreError::SymlinkRoot { .. })
    ));
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_project_root_anchor() {
    use std::os::unix::fs::symlink;

    let codex_home = TempDir::new().expect("create Codex home");
    let parent = TempDir::new().expect("create parent");
    let outside = TempDir::new().expect("create outside directory");
    let project = parent.path().join("project");
    symlink(outside.path(), &project).expect("symlink project root");

    assert!(matches!(
        SavedWorkflowStore::for_project(
            &absolute(codex_home.path()),
            &absolute(&project),
            SavedWorkflowProjectTrust::Trusted,
            WorkflowLimits::default(),
        ),
        Err(SavedWorkflowStoreError::SymlinkRoot { .. })
    ));
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_project_metadata_directory() {
    use std::os::unix::fs::symlink;

    let codex_home = TempDir::new().expect("create Codex home");
    let project = TempDir::new().expect("create project");
    let outside = TempDir::new().expect("create outside directory");
    write_saved(
        &outside.path().join("workflows"),
        &saved("linked", "Outside"),
    );
    symlink(outside.path(), project.path().join(".codex")).expect("symlink project metadata");
    let store = project_store(&codex_home, &project, SavedWorkflowProjectTrust::Trusted);

    assert!(store.read("linked").is_err());
}

#[cfg(unix)]
#[test]
fn anchors_codex_home_before_a_path_swap() {
    use std::os::unix::fs::symlink;

    let parent = TempDir::new().expect("create parent");
    let codex_home = parent.path().join("codex-home");
    let moved_home = parent.path().join("moved-home");
    let outside = TempDir::new().expect("create outside directory");
    fs::create_dir(&codex_home).expect("create Codex home");
    let store = SavedWorkflowStore::global(&absolute(&codex_home), WorkflowLimits::default())
        .expect("open saved workflow store");

    fs::rename(&codex_home, &moved_home).expect("move Codex home");
    symlink(outside.path(), &codex_home).expect("replace Codex home with symlink");
    store
        .save(&saved("safe", "Safe"))
        .expect("save through anchored directory");

    assert!(moved_home.join("workflows/safe.json").is_file());
    assert_eq!(
        fs::read_dir(outside.path()).expect("read outside").count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_workflow_file() {
    use std::os::unix::fs::symlink;

    let codex_home = TempDir::new().expect("create Codex home");
    let outside = TempDir::new().expect("create outside directory");
    let root = codex_home.path().join("workflows");
    fs::create_dir_all(&root).expect("create workflows root");
    let outside_file = outside.path().join("outside.json");
    let outside_contents =
        serde_json::to_string(&saved("linked", "Outside")).expect("serialize outside workflow");
    fs::write(&outside_file, &outside_contents).expect("write outside workflow");
    symlink(&outside_file, root.join("linked.json")).expect("symlink workflow file");
    let store = global_store(&codex_home, WorkflowLimits::default());

    assert!(matches!(
        store.read("linked"),
        Err(SavedWorkflowStoreError::SymlinkFile { .. })
    ));
    assert!(matches!(
        store.save(&saved("linked", "Replacement")),
        Err(SavedWorkflowStoreError::SymlinkFile { .. })
    ));
    assert_eq!(
        fs::read_to_string(outside_file).expect("read outside workflow"),
        outside_contents
    );
}

#[test]
fn reports_corrupt_json_and_unknown_schema_separately() {
    let codex_home = TempDir::new().expect("create Codex home");
    let root = codex_home.path().join("workflows");
    fs::create_dir_all(&root).expect("create workflows root");
    fs::write(root.join("corrupt.json"), "{not json").expect("write corrupt JSON");

    fs::write(root.join("unknown.json"), r#"{"schema_version":999}"#)
        .expect("write unknown schema workflow");
    let store = global_store(&codex_home, WorkflowLimits::default());

    assert!(matches!(
        store.read("corrupt"),
        Err(SavedWorkflowStoreError::InvalidJson { .. })
    ));
    assert!(matches!(
        store.read("unknown"),
        Err(SavedWorkflowStoreError::UnsupportedSchemaVersion { found: 999, .. })
    ));
}

#[test]
fn validates_definitions_with_store_limits_on_save_and_read() {
    let codex_home = TempDir::new().expect("create Codex home");
    let limits = WorkflowLimits {
        max_tasks: 0,
        ..WorkflowLimits::default()
    };
    let store = global_store(&codex_home, limits);
    let saved = saved("limited", "Limited");

    assert!(matches!(
        store.save(&saved),
        Err(SavedWorkflowStoreError::InvalidDefinition {
            source: WorkflowValidationError::TooManyTasks { max_tasks: 0 },
            ..
        })
    ));

    write_saved(&codex_home.path().join("workflows"), &saved);
    assert!(matches!(
        store.read("limited"),
        Err(SavedWorkflowStoreError::InvalidDefinition {
            source: WorkflowValidationError::TooManyTasks { max_tasks: 0 },
            ..
        })
    ));
}

#[test]
fn enforces_saved_file_size_limit_before_parsing() {
    let codex_home = TempDir::new().expect("create Codex home");
    let root = codex_home.path().join("workflows");
    fs::create_dir_all(&root).expect("create workflows root");
    fs::write(
        root.join("large.json"),
        vec![b' '; SAVED_WORKFLOW_MAX_BYTES as usize + 1],
    )
    .expect("write oversized workflow");
    let store = global_store(&codex_home, WorkflowLimits::default());

    assert!(matches!(
        store.read("large"),
        Err(SavedWorkflowStoreError::FileTooLarge {
            actual_bytes,
            max_bytes: SAVED_WORKFLOW_MAX_BYTES,
            ..
        }) if actual_bytes == SAVED_WORKFLOW_MAX_BYTES + 1
    ));
}

#[test]
fn enforces_list_count_limit() {
    let codex_home = TempDir::new().expect("create Codex home");
    let root = codex_home.path().join("workflows");
    for index in 0..=SAVED_WORKFLOW_MAX_COUNT {
        let name = format!("workflow_{index:03}");
        write_saved(&root, &saved(&name, &name));
    }
    let store = global_store(&codex_home, WorkflowLimits::default());

    assert!(matches!(
        store.list(),
        Err(SavedWorkflowStoreError::TooManyWorkflows {
            max_count: SAVED_WORKFLOW_MAX_COUNT,
            ..
        })
    ));
}

#[test]
fn enforces_count_limit_when_saving_new_workflow() {
    let codex_home = TempDir::new().expect("create Codex home");
    let root = codex_home.path().join("workflows");
    for index in 0..SAVED_WORKFLOW_MAX_COUNT {
        let name = format!("workflow_{index:03}");
        write_saved(&root, &saved(&name, &name));
    }
    let store = global_store(&codex_home, WorkflowLimits::default());

    assert!(matches!(
        store.save(&saved("overflow", "Overflow")),
        Err(SavedWorkflowStoreError::TooManyWorkflows {
            max_count: SAVED_WORKFLOW_MAX_COUNT,
        })
    ));
    store
        .save(&saved("workflow_000", "Replacement"))
        .expect("replacement does not increase count");
}

#[test]
fn serializes_concurrent_count_checks_with_save() {
    use std::sync::Arc;
    use std::sync::Barrier;

    let codex_home = TempDir::new().expect("create Codex home");
    let root = codex_home.path().join("workflows");
    for index in 0..SAVED_WORKFLOW_MAX_COUNT - 1 {
        let name = format!("workflow_{index:03}");
        write_saved(&root, &saved(&name, &name));
    }

    let codex_home_path = absolute(codex_home.path());
    let barrier = Arc::new(Barrier::new(3));
    let handles = ["first", "second"].map(|name| {
        let barrier = Arc::clone(&barrier);
        let codex_home_path = codex_home_path.clone();
        std::thread::spawn(move || {
            let store = SavedWorkflowStore::global(&codex_home_path, WorkflowLimits::default())
                .expect("open saved workflow store");
            barrier.wait();
            store.save(&saved(name, name))
        })
    });
    barrier.wait();

    let results = handles.map(|handle| handle.join().expect("save thread"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(SavedWorkflowStoreError::TooManyWorkflows {
                    max_count: SAVED_WORKFLOW_MAX_COUNT,
                })
            ))
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(root)
            .expect("read saved workflow root")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            })
            .count(),
        SAVED_WORKFLOW_MAX_COUNT
    );
}

#[test]
fn lists_effective_workflows_deterministically() {
    let codex_home = TempDir::new().expect("create Codex home");
    let store = global_store(&codex_home, WorkflowLimits::default());
    for name in ["zeta", "alpha", "middle"] {
        store
            .save(&saved(name, name))
            .expect("save workflow for list");
    }

    let names = store
        .list()
        .expect("list workflows")
        .into_iter()
        .map(|saved| saved.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["alpha", "middle", "zeta"]);
}

fn global_store(codex_home: &TempDir, limits: WorkflowLimits) -> SavedWorkflowStore {
    SavedWorkflowStore::global(&absolute(codex_home.path()), limits)
        .expect("open global saved workflow store")
}

fn project_store(
    codex_home: &TempDir,
    project: &TempDir,
    trust: SavedWorkflowProjectTrust,
) -> SavedWorkflowStore {
    SavedWorkflowStore::for_project(
        &absolute(codex_home.path()),
        &absolute(project.path()),
        trust,
        WorkflowLimits::default(),
    )
    .expect("open project saved workflow store")
}

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute test path")
}

fn write_saved(root: &Path, saved: &SavedWorkflowDefinition) {
    fs::create_dir_all(root).expect("create saved workflow root");
    fs::write(
        root.join(format!("{}.json", saved.name)),
        serde_json::to_vec(saved).expect("serialize saved workflow"),
    )
    .expect("write saved workflow");
}

fn saved(name: &str, title: &str) -> SavedWorkflowDefinition {
    SavedWorkflowDefinition::new(name, definition(title))
}

fn definition(title: &str) -> WorkflowDefinition {
    WorkflowDefinition::try_new(
        title,
        vec![WorkflowTask {
            id: WorkflowTaskId::from("task"),
            title: "Task".to_string(),
            instructions: "Do the task".to_string(),
            dependencies: Vec::new(),
            agent_role: None,
            intent: WorkflowTaskIntent::ReadOnly,
            owned_paths: Vec::new(),
            verification: vec!["Report evidence".to_string()],
            max_attempts: 1,
        }],
        WorkflowLimits::default(),
    )
    .expect("valid workflow definition")
}
