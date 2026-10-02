//! Wire (on-disk) provenance schemas.
//!
//! `wire::v1` freezes the legacy v1 JSON shape (read-only).
//! `wire::v5` is the current write schema; v1 through v4 are frozen readers.
//! The canonical in-memory representation lives in `super::types`.
//!
//! Never deserialize an event file directly into the canonical struct.
//! Read the [`EventHeader`] first, then dispatch explicitly by version.
//! Unknown versions are skipped with a warning; serde's implicit evolution
//! (unknown-field tolerance, `#[serde(other)]`) must not be used as a
//! migration mechanism for durable tagged enums.

pub mod v1;
pub mod v2;
pub mod v3;
pub mod v4;
pub mod v5;

use serde::{Deserialize, Serialize};

/// Minimal header read before any version dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventHeader {
    pub schema_version: u32,
}
