use sha1::Digest;
use std::fmt;
use thiserror::Error;

pub(crate) const GENERATED_MEMORY_SECTION_MARKER: &str =
    "\n\n<!-- spine:auto-compact-generated -->\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemorySectionId {
    pub(crate) storage_ref: String,
    pub(crate) section_index: usize,
}

impl MemorySectionId {
    pub(crate) fn new(storage_ref: impl Into<String>, section_index: usize) -> Self {
        Self {
            storage_ref: storage_ref.into(),
            section_index,
        }
    }

    pub(crate) fn parse(
        memory_section_id: impl AsRef<str>,
        storage_ref: impl Into<String>,
    ) -> Result<Self, MemoryBodyError> {
        let memory_section_id = memory_section_id.as_ref();
        let storage_ref = storage_ref.into();
        let prefix = format!("{storage_ref}#section-");
        let Some(section_index) = memory_section_id.strip_prefix(&prefix) else {
            return Err(MemoryBodyError::MalformedSectionId {
                memory_section_id: memory_section_id.to_string(),
                storage_ref,
            });
        };
        let section_index =
            section_index
                .parse::<usize>()
                .map_err(|_| MemoryBodyError::MalformedSectionId {
                    memory_section_id: memory_section_id.to_string(),
                    storage_ref: storage_ref.clone(),
                })?;
        Ok(Self::new(storage_ref, section_index))
    }
}

impl fmt::Display for MemorySectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#section-{}", self.storage_ref, self.section_index)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemoryBodyRef {
    pub(crate) section_id: MemorySectionId,
    pub(crate) body_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GeneratedMemorySection {
    pub(crate) section_id: MemorySectionId,
    pub(crate) payload: String,
    pub(crate) body: String,
    pub(crate) body_hash: String,
}

impl GeneratedMemorySection {
    pub(crate) fn body_ref(&self) -> MemoryBodyRef {
        MemoryBodyRef {
            section_id: self.section_id.clone(),
            body_hash: self.body_hash.clone(),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum MemoryBodyError {
    #[error("memory body section id {memory_section_id:?} is malformed for storage {storage_ref}")]
    MalformedSectionId {
        memory_section_id: String,
        storage_ref: String,
    },
    #[error("memory body section {section_id} is missing")]
    MissingSection { section_id: MemorySectionId },
    #[error(
        "memory body section {section_id} storage mismatch: expected {expected}, actual {actual}"
    )]
    StorageMismatch {
        section_id: MemorySectionId,
        expected: String,
        actual: String,
    },
    #[error("memory body section {section_id} hash mismatch: expected {expected}, actual {actual}")]
    BodyHashMismatch {
        section_id: MemorySectionId,
        expected: String,
        actual: String,
    },
}

pub(crate) fn memory_body_hash(body: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(body.as_bytes());
    format!("sha1:{:x}", hasher.finalize())
}

pub(crate) fn parse_generated_memory_sections(
    storage_ref: impl Into<String>,
    memory: &str,
) -> Vec<GeneratedMemorySection> {
    let storage_ref = storage_ref.into();
    memory
        .split(GENERATED_MEMORY_SECTION_MARKER)
        .skip(1)
        .enumerate()
        .map(|(section_index, payload)| {
            let body = memory_body_for_hash(payload).to_string();
            let body_hash = memory_body_hash(&body);
            GeneratedMemorySection {
                section_id: MemorySectionId::new(storage_ref.clone(), section_index),
                payload: payload.to_string(),
                body,
                body_hash,
            }
        })
        .collect()
}

pub(crate) fn verify_memory_body_ref(
    storage_ref: impl AsRef<str>,
    memory: &str,
    body_ref: &MemoryBodyRef,
) -> Result<GeneratedMemorySection, MemoryBodyError> {
    let storage_ref = storage_ref.as_ref();
    if body_ref.section_id.storage_ref != storage_ref {
        return Err(MemoryBodyError::StorageMismatch {
            section_id: body_ref.section_id.clone(),
            expected: storage_ref.to_string(),
            actual: body_ref.section_id.storage_ref.clone(),
        });
    }
    let Some(section) = parse_generated_memory_sections(storage_ref, memory)
        .into_iter()
        .find(|section| section.section_id == body_ref.section_id)
    else {
        return Err(MemoryBodyError::MissingSection {
            section_id: body_ref.section_id.clone(),
        });
    };
    if section.body_hash != body_ref.body_hash {
        return Err(MemoryBodyError::BodyHashMismatch {
            section_id: body_ref.section_id.clone(),
            expected: body_ref.body_hash.clone(),
            actual: section.body_hash,
        });
    }
    Ok(section)
}

fn memory_body_for_hash(section_payload: &str) -> &str {
    let payload = section_payload.trim_start_matches('\n');
    let Some(after_heading) = payload.strip_prefix("## Auto Compact\n\n") else {
        return section_payload;
    };
    let Some(metadata_end) = after_heading.find("\n\n") else {
        return section_payload;
    };
    let body_and_summary = &after_heading[metadata_end + 2..];
    let body_end = body_and_summary
        .find("\n\n## Node Summary")
        .unwrap_or(body_and_summary.len());
    body_and_summary[..body_end].trim_matches('\n')
}

#[cfg(test)]
#[path = "mem_install_tests.rs"]
mod tests;
