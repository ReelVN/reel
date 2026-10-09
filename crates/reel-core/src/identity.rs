//! Owned resource names and fixed-width identities. No native handles or pointers.

macro_rules! id {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);
    )+ };
}

id!(
    SessionId,
    Epoch,
    OwnerId,
    IoId,
    WorkId,
    ProcessId,
    RequestId,
    FrameId,
    SurfaceEpoch
);

/// Exact identity in the process service's request ledger, independent of IO IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessRequestId {
    pub process_id: ProcessId,
    pub request_id: RequestId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Session {
        session_id: SessionId,
        epoch: Epoch,
        owner_id: OwnerId,
    },
    Process,
}

/// Stable name in an application's asset namespace, not a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetKey(pub String);

/// Opaque fixed content digest selected by the release, never the current version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentVersion(pub [u8; 32]);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceVersion {
    pub key: AssetKey,
    pub version: ContentVersion,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Prefetch,
    #[default]
    Normal,
    Visible,
}
