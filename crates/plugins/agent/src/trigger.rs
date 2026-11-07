use fujiang_core::{AtTarget, MessageEvent, Segment};

use crate::client::LlmError;
#[cfg(test)]
use crate::client::{ChatMessage, Role};

/// shortcut: one message capped at 1200 chars, split when a reply must stay complete.
const REPLY_CHAR_LIMIT: usize = 1200;

pub(crate) const BUSY_REPLY: &str = "福酱刚才没想出来，稍后再试。";

/// Whether this message is for the agent.
///
/// Wake words match as substrings after leading `@token` prefixes are removed.
/// `require_at` drops the wake-word path. A reply to the bot still counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    Miss,
    Hit { user_text: String },
}

pub fn claim(
    msg: &MessageEvent,
    wake_words: &[String],
    require_at: bool,
    reply_from_self: bool,
) -> Claim {
    let at_self = mentions_self(msg);
    let text = message_text(msg);
    let body = strip_leading_mentions(&text);
    let woke = wake_hit(body, wake_words);
    let addressed = if require_at {
        at_self || reply_from_self
    } else {
        at_self || woke || reply_from_self
    };
    if !addressed {
        return Claim::Miss;
    }
    Claim::Hit {
        user_text: prompt_text(body, wake_words),
    }
}

fn mentions_self(msg: &MessageEvent) -> bool {
    msg.segments.iter().any(|segment| match segment {
        Segment::At {
            target: AtTarget::User(uid),
        } => *uid == msg.self_id,
        _ => false,
    })
}

pub fn llm_reply(err: &LlmError) -> String {
    match err {
        LlmError::MissingApiKey { env } => format!("未配置 {env}"),
        LlmError::Timeout
        | LlmError::Http(_)
        | LlmError::Api { .. }
        | LlmError::Decode(_)
        | LlmError::Truncated(_) => BUSY_REPLY.to_string(),
        LlmError::ContextClosed => {
            "这个模型的上下文上限还没确认。官方表以外要由管理员写上核实过的 model_context_limit。"
                .to_string()
        }
        LlmError::OverBudget => "这次请求超过上下文上限。我没有删掉已有对话。".to_string(),
        LlmError::ImageUnmetered => {
            "这张图的视觉 token 还不能核定，这次不发给模型，也没有删历史。".to_string()
        }
        LlmError::PayloadLimited => {
            "这张图超过张数或大小限制，没有发给模型，也没有删历史。".to_string()
        }
    }
}

pub fn fit_reply(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= REPLY_CHAR_LIMIT {
        return text.to_string();
    }
    let mut out: String = text.chars().take(REPLY_CHAR_LIMIT - 1).collect();
    out.push('…');
    out
}

/// System prompt is injected per call and is not read from the session table.
#[cfg(test)]
pub fn build_messages(
    system_prompt: &str,
    history: Vec<ChatMessage>,
    user_text: String,
) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(history.len() + 2);
    let prompt = system_prompt.trim();
    if !prompt.is_empty() {
        messages.push(ChatMessage {
            role: Role::System,
            content: prompt.to_string(),
            images: Vec::new(),
        });
    }
    messages.extend(history);
    messages.push(ChatMessage {
        role: Role::User,
        content: user_text,
        images: Vec::new(),
    });
    messages
}

