pub const DEFAULT_CONTEXT_BUDGET: u64 = 500_000;
pub const OUTPUT_RESERVE: u64 = 4_096;
pub const DEEPSEEK_FLASH_CAP: u64 = 1_000_000;
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
    fn ninety_percent_is_integer() {
        assert!(at_ninety(9, 10));
        assert!(!at_ninety(8, 10));
    }
}
