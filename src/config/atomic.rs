use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Debug)]
pub enum ConfigCommitOutcome {
    Durable,
    CommittedDurabilityUncertain(io::Error),
}

pub(crate) fn atomic_replace(path: &Path, content: &[u8]) -> io::Result<ConfigCommitOutcome> {
    atomic_replace_with_parent_sync(path, content, |parent| File::open(parent)?.sync_all())
}

fn config_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn atomic_replace_with_parent_sync<F>(
    path: &Path,
    content: &[u8],
    sync_parent: F,
) -> io::Result<ConfigCommitOutcome>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let parent = config_parent(path);
    let mut temporary = NamedTempFile::new_in(&parent)?;
    temporary.as_file_mut().write_all(content)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;

    match sync_parent(&parent) {
        Ok(()) => Ok(ConfigCommitOutcome::Durable),
        Err(error) => Ok(ConfigCommitOutcome::CommittedDurabilityUncertain(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::io;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn directory_entries(path: &std::path::Path) -> BTreeSet<std::ffi::OsString> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect()
    }

    #[test]
    fn atomic_replace_commits_complete_new_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"old-complete").unwrap();

        let outcome = atomic_replace(&path, b"new-complete").unwrap();

        assert!(matches!(outcome, ConfigCommitOutcome::Durable));
        assert_eq!(fs::read(&path).unwrap(), b"new-complete");
    }

    #[test]
    fn parent_sync_failure_is_post_commit_not_false_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"old").unwrap();

        let outcome = atomic_replace_with_parent_sync(&path, b"new", |_| {
            Err(io::Error::other("injected directory sync failure"))
        })
        .unwrap();

        assert!(matches!(
            outcome,
            ConfigCommitOutcome::CommittedDurabilityUncertain(_)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_commits_private_file_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        atomic_replace(&path, b"new").unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_replaces_symlink_without_changing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unexpected-target");
        let path = dir.path().join("config.toml");
        fs::write(&target, b"target-old").unwrap();
        symlink(&target, &path).unwrap();

        atomic_replace(&path, b"config-new").unwrap();

        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(path).unwrap(), b"config-new");
        assert_eq!(fs::read(target).unwrap(), b"target-old");
    }

    #[test]
    fn atomic_replace_cleans_temporary_file_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("marker"), b"keep").unwrap();
        let entries_before = directory_entries(dir.path());

        assert!(atomic_replace(&path, b"new").is_err());

        assert_eq!(directory_entries(dir.path()), entries_before);
        assert_eq!(fs::read(path.join("marker")).unwrap(), b"keep");
    }
}