fn message_text(msg: &MessageEvent) -> String {
    let joined: String = msg
        .segments
        .iter()
        .filter_map(|segment| match segment {
            Segment::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if joined.is_empty() {
        msg.raw_text.clone()
    } else {
        joined
    }
}

fn strip_leading_mentions(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(after_at) = rest.strip_prefix('@') {
        let end = after_at
            .find(|c: char| c.is_whitespace())
            .unwrap_or(after_at.len());
        if end == 0 {
            break;
        }
        rest = after_at[end..].trim_start();
    }
    rest
}

fn wake_hit(body: &str, wake_words: &[String]) -> bool {
    wake_words
        .iter()
        .any(|word| !word.is_empty() && body.contains(word.as_str()))
}

fn prompt_text(body: &str, wake_words: &[String]) -> String {
    let mut stripped = body.to_string();
    for word in wake_words {
        if word.is_empty() {
            continue;
        }
        stripped = stripped.replace(word, " ");
    }
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    if !collapsed.is_empty() {
        return collapsed;
    }
    let raw = body.trim();
    if raw.is_empty() {
        "你好".to_string()
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujiang_core::{MessageEvent, Sender, Source};

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_string()).collect()
    }

    fn ev(self_id: i64, segments: Vec<Segment>, raw: &str) -> MessageEvent {
        MessageEvent {
            id: 1,
            time: 0,
            self_id,
            sender: Sender {
                user_id: 42,
                nickname: None,
                card: None,
            },
            source: Source::Group { id: 7 },
            segments,
            raw_text: raw.to_string(),
        }
    }

    #[test]
    fn at_self_hits_and_other_at_misses() {
        let wake = words(&["福酱"]);
        let hit = ev(
            9,
            vec![
                Segment::At {
                    target: AtTarget::User(9),
                },
                Segment::text(" 天气"),
            ],
            "",
        );
        assert_eq!(
            claim(&hit, &wake, false, false),
            Claim::Hit {
                user_text: "天气".into()
            }
        );
        let miss = ev(
            9,
            vec![Segment::At {
                target: AtTarget::User(3),
            }],
            "天气",
        );
        assert_eq!(claim(&miss, &wake, false, false), Claim::Miss);
        let all = ev(
            9,
            vec![Segment::At {
                target: AtTarget::All,
            }],
            "天气",
        );
        assert_eq!(claim(&all, &wake, false, false), Claim::Miss);
    }

    #[test]
    fn wake_word_is_a_substring_after_leading_mentions() {
        let wake = words(&["福酱"]);
        let mid = ev(9, vec![Segment::text("请问福酱今天天气")], "");
        assert_eq!(
            claim(&mid, &wake, false, false),
            Claim::Hit {
                user_text: "请问 今天天气".into()
            }
        );
        let lead = ev(9, vec![], "@12345  福酱 在吗");
        assert_eq!(
            claim(&lead, &wake, false, false),
            Claim::Hit {
                user_text: "在吗".into()
            }
        );
        let none = ev(9, vec![Segment::text("今天天气")], "");
        assert_eq!(claim(&none, &wake, false, false), Claim::Miss);
    }

    #[test]
    fn require_at_blocks_wake_word_but_not_reply() {
        let wake = words(&["福酱"]);
        let woke = ev(9, vec![Segment::text("福酱在吗")], "");
        assert_eq!(claim(&woke, &wake, true, false), Claim::Miss);
        assert_eq!(
            claim(&woke, &wake, true, true),
            Claim::Hit {
                user_text: "在吗".into()
            }
        );
        let quoted = ev(
            9,
            vec![Segment::Reply { id: 5 }, Segment::text("接着说")],
            "",
        );
        assert_eq!(claim(&quoted, &wake, false, false), Claim::Miss);
        assert_eq!(
            claim(&quoted, &wake, false, true),
            Claim::Hit {
                user_text: "接着说".into()
            }
        );
        let at = ev(
            9,
            vec![
                Segment::At {
                    target: AtTarget::User(9),
                },
                Segment::text("只艾特"),
            ],
            "",
        );
        assert_eq!(
            claim(&at, &wake, true, false),
            Claim::Hit {
                user_text: "只艾特".into()
            }
        );
    }

    #[test]
    fn bare_ping_keeps_the_wake_word() {
        let wake = words(&["福酱"]);
        let ping = ev(9, vec![Segment::text("福酱")], "");
        assert_eq!(
            claim(&ping, &wake, false, false),
            Claim::Hit {
                user_text: "福酱".into()
            }
        );
        let only_at = ev(
            9,
            vec![Segment::At {
                target: AtTarget::User(9),
            }],
            "",
        );
        assert_eq!(
            claim(&only_at, &wake, false, false),
            Claim::Hit {
                user_text: "你好".into()
            }
        );
    }

    #[test]
    fn fit_reply_cuts_on_chars_not_bytes() {
        let short = "你".repeat(REPLY_CHAR_LIMIT);
        assert_eq!(fit_reply(&short), short);
        let long = "你".repeat(REPLY_CHAR_LIMIT + 1);
        let fitted = fit_reply(&long);
        assert_eq!(fitted.chars().count(), REPLY_CHAR_LIMIT);
        assert!(fitted.ends_with('…'));
        assert_eq!(fit_reply("  hi  "), "hi");
        assert_eq!(fit_reply("   "), "");
    }

    #[test]
    fn llm_errors_are_chinese_sentences() {
        let missing = LlmError::MissingApiKey {
            env: "FUJIANG_LLM_KEY".into(),
        };
        assert_eq!(llm_reply(&missing), "未配置 FUJIANG_LLM_KEY");
        assert_eq!(llm_reply(&LlmError::Timeout), BUSY_REPLY);
        assert_eq!(
            llm_reply(&LlmError::Api {
                status: 500,
                body: "nope".into(),
            }),
            BUSY_REPLY
        );
    }

    #[test]
    fn build_messages_injects_system_once_per_call() {
        let history = vec![ChatMessage {
            role: Role::Assistant,
            content: "早".into(),
            images: Vec::new(),
        }];
        let messages = build_messages("  你是福酱  ", history, "天气".into());
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[0].content, "你是福酱");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].content, "天气");

        let bare = build_messages("  ", vec![], "在".into());
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].role, Role::User);
    }
}
