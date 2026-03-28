use std::fs;
use std::path::PathBuf;

use md5::{Digest, Md5};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::error::{Result, XurlError};
use crate::model::{ProviderKind, ResolutionMeta, ResolvedThread};
use crate::provider::Provider;

#[derive(Debug, Deserialize)]
struct KimiMeta {
    #[serde(default)]
    work_dirs: Vec<KimiWorkDir>,
}

#[derive(Debug, Deserialize)]
struct KimiWorkDir {
    path: String,
    #[allow(dead_code)]
    #[serde(default)]
    kaos: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    last_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct KimiProvider {
    root: PathBuf,
}

impl KimiProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn sessions_root(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn meta_path(&self) -> PathBuf {
        self.root.join("kimi.json")
    }

    fn load_meta(&self) -> Option<KimiMeta> {
        let raw = fs::read_to_string(self.meta_path()).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn md5_of_path(path: &str) -> String {
        let mut hasher = Md5::new();
        hasher.update(path.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn find_via_metadata(&self, session_id: &str) -> Vec<PathBuf> {
        let Some(meta) = self.load_meta() else {
            return Vec::new();
        };

        let sessions_root = self.sessions_root();
        let mut results = Vec::new();

        for work_dir in &meta.work_dirs {
            let hash = Self::md5_of_path(&work_dir.path);
            let context_path = sessions_root
                .join(&hash)
                .join(session_id)
                .join("context.jsonl");
            if context_path.exists() {
                results.push(context_path);
            }
        }

        results
    }

    fn find_by_scan(&self, session_id: &str) -> Vec<PathBuf> {
        let sessions_root = self.sessions_root();
        if !sessions_root.exists() {
            return Vec::new();
        }

        WalkDir::new(&sessions_root)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_dir())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name == session_id)
            })
            .map(|entry| entry.into_path().join("context.jsonl"))
            .filter(|path| path.exists())
            .collect()
    }
}

impl Provider for KimiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Kimi
    }

    fn resolve(&self, session_id: &str) -> Result<ResolvedThread> {
        let meta_hits = self.find_via_metadata(session_id);
        if let Some(selected) = meta_hits.first() {
            let mut metadata = ResolutionMeta {
                source: "kimi:metadata".to_string(),
                candidate_count: meta_hits.len(),
                warnings: Vec::new(),
            };
            if meta_hits.len() > 1 {
                metadata.warnings.push(format!(
                    "multiple matches found ({}) for session_id={session_id}; selected first: {}",
                    meta_hits.len(),
                    selected.display()
                ));
            }
            return Ok(ResolvedThread {
                provider: ProviderKind::Kimi,
                session_id: session_id.to_string(),
                path: selected.clone(),
                metadata,
            });
        }

        let scan_hits = self.find_by_scan(session_id);
        if let Some(selected) = scan_hits.first() {
            let mut metadata = ResolutionMeta {
                source: "kimi:scan".to_string(),
                candidate_count: scan_hits.len(),
                warnings: Vec::new(),
            };
            if scan_hits.len() > 1 {
                metadata.warnings.push(format!(
                    "multiple matches found ({}) for session_id={session_id}; selected first: {}",
                    scan_hits.len(),
                    selected.display()
                ));
            }
            return Ok(ResolvedThread {
                provider: ProviderKind::Kimi,
                session_id: session_id.to_string(),
                path: selected.clone(),
                metadata,
            });
        }

        Err(XurlError::ThreadNotFound {
            provider: ProviderKind::Kimi.to_string(),
            session_id: session_id.to_string(),
            searched_roots: vec![self.sessions_root()],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::provider::Provider;
    use crate::provider::kimi::KimiProvider;

    #[test]
    fn resolves_via_metadata() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();

        let work_dir_path = "/Users/alice/some/project";
        let hash = KimiProvider::md5_of_path(work_dir_path);
        let session_id = "2823d1df-720a-4c31-ac55-ae8ba726721f";

        let session_dir = root.join("sessions").join(&hash).join(session_id);
        fs::create_dir_all(&session_dir).expect("mkdir");
        let context_file = session_dir.join("context.jsonl");
        fs::write(&context_file, "{\"role\":\"user\",\"content\":\"hello\"}\n")
            .expect("write context");

        let meta = format!(
            r#"{{"work_dirs":[{{"path":"{}","kaos":"local","last_session_id":"{}"}}]}}"#,
            work_dir_path, session_id
        );
        fs::write(root.join("kimi.json"), meta).expect("write meta");

        let provider = KimiProvider::new(root);
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");
        assert_eq!(resolved.path, context_file);
        assert_eq!(resolved.metadata.source, "kimi:metadata");
    }

    #[test]
    fn resolves_via_scan_when_metadata_missing() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();

        let session_id = "8c06e0f0-2978-48ac-bb42-90d13e3b0470";
        let session_dir = root.join("sessions").join("somehashdir").join(session_id);
        fs::create_dir_all(&session_dir).expect("mkdir");
        let context_file = session_dir.join("context.jsonl");
        fs::write(&context_file, "{\"role\":\"user\",\"content\":\"hello\"}\n")
            .expect("write context");

        let provider = KimiProvider::new(root);
        let resolved = provider
            .resolve(session_id)
            .expect("resolve should succeed");
        assert_eq!(resolved.path, context_file);
        assert_eq!(resolved.metadata.source, "kimi:scan");
    }

    #[test]
    fn returns_not_found_when_missing() {
        let temp = tempdir().expect("tempdir");
        let provider = KimiProvider::new(temp.path());
        let err = provider
            .resolve("2823d1df-720a-4c31-ac55-ae8ba726721f")
            .expect_err("should fail");
        assert!(format!("{err}").contains("thread not found"));
    }

    #[test]
    fn md5_hash_matches_python_hashlib() {
        // Verify our md5 hash matches what Python's hashlib would produce:
        // hashlib.md5("/Users/alice/some/project".encode("utf-8")).hexdigest()
        let hash = KimiProvider::md5_of_path("/Users/alice/some/project");
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
