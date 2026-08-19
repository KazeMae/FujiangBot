use fujiang_core::{
    AtTarget, Event, Media, MessageEvent, NoticeEvent, RequestEvent, Segment, Sender, Source,
};
use napcat_sdk::{Event as NcEvent, EventKind, RecvSegment, SendSegment, Structs};

pub fn map_event(nc: NcEvent) -> Option<Event> {
    if nc.is_message() {
        return map_message(&nc).map(Event::Message);
    }
    match nc.kind {
        EventKind::MetaHeartbeat
        | EventKind::MetaLifecycleConnect
        | EventKind::MetaLifecycleEnable
        | EventKind::MetaLifecycleDisable => Some(Event::Meta {
            kind: nc.kind.as_str().into(),
        }),
        EventKind::RequestFriend | EventKind::RequestGroupAdd | EventKind::RequestGroupInvite => {
            Some(Event::Request(RequestEvent {
                kind: nc.kind.as_str().into(),
                flag: nc
                    .raw
                    .get("flag")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .into(),
                user_id: nc.user_id().unwrap_or(0),
                group_id: nc.group_id(),
                comment: nc
                    .raw
                    .get("comment")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }))
        }
        EventKind::Unknown => None,
        k if k.as_str().starts_with("notice.") => Some(Event::Notice(NoticeEvent {
            kind: k.as_str().into(),
            group_id: nc.group_id(),
            user_id: nc.user_id(),
            raw: nc.raw,
        })),
        _ => None,
    }
}

pub fn map_message(nc: &NcEvent) -> Option<MessageEvent> {
    if !nc.is_message() && nc.post_type() != "message_sent" && nc.raw.get("message").is_none() {
        return None;
    }
    let source = if let Some(gid) = nc.group_id() {
        Source::Group { id: gid }
    } else if let Some(uid) = nc.user_id() {
        Source::Friend { id: uid }
    } else {
        return None;
    };
    let sender = nc.sender();
    let segments: Vec<Segment> = nc.message_segments().into_iter().map(from_recv).collect();
    let raw_text = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    Some(MessageEvent {
        id: nc.message_id().unwrap_or(0),
        time: nc.time().unwrap_or(0),
        self_id: nc.self_id().unwrap_or(0),
        sender: Sender {
            user_id: sender.user_id,
            nickname: sender.nickname,
            card: sender.card,
        },
        source,
        segments,
        raw_text,
    })
}

fn from_recv(s: RecvSegment) -> Segment {
    match s {
        RecvSegment::Text { text } => Segment::Text { text },
        RecvSegment::At { qq } => {
            if qq == "all" {
                Segment::At {
                    target: AtTarget::All,
                }
            } else {
                Segment::At {
                    target: AtTarget::User(qq.parse().unwrap_or(0)),
                }
            }
        }
        RecvSegment::Reply { id } => Segment::Reply {
            id: id.parse().unwrap_or(0),
        },
        RecvSegment::Image {
            file, url, summary, ..
        } => {
            let src = url
                .map(Media::Url)
                .or_else(|| file.clone().map(Media::FileId))
                .unwrap_or(Media::FileId(String::new()));
            Segment::Image { src, summary }
        }
        RecvSegment::File {
            file,
            file_id,
            name,
            ..
        } => Segment::File {
            src: Media::FileId(file_id.or(file).unwrap_or_default()),
            name,
        },
        RecvSegment::Face { id, .. } => Segment::Face { id },
        other => Segment::Unknown {
            ty: format!("{other:?}"),
            raw: serde_json::Value::Null,
        },
    }
}

pub fn to_send(s: &Segment) -> SendSegment {
    match s {
        Segment::Text { text } => Structs::text(text),
        Segment::At {
            target: AtTarget::All,
        } => Structs::at_all(),
        Segment::At {
            target: AtTarget::User(qq),
        } => Structs::at(*qq),
        Segment::Reply { id } => Structs::reply(*id),
        Segment::Image { src, summary } => {
            let mut img = Structs::image(onebot_file(src));
            if let SendSegment::Image { summary: slot, .. } = &mut img {
                *slot = summary.clone();
            }
            img
        }
        Segment::File { src, name } => Structs::file(onebot_file(src), name.clone()),
        Segment::Face { id } => Structs::face(id),
        Segment::Unknown { .. } => Structs::text(""),
    }
}

/// NapCat / LLOneBot reject relative paths (`识别URL失败`). Local files go out as `file://`.
fn onebot_file(src: &Media) -> String {
    match src {
        Media::Url(s) | Media::FileId(s) => s.clone(),
        Media::Base64(s) => {
            if s.starts_with("base64://") {
                s.clone()
            } else {
                format!("base64://{s}")
            }
        }
        Media::Path(s) => local_path_to_file_uri(s),
    }
}

fn local_path_to_file_uri(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.starts_with("file://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("base64://")
    {
        return trimmed.to_string();
    }
    let p = std::path::PathBuf::from(trimmed);
    let abs = if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
    };
    let abs = abs.canonicalize().unwrap_or(abs);
    url::Url::from_file_path(&abs)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| {
            let s = abs.to_string_lossy().replace('\\', "/");
            if s.starts_with('/') {
                format!("file://{s}")
            } else {
                format!("file:///{s}")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_becomes_file_uri() {
        let uri = local_path_to_file_uri("data/images/ae052129e8426c1bf63251c77b171a90.jpg");
        assert!(uri.starts_with("file://"), "{uri}");
        assert!(
            uri.contains("data/images/ae052129e8426c1bf63251c77b171a90.jpg"),
            "{uri}"
        );
        assert!(!uri.starts_with("file://data"), "{uri}");
    }

    #[test]
    fn already_uri_left_alone() {
        assert_eq!(
            local_path_to_file_uri("file:///tmp/a.jpg"),
            "file:///tmp/a.jpg"
        );
        assert_eq!(
            local_path_to_file_uri("https://example.com/a.jpg"),
            "https://example.com/a.jpg"
        );
    }

    #[test]
    fn image_segment_uses_file_uri() {
        let seg = Segment::Image {
            src: Media::Path("data/images/x.jpg".into()),
            summary: None,
        };
        match to_send(&seg) {
            SendSegment::Image { file, .. } => {
                assert!(file.starts_with("file://"), "{file}");
            }
            other => panic!("{other:?}"),
        }
    }
}
