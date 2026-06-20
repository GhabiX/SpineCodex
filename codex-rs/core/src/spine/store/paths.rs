use super::SpineStore;
use std::path::Path;
use std::path::PathBuf;

const TREE_FILE: &str = "tree.jsonl";
const PRESSURE_FILE: &str = "pressure.jsonl";
const TRIM_FILE: &str = "trim.jsonl";
const MEM_FILE: &str = "mem.jsonl";
const MEM_ACCOUNTING_FILE: &str = "mem_accounting.jsonl";
const MEM_ACCOUNTING_WITNESS_FILE: &str = "mem_accounting_witness.jsonl";
const COMMIT_FILE: &str = "commits.jsonl";
const COMPACT_CHECKPOINT_FILE: &str = "compact_checkpoints.jsonl";
const FEEDBACK_FILE: &str = "spine_feedback.md";
const CHECKPOINT_DIR: &str = "checkpoints";
const INITIAL_CHECKPOINT_FILE: &str = "initial.json";

pub(crate) const BODY_DIR: &str = "memory";

pub(super) fn sidecar_store_path(store_root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        store_root.join(path)
    }
}

impl SpineStore {
    pub(crate) fn tree_path(&self) -> PathBuf {
        self.root.join(TREE_FILE)
    }

    #[cfg(test)]
    pub(crate) fn tree_path_for_test(&self) -> PathBuf {
        self.tree_path()
    }

    pub(crate) fn mem_path(&self) -> PathBuf {
        self.root.join(MEM_FILE)
    }

    pub(super) fn mem_accounting_path(&self) -> PathBuf {
        self.root.join(MEM_ACCOUNTING_FILE)
    }

    pub(super) fn mem_accounting_witness_path(&self) -> PathBuf {
        self.root.join(MEM_ACCOUNTING_WITNESS_FILE)
    }

    pub(super) fn commit_path(&self) -> PathBuf {
        self.root.join(COMMIT_FILE)
    }

    #[cfg(test)]
    pub(crate) fn commit_path_for_test(&self) -> PathBuf {
        self.commit_path()
    }

    pub(super) fn pressure_path(&self) -> PathBuf {
        self.root.join(PRESSURE_FILE)
    }

    #[cfg(test)]
    pub(crate) fn pressure_path_for_test(&self) -> PathBuf {
        self.pressure_path()
    }

    pub(super) fn trim_path(&self) -> PathBuf {
        self.root.join(TRIM_FILE)
    }

    #[cfg(test)]
    pub(crate) fn trim_path_for_test(&self) -> PathBuf {
        self.trim_path()
    }

    pub(crate) fn feedback_path(&self) -> PathBuf {
        self.root.join(FEEDBACK_FILE)
    }

    #[cfg(test)]
    pub(crate) fn feedback_path_for_test(&self) -> PathBuf {
        self.feedback_path()
    }

    pub(super) fn compact_checkpoint_path(&self) -> PathBuf {
        self.root.join(COMPACT_CHECKPOINT_FILE)
    }

    #[cfg(test)]
    pub(crate) fn compact_checkpoint_path_for_test(&self) -> PathBuf {
        self.compact_checkpoint_path()
    }

    pub(super) fn checkpoint_dir(&self) -> PathBuf {
        self.root.join(CHECKPOINT_DIR)
    }

    pub(crate) fn checkpoint_path(&self, raw_ordinal: u64) -> PathBuf {
        self.checkpoint_dir()
            .join(format!("pre-user-{raw_ordinal:020}.json"))
    }

    pub(super) fn initial_checkpoint_path(&self) -> PathBuf {
        self.checkpoint_dir().join(INITIAL_CHECKPOINT_FILE)
    }

    #[cfg(test)]
    pub(crate) fn initial_checkpoint_path_for_test(&self) -> PathBuf {
        self.initial_checkpoint_path()
    }
}
