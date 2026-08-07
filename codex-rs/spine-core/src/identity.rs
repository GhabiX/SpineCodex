use serde::Deserialize;
use serde::Serialize;
use std::fmt;

pub const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError {
    EmptyNamespace,
    NamespaceTooLarge,
    InvalidNamespace,
    UnchangedForkNamespace,
    EmptyId,
    IdTooLarge,
    InvalidId,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNamespace => f.write_str("thread namespace must not be empty"),
            Self::NamespaceTooLarge => {
                write!(f, "thread namespace exceeds {MAX_ID_BYTES} bytes")
            }
            Self::InvalidNamespace => {
                f.write_str("thread namespace contains an unsupported character")
            }
            Self::UnchangedForkNamespace => {
                f.write_str("fork thread namespace must differ from its parent")
            }
            Self::EmptyId => f.write_str("identity value must not be empty"),
            Self::IdTooLarge => write!(f, "identity value exceeds {MAX_ID_BYTES} bytes"),
            Self::InvalidId => f.write_str("identity value contains an unsupported character"),
        }
    }
}

impl std::error::Error for IdentityError {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ThreadNamespace(String);

impl ThreadNamespace {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        validate_component(
            &value,
            IdentityError::EmptyNamespace,
            IdentityError::NamespaceTooLarge,
            IdentityError::InvalidNamespace,
        )?;
        Ok(Self(value))
    }

    pub fn for_fork(&self, value: impl Into<String>) -> Result<Self, IdentityError> {
        let fork = Self::parse(value)?;
        if fork == *self {
            return Err(IdentityError::UnchangedForkNamespace);
        }
        Ok(fork)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ThreadNamespace {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ThreadNamespace> for String {
    fn from(value: ThreadNamespace) -> Self {
        value.0
    }
}

impl fmt::Display for ThreadNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ContextEpoch(u64);

impl ContextEpoch {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

macro_rules! epoch_ordinal_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name {
            thread: ThreadNamespace,
            epoch: ContextEpoch,
            ordinal: u64,
        }

        impl $name {
            pub const fn new(thread: ThreadNamespace, epoch: ContextEpoch, ordinal: u64) -> Self {
                Self {
                    thread,
                    epoch,
                    ordinal,
                }
            }

            pub fn thread(&self) -> &ThreadNamespace {
                &self.thread
            }

            pub const fn epoch(&self) -> ContextEpoch {
                self.epoch
            }

            pub const fn ordinal(&self) -> u64 {
                self.ordinal
            }
        }
    };
}

epoch_ordinal_id!(SourceCellId);
epoch_ordinal_id!(ProjectionCellId);
epoch_ordinal_id!(BoundaryId);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct NamespacedIdRepr {
    thread: ThreadNamespace,
    value: String,
}

macro_rules! namespaced_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "NamespacedIdRepr", into = "NamespacedIdRepr")]
        pub struct $name {
            thread: ThreadNamespace,
            value: String,
        }

        impl $name {
            pub fn parse(
                thread: ThreadNamespace,
                value: impl Into<String>,
            ) -> Result<Self, IdentityError> {
                let value = value.into();
                validate_component(
                    &value,
                    IdentityError::EmptyId,
                    IdentityError::IdTooLarge,
                    IdentityError::InvalidId,
                )?;
                Ok(Self { thread, value })
            }

            pub fn thread(&self) -> &ThreadNamespace {
                &self.thread
            }

            pub fn as_str(&self) -> &str {
                &self.value
            }
        }

        impl TryFrom<NamespacedIdRepr> for $name {
            type Error = IdentityError;

            fn try_from(value: NamespacedIdRepr) -> Result<Self, Self::Error> {
                Self::parse(value.thread, value.value)
            }
        }

        impl From<$name> for NamespacedIdRepr {
            fn from(value: $name) -> Self {
                Self {
                    thread: value.thread,
                    value: value.value,
                }
            }
        }
    };
}

namespaced_id!(SamplingAttemptId);
namespaced_id!(SamplingCommitId);
namespaced_id!(ExecutionId);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "TrimTicketRepr", into = "TrimTicketRepr")]
pub struct TrimTicket {
    thread: ThreadNamespace,
    epoch: ContextEpoch,
    value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TrimTicketRepr {
    thread: ThreadNamespace,
    epoch: ContextEpoch,
    value: String,
}

impl TrimTicket {
    pub fn parse(
        thread: ThreadNamespace,
        epoch: ContextEpoch,
        value: impl Into<String>,
    ) -> Result<Self, IdentityError> {
        let value = value.into();
        validate_component(
            &value,
            IdentityError::EmptyId,
            IdentityError::IdTooLarge,
            IdentityError::InvalidId,
        )?;
        Ok(Self {
            thread,
            epoch,
            value,
        })
    }

    pub fn thread(&self) -> &ThreadNamespace {
        &self.thread
    }

    pub const fn epoch(&self) -> ContextEpoch {
        self.epoch
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl TryFrom<TrimTicketRepr> for TrimTicket {
    type Error = IdentityError;

    fn try_from(value: TrimTicketRepr) -> Result<Self, Self::Error> {
        Self::parse(value.thread, value.epoch, value.value)
    }
}

impl From<TrimTicket> for TrimTicketRepr {
    fn from(value: TrimTicket) -> Self {
        Self {
            thread: value.thread,
            epoch: value.epoch,
            value: value.value,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct AdmissionOrdinal(u64);

impl AdmissionOrdinal {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

fn validate_component(
    value: &str,
    empty: IdentityError,
    too_large: IdentityError,
    invalid: IdentityError,
) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(empty);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(too_large);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(invalid);
    }
    Ok(())
}
