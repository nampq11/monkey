use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    Proxy,
    Pat,
}

#[derive(Debug, Clone)]
pub struct Settings {
    // Webhook security
    pub github_webhook_secret: String,

    // Bot identity
    pub bot_login: String,
    pub git_author_name: String,
    pub git_author_email: String,

    // Scope
    pub repo_allowlist: String,

    // Engine (pi)
    pub model: String,
    pub thinking: String,
    pub provider: String,
    pub session_dir: String,

    // Concurrency / limits
    pub max_concurrency: usize,
    pub question_autoclose_hours: u64,

    // Release sentinel
    pub release_sentinel_enabled: bool,
    pub release_max_rounds: usize,

    // gh-proxy auth (mode-exclusive)
    pub gh_proxy_url: String,
    pub gh_proxy_hmac_key: String,

    // Direct PAT mode
    pub github_token: String,

    // Workspaces
    pub workspaces_root: String,
}

impl Settings {
    pub fn load_from_env() -> Result<Self, String> {
        let get_env = |keys: &[&str]| -> String {
            for key in keys {
                if let Ok(val) = env::var(key) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
            String::new()
        };

        let github_webhook_secret =
            get_env(&["GITHUB_WEBHOOK_SECRET", "MONKEY_GITHUB_WEBHOOK_SECRET"]);
        let bot_login = get_env(&["MONKEY_BOT_LOGIN", "ROBOMP_BOT_LOGIN"]);
        let git_author_name = {
            let val = get_env(&["MONKEY_GIT_AUTHOR_NAME"]);
            if val.is_empty() {
                "monkey".to_string()
            } else {
                val
            }
        };
        let git_author_email = {
            let val = get_env(&["MONKEY_GIT_AUTHOR_EMAIL"]);
            if val.is_empty() {
                "monkey@users.noreply.github.com".to_string()
            } else {
                val
            }
        };
        let repo_allowlist = get_env(&["MONKEY_REPO_ALLOWLIST", "REPO_ALLOWLIST"]);

        let model = get_env(&["MONKEY_MODEL"]);
        let thinking = {
            let val = get_env(&["MONKEY_THINKING"]);
            if val.is_empty() {
                "medium".to_string()
            } else {
                val
            }
        };
        let provider = get_env(&["MONKEY_PROVIDER"]);
        let session_dir = {
            let val = get_env(&["MONKEY_SESSION_DIR"]);
            if val.is_empty() {
                "/data/sessions".to_string()
            } else {
                val
            }
        };

        let max_concurrency = get_env(&["MONKEY_MAX_CONCURRENCY"])
            .parse::<usize>()
            .unwrap_or(8);
        let question_autoclose_hours = get_env(&["MONKEY_QUESTION_AUTOCLOSE_HOURS"])
            .parse::<u64>()
            .unwrap_or(4);

        let release_sentinel_enabled = matches!(
            get_env(&["MONKEY_RELEASE_SENTINEL_ENABLED"])
                .to_lowercase()
                .as_str(),
            "1" | "true" | "yes"
        );
        let release_max_rounds = get_env(&["MONKEY_RELEASE_MAX_ROUNDS"])
            .parse::<usize>()
            .unwrap_or(5);

        let gh_proxy_url = get_env(&["MONKEY_GH_PROXY_URL"]);
        let gh_proxy_hmac_key = get_env(&["MONKEY_GH_PROXY_HMAC_KEY"]);
        let github_token = get_env(&["GITHUB_TOKEN", "MONKEY_GITHUB_TOKEN"]);

        let workspaces_root = {
            let val = get_env(&["MONKEY_WORKSPACES_ROOT"]);
            if val.is_empty() {
                "/data/workspaces".to_string()
            } else {
                val
            }
        };

        let settings = Self {
            github_webhook_secret,
            bot_login,
            git_author_name,
            git_author_email,
            repo_allowlist,
            model,
            thinking,
            provider,
            session_dir,
            max_concurrency,
            question_autoclose_hours,
            release_sentinel_enabled,
            release_max_rounds,
            gh_proxy_url,
            gh_proxy_hmac_key,
            github_token,
            workspaces_root,
        };

        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> Result<(), String> {
        let has_proxy = !self.gh_proxy_url.is_empty() || !self.gh_proxy_hmac_key.is_empty();
        let has_pat = !self.github_token.is_empty();

        if has_proxy && has_pat {
            return Err("set either gh-proxy (URL + HMAC key) OR GITHUB_TOKEN, not both".into());
        }
        if !has_proxy && !has_pat {
            return Err("must set gh-proxy (URL + HMAC key) OR GITHUB_TOKEN".into());
        }
        if has_proxy && (self.gh_proxy_url.is_empty() || self.gh_proxy_hmac_key.is_empty()) {
            return Err("gh-proxy mode needs both GH_PROXY_URL and GH_PROXY_HMAC_KEY".into());
        }

        if self.github_webhook_secret.is_empty() {
            return Err("GITHUB_WEBHOOK_SECRET is required".into());
        }
        if self.bot_login.is_empty() {
            return Err("ROBOMP_BOT_LOGIN / MONKEY_BOT_LOGIN is required".into());
        }
        if self.repo_allowlist.is_empty() {
            return Err("REPO_ALLOWLIST is required".into());
        }

        Ok(())
    }

    pub fn allowlist(&self) -> Vec<String> {
        self.repo_allowlist
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn models(&self) -> Vec<String> {
        self.model
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn auth_mode(&self) -> AuthMode {
        if !self.gh_proxy_url.is_empty() && !self.gh_proxy_hmac_key.is_empty() {
            AuthMode::Proxy
        } else {
            AuthMode::Pat
        }
    }
}
