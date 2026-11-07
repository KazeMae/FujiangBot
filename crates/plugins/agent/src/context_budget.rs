pub const DEFAULT_CONTEXT_BUDGET: u64 = 500_000;
pub const OUTPUT_RESERVE: u64 = 4_096;
pub const DEEPSEEK_FLASH_CAP: u64 = 1_000_000;
pub const DEEPSEEK_FLASH_IMAGE_CAP: u64 = 1_024;
pub const MAX_INPUT_IMAGES: usize = 600;
pub const MAX_REQUEST_BODY_BYTES: u64 = 48 * 1024 * 1024;
pub const MAX_ONE_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_IMAGES_BYTES: u64 = 64 * 1024 * 1024;
pub const OFFICIAL_DEEPSEEK_HOST: &str = "api.deepseek.com";
pub const DEEPSEEK_FLASH_MODEL: &str = "deepseek-flash";

pub fn resolve(base_url: &str, model: &str, budget: u64, admin: Option<u64>) -> Option<u64> {
    let budget = if budget == 0 {
        DEFAULT_CONTEXT_BUDGET
    } else {
        budget
    };
    let known = official_cap(base_url, model);
    let name_cap = model_name_cap(model);
    let cap = if let Some(known) = known {
        Some(match admin {
            Some(admin) => known.min(admin),
            None => known,
        })
    } else {
        admin.map(|admin| match name_cap {
            Some(name_cap) => admin.min(name_cap),
            None => admin,
        })
    }?;
    Some(budget.min(cap))
}

pub fn image_token_cap(base_url: &str, model: &str, admin: Option<u64>) -> Option<u64> {
    let admin = admin.filter(|n| *n > 0);
    let official = if official_host(base_url) && model == DEEPSEEK_FLASH_MODEL {
        Some(DEEPSEEK_FLASH_IMAGE_CAP)
    } else {
        None
    };
    match (official, admin) {
        (Some(official), Some(admin)) => Some(official.max(admin)),
        (Some(official), None) => Some(official),
        (None, Some(admin)) => Some(admin),
        (None, None) => None,
    }
}

pub fn image_transport_blocked(count: usize, url_bytes: u64, decoded_bytes: u64) -> bool {
    count > MAX_INPUT_IMAGES
        || url_bytes > MAX_REQUEST_BODY_BYTES
        || decoded_bytes > MAX_IMAGES_BYTES
}

pub fn at_ninety(total: u64, limit: u64) -> bool {
    total.saturating_mul(10) >= limit.saturating_mul(9)
}

fn official_cap(base_url: &str, model: &str) -> Option<u64> {
    if official_host(base_url) && model == DEEPSEEK_FLASH_MODEL {
        Some(DEEPSEEK_FLASH_CAP)
    } else {
        None
    }
}

fn model_name_cap(model: &str) -> Option<u64> {
    if model == DEEPSEEK_FLASH_MODEL {
        Some(DEEPSEEK_FLASH_CAP)
    } else {
        None
    }
}

