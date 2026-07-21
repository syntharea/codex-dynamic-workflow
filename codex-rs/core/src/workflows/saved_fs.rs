use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use cap_fs_ext::FollowSymlinks;
use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::ambient_authority;
use cap_primitives::fs::DirOptions;
use cap_primitives::fs::OpenOptions;
use cap_primitives::fs::create_dir;
use cap_primitives::fs::open;
use cap_primitives::fs::open_ambient_dir;
use cap_primitives::fs::open_dir_nofollow;
use cap_primitives::fs::read_base_dir;
use cap_primitives::fs::remove_file;
use cap_primitives::fs::rename;
use codex_utils_absolute_path::AbsolutePathBuf;
use fd_lock::RwLock;
use uuid::Uuid;

use super::saved::SavedWorkflowStoreError;
use super::saved::io_error;
use super::saved::validate_name;
use super::saved::workflow_path;

const STORE_LOCK_FILE: &str = ".saved-workflows.lock";
const TEMP_FILE_ATTEMPTS: usize = 16;
static PROCESS_STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub(super) struct SavedWorkflowRoot {
    anchor: File,
    anchor_path: AbsolutePathBuf,
    components: Vec<OsString>,
    path: AbsolutePathBuf,
}

impl SavedWorkflowRoot {
    pub(super) fn new(
        anchor_path: &AbsolutePathBuf,
        components: impl IntoIterator<Item = &'static str>,
    ) -> Result<Self, SavedWorkflowStoreError> {
        let anchor = open_absolute_directory_nofollow(anchor_path)?;
        let components = components
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let path = components
            .iter()
            .fold(anchor_path.clone(), |path, component| path.join(component));
        Ok(Self {
            anchor,
            anchor_path: anchor_path.clone(),
            components,
            path,
        })
    }

    pub(super) fn path(&self) -> &AbsolutePathBuf {
        &self.path
    }

    pub(super) fn read(
        &self,
        name: &str,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, SavedWorkflowStoreError> {
        let Some(root) = self.open_root(/*create*/ false)? else {
            return Ok(None);
        };
        let Some(file) = self.open_regular_file(&root, name)? else {
            return Ok(None);
        };
        let path = workflow_path(&self.path, name);
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        if metadata.len() > max_bytes {
            return Err(SavedWorkflowStoreError::FileTooLarge {
                path,
                actual_bytes: metadata.len(),
                max_bytes,
            });
        }

        let mut contents = Vec::with_capacity(metadata.len() as usize + 1);
        file.take(max_bytes + 1)
            .read_to_end(&mut contents)
            .map_err(|source| io_error(&path, source))?;
        if contents.len() as u64 > max_bytes {
            return Err(SavedWorkflowStoreError::FileTooLarge {
                path,
                actual_bytes: contents.len() as u64,
                max_bytes,
            });
        }
        Ok(Some(contents))
    }

    pub(super) fn names(&self, max_count: usize) -> Result<Vec<String>, SavedWorkflowStoreError> {
        let Some(root) = self.open_root(/*create*/ false)? else {
            return Ok(Vec::new());
        };
        names_from_root(&root, &self.path, max_count)
    }

    pub(super) fn save(
        &self,
        name: &str,
        contents: &[u8],
        max_count: usize,
    ) -> Result<(), SavedWorkflowStoreError> {
        let root = self.open_root(/*create*/ true)?.ok_or_else(|| {
            io_error(
                &self.path,
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "saved workflow root was not created",
                ),
            )
        })?;
        let _process_guard = PROCESS_STORE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock_file = open_lock_file(&root, &self.path)?;
        let mut file_lock = RwLock::new(lock_file);
        let _file_guard = file_lock
            .write()
            .map_err(|source| io_error(&self.path.join(STORE_LOCK_FILE), source))?;

        let file_name = workflow_file_name(name);
        let (count, target_exists) = count_workflows(&root, &self.path, &file_name, max_count)?;
        if !target_exists && count >= max_count {
            return Err(SavedWorkflowStoreError::TooManyWorkflows { max_count });
        }
        if target_exists {
            let _ = self.open_regular_file(&root, name)?;
        }

        self.write_atomically(&root, name, contents)
    }

