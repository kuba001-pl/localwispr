//! Line-delimited JSON protocol between the daemon and `localwispr-llm`.
//!
//! Shared verbatim by both crates (`#[path]` include) so the two sides can
//! never disagree on the wire format.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_owned(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Must be the first request. A worker serves exactly one model; switching
    /// models means restarting the worker (which also frees VRAM cleanly).
    Load {
        path: String,
        gpu_layers: u32,
        n_ctx: u32,
    },
    /// Greedy chat completion. The longest prompt prefix shared with the
    /// previous request is reused from the KV cache.
    Complete {
        id: u64,
        messages: Vec<ChatMessage>,
        max_tokens: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Loaded {
        model: String,
        ms: u64,
    },
    Completion {
        id: u64,
        text: String,
        prompt_tokens: u32,
        cached_tokens: u32,
        generated_tokens: u32,
        /// False (including older workers omitting the field) is never safe to insert.
        #[serde(default)]
        reached_eog: bool,
        ms: u64,
    },
    Error {
        id: Option<u64>,
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_worker_completion_without_eog_proof_is_rejected_by_default() {
        let response: Response = serde_json::from_str(r#"{"kind":"completion","id":1,"text":"partial","prompt_tokens":1,"cached_tokens":0,"generated_tokens":1500,"ms":1}"#).unwrap();
        assert!(matches!(
            response,
            Response::Completion {
                reached_eog: false,
                ..
            }
        ));
    }
}