fn official_host(base_url: &str) -> bool {
    let rest = base_url.trim().trim_end_matches('/');
    let Some(rest) = rest.strip_prefix("https://") else {
        return false;
    };
    if rest.contains('@') {
        return false;
    }
    let host = rest.split('/').next().unwrap_or("");
    host == OFFICIAL_DEEPSEEK_HOST
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEEPSEEK_FLASH_CONTEXT_SOURCE: &str =
        "https://api-docs.deepseek.com/quick_start/pricing/";
    const DEEPSEEK_FLASH_VISION_SOURCE: &str = "https://api-docs.deepseek.com/guides/vision.md";
    const DEEPSEEK_FLASH_VISION_SCOPE: &str = "2026-11-07 https://api.deepseek.com deepseek-flash only; each image adds 1024; not an OpenAI, Anthropic, or other-model tile or detail cap";

    #[test]
    fn default_budget_is_half_a_million_and_official_flash_is_clamped() {
        assert_eq!(
            resolve(
                "https://api.deepseek.com",
                DEEPSEEK_FLASH_MODEL,
                DEFAULT_CONTEXT_BUDGET,
                None
            ),
            Some(DEFAULT_CONTEXT_BUDGET)
        );
        assert_eq!(
            resolve(
                "https://api.deepseek.com/",
                DEEPSEEK_FLASH_MODEL,
                2_000_000,
                Some(2_000_000)
            ),
            Some(DEEPSEEK_FLASH_CAP)
        );
        assert_eq!(
            resolve(
                "https://api.deepseek.com",
                DEEPSEEK_FLASH_MODEL,
                2_000_000,
                Some(32_000)
            ),
            Some(32_000)
        );
    }

    #[test]
    fn custom_url_does_not_inherit_the_official_cap() {
        assert_eq!(
            resolve("http://127.0.0.1:9", DEEPSEEK_FLASH_MODEL, 500_000, None),
            None
        );
        assert_eq!(
            resolve(
                "http://api.deepseek.com",
                DEEPSEEK_FLASH_MODEL,
                500_000,
                None
            ),
            None
        );
        assert_eq!(
            resolve(
                "https://api.deepseek.com.evil",
                DEEPSEEK_FLASH_MODEL,
                500_000,
                None
            ),
            None
        );
        assert_eq!(
            resolve(
                "https://user@api.deepseek.com",
                DEEPSEEK_FLASH_MODEL,
                500_000,
                None
            ),
            None
        );
        assert_eq!(
            resolve(
                "http://127.0.0.1:9",
                DEEPSEEK_FLASH_MODEL,
                2_000_000,
                Some(2_000_000)
            ),
            Some(DEEPSEEK_FLASH_CAP)
        );
        assert_eq!(
            resolve("http://127.0.0.1:9", "local-model", 80_000, Some(12_000)),
            Some(12_000)
        );
        assert_eq!(
            resolve("https://api.deepseek.com", "other-model", 500_000, None),
            None
        );
    }

    #[test]
    fn official_flash_image_cap_is_1024_and_cannot_be_lowered() {
        assert_eq!(
            image_token_cap("https://api.deepseek.com", DEEPSEEK_FLASH_MODEL, None),
            Some(DEEPSEEK_FLASH_IMAGE_CAP)
        );
        assert_eq!(
            image_token_cap("https://api.deepseek.com/", DEEPSEEK_FLASH_MODEL, Some(10)),
            Some(DEEPSEEK_FLASH_IMAGE_CAP)
        );
        assert_eq!(
            image_token_cap(
                "https://api.deepseek.com",
                DEEPSEEK_FLASH_MODEL,
                Some(4_000)
            ),
            Some(4_000)
        );
        assert_eq!(
            image_token_cap("http://127.0.0.1:9", DEEPSEEK_FLASH_MODEL, None),
            None
        );
        assert_eq!(
            image_token_cap("http://127.0.0.1:9", "local-model", Some(10)),
            Some(10)
        );
        assert_eq!(
            image_token_cap("https://api.deepseek.com", "other-model", None),
            None
        );
        assert!(image_transport_blocked(601, 10, 10));
        assert!(image_transport_blocked(1, MAX_REQUEST_BODY_BYTES + 1, 10));
        assert!(image_transport_blocked(2, 10, MAX_IMAGES_BYTES + 1));
        assert!(!image_transport_blocked(2, 10, 10));
    }

    #[test]
    fn image_cap_is_not_a_generic_tile_or_detail_table() {
        assert!(DEEPSEEK_FLASH_CONTEXT_SOURCE.contains("api-docs.deepseek.com"));
        assert!(DEEPSEEK_FLASH_VISION_SOURCE.ends_with("/guides/vision.md"));
        assert!(DEEPSEEK_FLASH_VISION_SCOPE.contains("deepseek-flash only"));
        assert_eq!(
            image_token_cap("https://api.openai.com/v1", "gpt-4o", None),
            None
        );
        assert_eq!(
            image_token_cap("https://api.openai.com/v1", "gpt-4o", Some(765)),
            Some(765)
        );
        assert_eq!(
            image_token_cap(
                "https://api.anthropic.com",
                "claude-3-5-sonnet",
                Some(1_600)
            ),
            Some(1_600)
        );
        assert_eq!(
            image_token_cap("https://api.deepseek.com", "deepseek-reasoner", Some(100)),
            Some(100)
        );
        assert_eq!(
            image_token_cap(
                "https://api.deepseek.com",
                "deepseek-flash-extra",
                Some(8_192)
            ),
            Some(8_192)
        );
        assert_eq!(
            image_token_cap("https://api.deepseek.com:443", DEEPSEEK_FLASH_MODEL, None),
            None
        );
        assert_eq!(
            image_token_cap(
                "https://gateway.example/api.deepseek.com",
                DEEPSEEK_FLASH_MODEL,
                Some(2048)
            ),
            Some(2048)
        );
        assert_eq!(
            resolve(
                "https://api.openai.com/v1",
                "gpt-4o",
                2_000_000,
                Some(2_000_000)
            ),
            Some(2_000_000)
        );
        assert_eq!(
            resolve(
                "https://api.anthropic.com",
                DEEPSEEK_FLASH_MODEL,
                2_000_000,
                Some(2_000_000)
            ),
            Some(DEEPSEEK_FLASH_CAP)
        );
    }

    #[test]
    fn ninety_percent_is_integer() {
        assert!(at_ninety(9, 10));
        assert!(!at_ninety(8, 10));
    }
}