    pub(super) fn delete(&self, name: &str) -> Result<bool, SavedWorkflowStoreError> {
        let Some(root) = self.open_root(/*create*/ false)? else {
            return Ok(false);
        };
        let _process_guard = PROCESS_STORE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock_file = open_lock_file(&root, &self.path)?;
        let mut file_lock = RwLock::new(lock_file);
        let _file_guard = file_lock
            .write()
            .map_err(|source| io_error(&self.path.join(STORE_LOCK_FILE), source))?;

        if self.open_regular_file(&root, name)?.is_none() {
            return Ok(false);
        }
        let file_name = workflow_file_name(name);
        remove_file(&root, Path::new(&file_name))
            .map_err(|source| file_error(&self.path.join(&file_name), source))?;
        Ok(true)
    }

    fn open_root(&self, create: bool) -> Result<Option<File>, SavedWorkflowStoreError> {
        let mut current = self
            .anchor
            .try_clone()
            .map_err(|source| io_error(&self.anchor_path, source))?;
        let mut current_path = self.anchor_path.clone();
        for component in &self.components {
            current_path = current_path.join(component);
            match open_dir_nofollow(&current, Path::new(component)) {
                Ok(directory) => current = directory,
                Err(source) if source.kind() == io::ErrorKind::NotFound && !create => {
                    return Ok(None);
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    match create_dir(&current, Path::new(component), &DirOptions::new()) {
                        Ok(()) => {}
                        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(source) => return Err(root_error(&current_path, source)),
                    }
                    current = open_dir_nofollow(&current, Path::new(component))
                        .map_err(|source| root_error(&current_path, source))?;
                }
                Err(source) => return Err(root_error(&current_path, source)),
            }
        }
        Ok(Some(current))
    }

    fn open_regular_file(
        &self,
        root: &File,
        name: &str,
    ) -> Result<Option<File>, SavedWorkflowStoreError> {
        let file_name = workflow_file_name(name);
        let path = self.path.join(&file_name);
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = match open(root, Path::new(&file_name), &options) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(file_error(&path, source)),
        };
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        if !metadata.is_file() {
            return Err(io_error(
                &path,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "saved workflow path is not a regular file",
                ),
            ));
        }
        Ok(Some(file))
    }

    fn write_atomically(
        &self,
        root: &File,
        name: &str,
        contents: &[u8],
    ) -> Result<(), SavedWorkflowStoreError> {
        let destination_name = workflow_file_name(name);
        let destination_path = self.path.join(&destination_name);
        for _attempt in 0..TEMP_FILE_ATTEMPTS {
            let temporary_name = format!(".{name}.{}.tmp", Uuid::new_v4());
            let temporary_path = self.path.join(&temporary_name);
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            let mut temporary = match open(root, Path::new(&temporary_name), &options) {
                Ok(file) => file,
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(file_error(&temporary_path, source)),
            };
            if let Err(source) = temporary
                .write_all(contents)
                .and_then(|()| temporary.sync_all())
            {
                drop(temporary);
                let _ = remove_file(root, Path::new(&temporary_name));
                return Err(io_error(&temporary_path, source));
            }
            drop(temporary);
            if let Err(source) = rename(
                root,
                Path::new(&temporary_name),
                root,
                Path::new(&destination_name),
            ) {
                let _ = remove_file(root, Path::new(&temporary_name));
                return Err(io_error(&destination_path, source));
            }
            return Ok(());
        }
        Err(io_error(
            &destination_path,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a unique saved workflow temporary file",
            ),
        ))
    }
}

