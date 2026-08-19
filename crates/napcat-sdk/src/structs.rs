use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outgoing message segment (OneBot array format).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum SendSegment {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "at")]
    At { qq: String },
    #[serde(rename = "reply")]
    Reply { id: String },
    #[serde(rename = "face")]
    Face { id: String },
    #[serde(rename = "mface")]
    Mface {
        emoji_id: String,
        emoji_package_id: String,
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    #[serde(rename = "image")]
    Image {
        file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sub_type: Option<String>,
    },
    #[serde(rename = "file")]
    File {
        file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    #[serde(rename = "video")]
    Video {
        file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumb: Option<String>,
    },
    #[serde(rename = "record")]
    Record {
        file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thumb: Option<String>,
    },
    #[serde(rename = "json")]
    Json { data: String },
    #[serde(rename = "markdown")]
    Markdown { content: String },
    #[serde(rename = "dice")]
    Dice {},
    #[serde(rename = "rps")]
    Rps {},
    #[serde(rename = "music")]
    Music(Value),
    #[serde(rename = "node")]
    Node(Value),
    #[serde(rename = "forward")]
    Forward { id: String },
    #[serde(rename = "contact")]
    Contact {
        #[serde(rename = "type")]
        kind: String,
        id: String,
    },
    #[serde(rename = "poke")]
    Poke {
        #[serde(rename = "type")]
        kind: String,
        id: String,
    },
}

/// Incoming segment. Unknown types keep the raw object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum RecvSegment {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "at")]
    At { qq: String },
    #[serde(rename = "reply")]
    Reply { id: String },
    #[serde(rename = "face")]
    Face {
        id: String,
        #[serde(default)]
        raw: Option<Value>,
    },
    #[serde(rename = "image")]
    Image {
        #[serde(default)]
        file: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        sub_type: Option<Value>,
    },
    #[serde(rename = "file")]
    File {
        #[serde(default)]
        file: Option<String>,
        #[serde(default)]
        file_id: Option<String>,
        #[serde(default)]
        file_size: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    #[serde(rename = "video")]
    Video {
        #[serde(default)]
        file: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        file_size: Option<String>,
    },
    #[serde(rename = "record")]
    Record {
        #[serde(default)]
        file: Option<String>,
        #[serde(default)]
        file_size: Option<String>,
    },
    #[serde(rename = "forward")]
    Forward {
        id: String,
        #[serde(default)]
        content: Option<Vec<RecvSegment>>,
    },
    #[serde(rename = "json")]
    Json { data: String },
    #[serde(rename = "markdown")]
    Markdown { content: String },
    #[serde(rename = "poke")]
    Poke {
        #[serde(default)]
        #[serde(rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        id: Option<String>,
    },
    #[serde(rename = "dice")]
    Dice {
        #[serde(default)]
        result: Option<String>,
    },
    #[serde(rename = "rps")]
    Rps {
        #[serde(default)]
        result: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

impl RecvSegment {
    pub fn from_value(v: &Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or(RecvSegment::Unknown)
    }
}

pub struct Structs;

impl Structs {
    pub fn text(text: impl Into<String>) -> SendSegment {
        SendSegment::Text { text: text.into() }
    }

    pub fn at(qq: impl ToString) -> SendSegment {
        SendSegment::At { qq: qq.to_string() }
    }

    pub fn at_all() -> SendSegment {
        SendSegment::At { qq: "all".into() }
    }

    pub fn reply(id: impl ToString) -> SendSegment {
        SendSegment::Reply { id: id.to_string() }
    }

    pub fn face(id: impl ToString) -> SendSegment {
        SendSegment::Face { id: id.to_string() }
    }

    pub fn image(file: impl Into<String>) -> SendSegment {
        SendSegment::Image {
            file: file.into(),
            summary: None,
            sub_type: None,
        }
    }

    pub fn image_base64(bytes: &[u8]) -> SendSegment {
        SendSegment::Image {
            file: format!("base64://{}", value_base64(bytes)),
            summary: None,
            sub_type: None,
        }
    }

    pub fn file(file: impl Into<String>, name: Option<String>) -> SendSegment {
        SendSegment::File {
            file: file.into(),
            name,
        }
    }

    pub fn video(file: impl Into<String>) -> SendSegment {
        SendSegment::Video {
            file: file.into(),
            name: None,
            thumb: None,
        }
    }

    pub fn record(file: impl Into<String>) -> SendSegment {
        SendSegment::Record {
            file: file.into(),
            name: None,
            thumb: None,
        }
    }

    pub fn json(data: impl Into<String>) -> SendSegment {
        SendSegment::Json { data: data.into() }
    }

    pub fn markdown(content: impl Into<String>) -> SendSegment {
        SendSegment::Markdown {
            content: content.into(),
        }
    }

    pub fn dice() -> SendSegment {
        SendSegment::Dice {}
    }

    pub fn rps() -> SendSegment {
        SendSegment::Rps {}
    }

    pub fn forward(id: impl ToString) -> SendSegment {
        SendSegment::Forward { id: id.to_string() }
    }
}

fn value_base64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(T[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(T[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip() {
        let s = Structs::text("hello");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["type"], "text");
        assert_eq!(v["data"]["text"], "hello");
    }
}
