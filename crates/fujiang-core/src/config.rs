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
