#[derive(Debug, Clone)]
pub struct BotConfig {
    pub command_prefix: String,
    pub groups: Vec<i64>,
    pub allow_private: bool,
    pub clist_username: String,
    pub clist_api_key: String,
    pub clist_limit: u32,
    pub contest_update_minutes: u64,
    pub rank_update_minutes: u64,
    pub daily_reset_hour: u32,
    pub fun_allow_mutate: bool,
    pub fun_admins: Vec<i64>,
    pub fun_delete_files: bool,
}

impl BotConfig {
    pub fn allowed(&self, source: crate::event::Source) -> bool {
        match source {
            crate::event::Source::Group { id } => {
                self.groups.is_empty() || self.groups.contains(&id)
            }
            crate::event::Source::Friend { .. } => self.allow_private,
        }
    }

    pub fn is_fun_admin(&self, qq: i64) -> bool {
        self.fun_admins.is_empty() || self.fun_admins.contains(&qq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;

    fn cfg(allow_private: bool, groups: Vec<i64>) -> BotConfig {
        BotConfig {
            command_prefix: ".".into(),
            groups,
            allow_private,
            clist_username: String::new(),
            clist_api_key: String::new(),
            clist_limit: 10,
            contest_update_minutes: 60,
            rank_update_minutes: 60,
            daily_reset_hour: 4,
            fun_allow_mutate: true,
            fun_admins: vec![],
            fun_delete_files: false,
        }
    }

    #[test]
    fn allowed_honors_private_switch_and_group_list() {
        let on = cfg(true, vec![]);
        assert!(on.allowed(Source::Friend { id: 2 }));
        assert!(on.allowed(Source::Group { id: 9 }));
        let off = cfg(false, vec![10]);
        assert!(!off.allowed(Source::Friend { id: 2 }));
        assert!(off.allowed(Source::Group { id: 10 }));
        assert!(!off.allowed(Source::Group { id: 9 }));
    }
}
