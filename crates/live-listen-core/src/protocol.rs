//! Speech provider protocol v1: JSON lines on stdin and stdout.
//!
//! The app writes `start`, `audio` (base64 PCM16, 16 kHz mono), `stop`, `cancel`, `prepare`
//! and `shutdown`. The provider answers with `hello`, `connected`, `text`, `final`, `end` and
//! `error`. `text.stable` counts the leading characters (Unicode scalar values) that the
//! service has finalized and will not revise.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToProvider {
    Start { id: u64, sample_rate: u32, language: String },
    Audio { id: u64, data: String },
    Stop { id: u64 },
    Cancel { id: u64 },
    Prepare,
    Shutdown,
}

impl ToProvider {
    pub fn audio(id: u64, pcm: &[i16]) -> ToProvider {
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        ToProvider::Audio { id, data: base64::engine::general_purpose::STANDARD.encode(bytes) }
    }
}

/// What a provider says it can do; the UI only offers matching features.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub interim: bool,
    #[serde(rename = "final")]
    pub finals: bool,
    pub timestamps: bool,
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromProvider {
    Hello {
        #[serde(default)]
        protocol: u32,
        #[serde(default)]
        name: String,
        #[serde(default)]
        capabilities: Capabilities,
    },
    Connected {
        id: u64,
    },
    Text {
        id: u64,
        text: String,
        #[serde(default)]
        stable: usize,
    },
    Final {
        id: u64,
        text: String,
    },
    End {
        id: u64,
        #[serde(default)]
        cancelled: bool,
    },
    Error {
        #[serde(default)]
        id: Option<u64>,
        code: String,
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_serialize_as_tagged_json() {
        let start =
            serde_json::to_string(&ToProvider::Start { id: 3, sample_rate: 16_000, language: "zh".into() }).unwrap();
        assert_eq!(start, r#"{"type":"start","id":3,"sample_rate":16000,"language":"zh"}"#);
        assert_eq!(serde_json::to_string(&ToProvider::Prepare).unwrap(), r#"{"type":"prepare"}"#);
        let audio = serde_json::to_value(ToProvider::audio(1, &[1, -1])).unwrap();
        assert_eq!(audio["data"], "AQD//w==");
    }

    #[test]
    fn responses_parse_with_defaults_and_unknown_types() {
        let hello: FromProvider = serde_json::from_str(
            r#"{"type":"hello","protocol":1,"name":"x","capabilities":{"interim":true,"final":true}}"#,
        )
        .unwrap();
        assert!(matches!(
            hello,
            FromProvider::Hello { capabilities: Capabilities { interim: true, finals: true, .. }, .. }
        ));
        let text: FromProvider = serde_json::from_str(r#"{"type":"text","id":2,"text":"你好"}"#).unwrap();
        assert_eq!(text, FromProvider::Text { id: 2, text: "你好".into(), stable: 0 });
        // Fields this app does not use, such as an old provider's speaker label, are ignored.
        let extra: FromProvider = serde_json::from_str(r#"{"type":"final","id":2,"text":"x","speaker":"S1"}"#).unwrap();
        assert_eq!(extra, FromProvider::Final { id: 2, text: "x".into() });
        let other: FromProvider = serde_json::from_str(r#"{"type":"progress","value":3}"#).unwrap();
        assert_eq!(other, FromProvider::Unknown);
        assert!(serde_json::from_str::<FromProvider>(r#"{"type":"text"}"#).is_err());
    }
}
