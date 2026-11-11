//! The client's identity: a UUID generated once per installation and stored
//! locally, sent as the `X-Marathon-Client` header (PLAN.md §8). It is the
//! only state a client keeps on disk.

use std::io;
use std::path::Path;

/// Reads the client ID stored at `path`, or generates one and stores it
/// there (creating parent directories) if the file is missing or does not
/// hold a UUID.
pub fn load_or_create(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let id = contents.trim();
            if uuid::Uuid::parse_str(id).is_ok() {
                return Ok(id.to_owned());
            }
            tracing::warn!(path = %path.display(), "client ID file is not a UUID; replacing it");
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{id}\n"))?;
    tracing::info!(path = %path.display(), client_id = %id, "generated a new client ID");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_once_then_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/client-id");
        let first = load_or_create(&path).unwrap();
        assert!(uuid::Uuid::parse_str(&first).is_ok());
        assert_eq!(load_or_create(&path).unwrap(), first);
    }

    #[test]
    fn replaces_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client-id");
        std::fs::write(&path, "not a uuid").unwrap();
        let id = load_or_create(&path).unwrap();
        assert_ne!(id, "not a uuid");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), id);
    }
}
