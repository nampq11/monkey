use monkey::config::{AuthMode, Settings};

fn base_settings() -> Settings {
    Settings {
        github_webhook_secret: "secret".to_string(),
        bot_login: "monkey".to_string(),
        git_author_name: "monkey".to_string(),
        git_author_email: "monkey@example.com".to_string(),
        repo_allowlist: "acme/widget".to_string(),
        model: "m1,m2".to_string(),
        thinking: "medium".to_string(),
        provider: "".to_string(),
        session_dir: "/data/sessions".to_string(),
        max_concurrency: 8,
        question_autoclose_hours: 4,
        release_sentinel_enabled: false,
        release_max_rounds: 5,
        gh_proxy_url: "http://gh-proxy:8080".to_string(),
        gh_proxy_hmac_key: "key".to_string(),
        github_token: "".to_string(),
        workspaces_root: "/data/workspaces".to_string(),
    }
}

#[test]
fn test_valid_proxy_mode() {
    let s = base_settings();
    assert!(s.validate().is_ok());
    assert_eq!(s.auth_mode(), AuthMode::Proxy);
    assert_eq!(s.allowlist(), vec!["acme/widget"]);
    assert_eq!(s.models(), vec!["m1", "m2"]);
}

#[test]
fn test_valid_pat_mode() {
    let mut s = base_settings();
    s.gh_proxy_url = "".to_string();
    s.gh_proxy_hmac_key = "".to_string();
    s.github_token = "ghp_12345".to_string();
    assert!(s.validate().is_ok());
    assert_eq!(s.auth_mode(), AuthMode::Pat);
}

#[test]
fn test_both_proxy_and_pat_fails() {
    let mut s = base_settings();
    s.github_token = "ghp_12345".to_string();
    assert!(s.validate().is_err());
}

#[test]
fn test_neither_proxy_nor_pat_fails() {
    let mut s = base_settings();
    s.gh_proxy_url = "".to_string();
    s.gh_proxy_hmac_key = "".to_string();
    s.github_token = "".to_string();
    assert!(s.validate().is_err());
}

#[test]
fn test_partial_proxy_fails() {
    let mut s = base_settings();
    s.gh_proxy_hmac_key = "".to_string();
    assert!(s.validate().is_err());
}

#[test]
fn test_missing_required_fields_fails() {
    let mut s = base_settings();
    s.github_webhook_secret = "".to_string();
    assert!(s.validate().is_err());

    let mut s = base_settings();
    s.bot_login = "".to_string();
    assert!(s.validate().is_err());

    let mut s = base_settings();
    s.repo_allowlist = "".to_string();
    assert!(s.validate().is_err());
}

#[test]
fn test_zero_concurrency_fails() {
    let mut settings = base_settings();
    settings.max_concurrency = 0;
    assert!(settings.validate().is_err());
}
