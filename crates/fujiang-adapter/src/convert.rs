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
            let mut img = Structs::image(src.as_file_str());
            if let SendSegment::Image { summary: slot, .. } = &mut img {
                *slot = summary.clone();
            }
            img
        }
        Segment::File { src, name } => Structs::file(src.as_file_str(), name.clone()),
        Segment::Face { id } => Structs::face(id),
        Segment::Unknown { .. } => Structs::text(""),
    }
}