fn open_absolute_directory_nofollow(
    path: &AbsolutePathBuf,
) -> Result<File, SavedWorkflowStoreError> {
    let mut filesystem_root = PathBuf::new();
    let mut components = Vec::new();
    let mut has_root = false;
    for component in path.as_path().components() {
        match component {
            Component::Prefix(prefix) => filesystem_root.push(prefix.as_os_str()),
            Component::RootDir => {
                filesystem_root.push(component.as_os_str());
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(io_error(
                    path,
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "saved workflow anchor contains a parent-directory component",
                    ),
                ));
            }
            Component::Normal(component) => components.push(component.to_os_string()),
        }
    }
    if !has_root {
        return Err(io_error(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "saved workflow anchor is not absolute",
            ),
        ));
    }

    let mut current = open_ambient_dir(&filesystem_root, ambient_authority())
        .map_err(|source| io_error(path, source))?;
    let mut current_path =
        AbsolutePathBuf::from_absolute_path(&filesystem_root).map_err(|error| {
            io_error(
                path,
                io::Error::new(io::ErrorKind::InvalidInput, error.to_string()),
            )
        })?;
    for component in components {
        current_path = current_path.join(&component);
        current = open_dir_nofollow(&current, Path::new(&component))
            .map_err(|source| root_error(&current_path, source))?;
    }
    Ok(current)
}

fn workflow_file_name(name: &str) -> OsString {
    OsString::from(format!("{name}.json"))
}

fn names_from_root(
    root: &File,
    root_path: &AbsolutePathBuf,
    max_count: usize,
) -> Result<Vec<String>, SavedWorkflowStoreError> {
    let entries = read_base_dir(root).map_err(|source| io_error(root_path, source))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(root_path, source))?;
        let file_name = entry.file_name();
        let relative_path = Path::new(&file_name);
        if relative_path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        let name = relative_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| SavedWorkflowStoreError::InvalidName {
                name: relative_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            })?
            .to_string();
        validate_name(&name)?;
        names.push(name);
        if names.len() > max_count {
            return Err(SavedWorkflowStoreError::TooManyWorkflows { max_count });
        }
    }
    names.sort_unstable();
    Ok(names)
}

fn count_workflows(
    root: &File,
    root_path: &AbsolutePathBuf,
    target_name: &OsString,
    max_count: usize,
) -> Result<(usize, bool), SavedWorkflowStoreError> {
    let entries = read_base_dir(root).map_err(|source| io_error(root_path, source))?;
    let mut count = 0;
    let mut target_exists = false;
    for entry in entries {
        let entry = entry.map_err(|source| io_error(root_path, source))?;
        let file_name = entry.file_name();
        if Path::new(&file_name)
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        count += 1;
        target_exists |= file_name == *target_name;
        if count > max_count {
            return Err(SavedWorkflowStoreError::TooManyWorkflows { max_count });
        }
    }
    Ok((count, target_exists))
}

fn open_lock_file(
    root: &File,
    root_path: &AbsolutePathBuf,
) -> Result<File, SavedWorkflowStoreError> {
    let path = root_path.join(STORE_LOCK_FILE);
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = open(root, Path::new(STORE_LOCK_FILE), &options)
        .map_err(|source| file_error(&path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
    if !metadata.is_file() {
        return Err(io_error(
            &path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "saved workflow lock path is not a regular file",
            ),
        ));
    }
    Ok(file)
}

fn root_error(path: &AbsolutePathBuf, source: io::Error) -> SavedWorkflowStoreError {
    if std::fs::symlink_metadata(path.as_ref()).is_ok_and(|metadata| metadata.is_symlink()) {
        SavedWorkflowStoreError::SymlinkRoot { path: path.clone() }
    } else {
        io_error(path, source)
    }
}

fn file_error(path: &AbsolutePathBuf, source: io::Error) -> SavedWorkflowStoreError {
    if std::fs::symlink_metadata(path.as_ref()).is_ok_and(|metadata| metadata.is_symlink()) {
        SavedWorkflowStoreError::SymlinkFile { path: path.clone() }
    } else {
        io_error(path, source)
    }
}
