use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable identifier of an input (source). Never reused within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputId(pub u32);

impl fmt::Display for InputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "input#{}", self.0)
    }
}
