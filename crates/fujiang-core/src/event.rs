use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Group { id: i64 },
    Friend { id: i64 },
}

impl Source {
    pub fn id(self) -> i64 {
        match self {
            Self::Group { id } | Self::Friend { id } => id,
        }
    }

    pub fn is_group(self) -> bool {
        matches!(self, Self::Group { .. })
    }

    pub fn group_id(self) -> Option<i64> {
        match self {
            Self::Group { id } => Some(id),
            Self::Friend { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtTarget {
    User(i64),
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Media {
    Url(String),
    Path(String),
    Base64(String),
    FileId(String),
}

impl Media {
    pub fn as_file_str(&self) -> String {
        match self {
            Self::Url(s) | Self::Path(s) | Self::FileId(s) => s.clone(),
            Self::Base64(s) => {
                if s.starts_with("base64://") {
                    s.clone()
                } else {
                    format!("base64://{s}")
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Segment {
    Text { text: String },
    At { target: AtTarget },
    Image { src: Media, summary: Option<String> },
    Reply { id: i64 },
    File { src: Media, name: Option<String> },
    Face { id: String },
    Unknown { ty: String, raw: Value },
}

impl Segment {
    pub fn text(t: impl Into<String>) -> Self {
        Self::Text { text: t.into() }
    }
}

#[derive(Debug, Clone)]
pub struct Sender {
    pub user_id: i64,
    pub nickname: Option<String>,
    pub card: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MessageEvent {
    pub id: i64,
    pub time: i64,
    pub self_id: i64,
    pub sender: Sender,
    pub source: Source,
    pub segments: Vec<Segment>,
    pub raw_text: String,
}

impl MessageEvent {
    pub fn user_id(&self) -> i64 {
        self.sender.user_id
    }

    pub fn reply_id(&self) -> Option<i64> {
        self.segments.iter().find_map(|s| match s {
            Segment::Reply { id } => Some(*id),
            _ => None,
        })
    }

    pub fn first_image(&self) -> Option<&Media> {
        self.segments.iter().find_map(|s| match s {
            Segment::Image { src, .. } => Some(src),
            _ => None,
        })
    }

    pub fn command_line(&self) -> String {
        self.raw_text.trim().to_string()
    }
}

#[derive(Debug, Clone)]
pub struct NoticeEvent {
    pub kind: String,
    pub group_id: Option<i64>,
    pub user_id: Option<i64>,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct RequestEvent {
    pub kind: String,
    pub flag: String,
    pub user_id: i64,
    pub group_id: Option<i64>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Event {
    Message(MessageEvent),
    Notice(NoticeEvent),
    Request(RequestEvent),
    Meta { kind: String },
}

impl Event {
    pub fn as_message(&self) -> Option<&MessageEvent> {
        match self {
            Self::Message(m) => Some(m),
            _ => None,
        }
    }
}
