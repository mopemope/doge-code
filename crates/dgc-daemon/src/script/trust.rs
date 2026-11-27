//! Versioned, project-local history of approved dgc.json snapshots.

use dgc_protocol::methods::ScriptTrustState;
use serde::{Deserialize, Serialize};

const MAX_TRUSTED_CONTENTS: usize = 16;

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct TrustHistory {
    contents: Vec<String>,
}

impl TrustHistory {
    pub(super) fn state(&self, bytes: Option<&[u8]>) -> ScriptTrustState {
        if self.contents.is_empty() {
            ScriptTrustState::Untrusted
        } else if bytes
            .is_some_and(|bytes| self.contents.iter().any(|text| text.as_bytes() == bytes))
        {
            ScriptTrustState::Trusted
        } else {
            ScriptTrustState::Changed
        }
    }

    pub(super) fn remember(&mut self, content: &str) {
        self.contents.retain(|text| text != content);
        self.contents.insert(0, content.to_owned());
        self.contents.truncate(MAX_TRUSTED_CONTENTS);
    }
}
