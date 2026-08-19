use serde_json::{json, Value};

pub fn cq_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
        .replace(',', "&#44;")
}

pub fn cq_unescape(text: &str) -> String {
    text.replace("&#44;", ",")
        .replace("&#91;", "[")
        .replace("&#93;", "]")
        .replace("&amp;", "&")
}

/// Convert a CQ-code string into OneBot array-format message.
pub fn cq_to_array(text: &str) -> Vec<Value> {
    let decoded = cq_unescape(text);
    let bytes = decoded.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut buf = String::new();

    while i < bytes.len() {
        if bytes[i] == b'[' && decoded[i..].starts_with("[CQ:") {
            if !buf.is_empty() {
                out.push(json!({"type":"text","data":{"text": buf}}));
                buf.clear();
            }
            if let Some(end) = decoded[i..].find(']') {
                let inner = &decoded[i + 4..i + end];
                out.push(parse_cq_inner(inner));
                i += end + 1;
                continue;
            }
        }
        buf.push(decoded[i..].chars().next().unwrap());
        i += decoded[i..].chars().next().unwrap().len_utf8();
    }
    if !buf.is_empty() {
        out.push(json!({"type":"text","data":{"text": buf}}));
    }
    out
}

fn parse_cq_inner(inner: &str) -> Value {
    let mut parts = inner.split(',');
    let ty = parts.next().unwrap_or("unknown");
    let mut data = serde_json::Map::new();
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            data.insert(k.to_string(), Value::String(cq_unescape(v)));
        }
    }
    json!({"type": ty, "data": data})
}

pub fn array_to_cq(segments: &[Value]) -> String {
    let mut out = String::new();
    for seg in segments {
        let ty = seg
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        if ty == "text" {
            let text = seg
                .pointer("/data/text")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            out.push_str(&cq_escape(text));
            continue;
        }
        out.push_str("[CQ:");
        out.push_str(ty);
        if let Some(obj) = seg.get("data").and_then(|d| d.as_object()) {
            for (k, v) in obj {
                if v.is_null() {
                    continue;
                }
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out.push(',');
                out.push_str(k);
                out.push('=');
                out.push_str(&cq_escape(&val));
            }
        }
        out.push(']');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_mixed() {
        let segs = cq_to_array("hi[CQ:at,qq=123]!");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0]["data"]["text"], "hi");
        assert_eq!(segs[1]["type"], "at");
        assert_eq!(segs[1]["data"]["qq"], "123");
        assert_eq!(segs[2]["data"]["text"], "!");
    }

    #[test]
    fn unescape_then_decode() {
        let segs = cq_to_array("a&amp;b");
        assert_eq!(segs[0]["data"]["text"], "a&b");
    }
}
