use base64::Engine;
use fujiang_core::{Media, MessageEvent, Segment};
use tracing::warn;

use crate::client::{ChatMessage, ImagePart, Role};

pub(crate) const SKIP_VISION: &str = "我现在还看不了图，先按文字回你。";
pub(crate) const FETCH_TIP: &str = "这张图我没取到，过一会儿再发一次。";
pub(crate) const TOO_BIG_TIP: &str = "这张图太大了，我看不了。";
pub(crate) const FORMAT_TIP: &str = "这张图的格式我看不了。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImagePlan {
    None,
    Skip,
    Load,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageFault {
    TooBig,
    Format,
}

pub(crate) fn message_images(msg: &MessageEvent) -> Vec<Media> {
    msg.segments
        .iter()
        .filter_map(|segment| match segment {
            Segment::Image { src, .. } => Some(src.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn image_plan(enabled: bool, image_count: usize) -> ImagePlan {
    if image_count == 0 {
        ImagePlan::None
    } else if enabled {
        ImagePlan::Load
    } else {
        ImagePlan::Skip
    }
}

pub(crate) fn image_part(bytes: &[u8], max_bytes: u64) -> Result<ImagePart, ImageFault> {
    if bytes.len() as u64 > max_bytes {
        return Err(ImageFault::TooBig);
    }
    let mime = sniff_mime(bytes).ok_or(ImageFault::Format)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ImagePart::new(format!("data:{mime};base64,{encoded}")))
}

pub(crate) fn fault_tip(fault: ImageFault) -> &'static str {
    match fault {
        ImageFault::TooBig => TOO_BIG_TIP,
        ImageFault::Format => FORMAT_TIP,
    }
}

pub(crate) fn loaded_part(
    max_bytes: u64,
    loaded: Result<Vec<u8>, ()>,
) -> Result<ImagePart, &'static str> {
    let bytes = loaded.map_err(|_| FETCH_TIP)?;
    match image_part(&bytes, max_bytes) {
        Ok(part) => Ok(part),
        Err(fault) => {
            note_image_fault(fault, bytes.len(), max_bytes);
            Err(fault_tip(fault))
        }
    }
}

pub(crate) fn give_images(messages: &mut [ChatMessage], images: Vec<ImagePart>) {
    if images.is_empty() {
        return;
    }
    let Some(message) = messages
        .iter_mut()
        .rfind(|message| message.role == Role::User)
    else {
        return;
    };
    message.images = images;
}

pub(crate) fn show_reply(model_text: &str, skipped: bool) -> String {
    let body = model_text.trim();
    if !skipped {
        return body.to_string();
    }
    if body.is_empty() {
        return SKIP_VISION.to_string();
    }
    format!("{SKIP_VISION}\n{body}")
}

pub(crate) fn note_image_fault(fault: ImageFault, bytes: usize, max_bytes: u64) {
    match fault {
        ImageFault::TooBig => warn!(bytes, max_bytes, "agent image"),
        ImageFault::Format => warn!(bytes, "agent image"),
    }
}

fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use fujiang_core::{MessageEvent, Sender, Source};

    use super::*;

    fn ev(segments: Vec<Segment>) -> MessageEvent {
        MessageEvent {
            id: 1,
            time: 0,
            self_id: 9,
            sender: Sender {
                user_id: 42,
                nickname: None,
                card: None,
            },
            source: Source::Group { id: 7 },
            segments,
            raw_text: String::new(),
        }
    }

    #[test]
    fn only_image_segments_are_collected() {
        let msg = ev(vec![
            Segment::text("看"),
            Segment::Image {
                src: Media::Path("a.jpg".into()),
                summary: None,
            },
            Segment::Video {
                src: Media::Path("b.mp4".into()),
                name: None,
            },
            Segment::Image {
                src: Media::Url("https://example.com/c.png".into()),
                summary: Some("图".into()),
            },
        ]);
        assert_eq!(
            message_images(&msg),
            vec![
                Media::Path("a.jpg".into()),
                Media::Url("https://example.com/c.png".into()),
            ]
        );
    }

    #[test]
    fn vision_off_skips_and_empty_does_not_load() {
        assert_eq!(image_plan(false, 2), ImagePlan::Skip);
        assert_eq!(image_plan(true, 0), ImagePlan::None);
        assert_eq!(image_plan(false, 0), ImagePlan::None);
        assert_eq!(image_plan(true, 1), ImagePlan::Load);
    }

    #[test]
    fn sniff_accepts_four_kinds_and_rejects_the_rest() {
        let cases: &[(&[u8], &str)] = &[
            (b"\xff\xd8\xff\x00", "image/jpeg"),
            (b"\x89PNG\r\n\x1a\n", "image/png"),
            (b"GIF89a", "image/gif"),
            (b"RIFF\x00\x00\x00\x00WEBP", "image/webp"),
        ];
        for (bytes, mime) in cases {
            let part = image_part(bytes, bytes.len() as u64).expect(mime);
            let prefix = format!("data:{mime};base64,");
            assert!(part.data_url().starts_with(&prefix), "{}", part.data_url());
            let encoded = part.data_url().rsplit_once(',').unwrap().1;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            assert_eq!(decoded, *bytes);
        }
        assert_eq!(image_part(b"not an image", 1024), Err(ImageFault::Format));
        assert_eq!(image_part(b"\xff\xd8\xff\x00", 3), Err(ImageFault::TooBig));
    }

    #[test]
    fn fetch_size_and_format_failures_are_chinese_tips() {
        assert_eq!(
            loaded_part(1024, Err(())).unwrap_err(),
            "这张图我没取到，过一会儿再发一次。"
        );
        assert_eq!(
            loaded_part(3, Ok(b"\xff\xd8\xff\x00".to_vec())).unwrap_err(),
            "这张图太大了，我看不了。"
        );
        assert_eq!(
            loaded_part(1024, Ok(b"hello".to_vec())).unwrap_err(),
            "这张图的格式我看不了。"
        );
        let part = loaded_part(1024, Ok(b"\xff\xd8\xff\x00".to_vec())).unwrap();
        assert!(part.data_url().starts_with("data:image/jpeg;base64,"));
    }

    #[test]
    fn give_images_lands_on_the_latest_user_turn() {
        let mut messages = vec![
            ChatMessage {
                role: Role::User,
                content: "旧".into(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "嗯".into(),
                images: Vec::new(),
            },
            ChatMessage {
                role: Role::User,
                content: "新".into(),
                images: Vec::new(),
            },
        ];
        let part = image_part(b"\xff\xd8\xff\x00", 100).unwrap();
        give_images(&mut messages, vec![part.clone()]);
        assert!(messages[0].images.is_empty());
        assert!(messages[1].images.is_empty());
        assert_eq!(messages[2].images, vec![part]);
    }

    #[test]
    fn skip_note_leads_the_reply_and_plain_text_stays_plain() {
        assert_eq!(show_reply("  猫  ", false), "猫");
        assert_eq!(show_reply("   ", false), "");
        assert_eq!(
            show_reply("猫", true),
            "我现在还看不了图，先按文字回你。\n猫"
        );
        assert_eq!(show_reply("  ", true), SKIP_VISION);
    }
}
