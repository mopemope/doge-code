//! Typed identifiers. The string form is `<prefix><uuid v7, 32 lowercase hex>`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Length of the uuid part of an id: 32 lowercase hex characters.
const BODY_LEN: usize = 32;

/// Failure to parse an id from its string form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdParseError {
    expected_prefix: &'static str,
    input: String,
}

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid id {:?}: expected `{}` followed by {BODY_LEN} lowercase hex characters",
            self.input, self.expected_prefix
        )
    }
}

impl std::error::Error for IdParseError {}

fn validate(prefix: &'static str, s: &str) -> Result<(), IdParseError> {
    let ok = s.strip_prefix(prefix).is_some_and(|body| {
        body.len() == BODY_LEN && body.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    if ok {
        Ok(())
    } else {
        Err(IdParseError {
            expected_prefix: prefix,
            input: s.to_owned(),
        })
    }
}

macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// String prefix that identifies this kind of id.
            pub const PREFIX: &'static str = $prefix;

            /// Generate a new time-ordered id (uuid v7).
            pub fn generate() -> Self {
                Self(format!("{}{}", $prefix, Uuid::now_v7().simple()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                validate($prefix, s)?;
                Ok(Self(s.to_owned()))
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdParseError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                validate($prefix, &s)?;
                Ok(Self(s))
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

define_id!(
    /// Identifies a registered project.
    ProjectId, "prj_"
);
define_id!(
    /// Identifies a workspace (a checkout where agents run).
    WorkspaceId, "ws_"
);
define_id!(
    /// Identifies an agent.
    AgentId, "agt_"
);
define_id!(
    /// Identifies a pending permission request.
    PermissionId, "perm_"
);

define_id!(
    /// Identifies a schedule or heartbeat.
    ScheduleId, "sch_"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_wrong_prefix() {
        assert!("ws_x".parse::<AgentId>().is_err());
        let ws = WorkspaceId::generate();
        assert!(ws.as_str().parse::<AgentId>().is_err());
    }

    #[test]
    fn ids_reject_bad_body() {
        assert!("agt_x".parse::<AgentId>().is_err());
        assert!("agt_".parse::<AgentId>().is_err());
        let upper = format!("agt_{}", "A".repeat(32));
        assert!(upper.parse::<AgentId>().is_err());
        let hyphenated = "agt_0190aaaa-bbbb-cccc-dddd-eeeeffff0000";
        assert!(hyphenated.parse::<AgentId>().is_err());
    }

    #[test]
    fn generated_ids_roundtrip_and_are_unique() {
        let a = AgentId::generate();
        let b = AgentId::generate();
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("agt_"));
        assert_eq!(a.as_str().len(), "agt_".len() + 32);
        assert_eq!(a.as_str().parse::<AgentId>().expect("parse"), a);
        assert_eq!(a.to_string(), a.as_str());
    }

    #[test]
    fn ids_serialize_as_plain_strings_and_validate_on_deserialize() {
        let id = PermissionId::generate();
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, format!("\"{}\"", id.as_str()));
        let back: PermissionId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, id);
        assert!(serde_json::from_str::<PermissionId>("\"agt_x\"").is_err());
    }

    #[test]
    fn every_id_kind_has_its_own_prefix() {
        assert!(ProjectId::generate().as_str().starts_with("prj_"));
        assert!(WorkspaceId::generate().as_str().starts_with("ws_"));
        assert!(AgentId::generate().as_str().starts_with("agt_"));
        assert!(PermissionId::generate().as_str().starts_with("perm_"));
    }
}
