"""Monkey - config.

Settings load from MONKEY_* env vars via pydantic-settings. The two GitHub-name
variables (GITHUB_TOKEN, GITHUB_WEBHOOK_SECRET) keep their canonical names via
validation_alias so they stay recognizable and match GitHub/roboomp docs.
Mode-exclusive auth validation mirrors roboomp: either a gh-proxy (proxy URL +
HMAC key) OR a direct GITHUB_TOKEN, never both.
"""

from __future__ import annotations

from functools import lru_cache
from typing import Literal

from pydantic import Field, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="MONKEY_",
        case_sensitive=False,
        extra="ignore",
    )

    # --- webhook security ---
    github_webhook_secret: str = Field(default="", validation_alias="GITHUB_WEBHOOK_SECRET")

    # --- bot identity ---
    bot_login: str = ""  # lowercase mention handle, no '@' and no '[bot]'
    git_author_name: str = "monkey"
    git_author_email: str = "monkey@users.noreply.github.com"

    # --- scope ---
    repo_allowlist: str = ""  # comma-separated "owner/repo"

    # --- engine (pi) ---
    model: str = ""  # CSV of model patterns; picked randomly per task
    thinking: str = "medium"
    provider: str = ""
    session_dir: str = "/data/sessions"

    # --- concurrency / limits ---
    max_concurrency: int = 8
    question_autoclose_hours: int = 4

    # --- release sentinel (default off, dangerous) ---
    release_sentinel_enabled: bool = False
    release_max_rounds: int = 5

    # --- gh-proxy auth (mode-exclusive) ---
    gh_proxy_url: str = ""
    gh_proxy_hmac_key: str = ""

    # --- direct PAT mode ---
    github_token: str = Field(default="", validation_alias="GITHUB_TOKEN")

    # --- workspaces ---
    workspaces_root: str = "/data/workspaces"

    @property
    def allowlist(self) -> list[str]:
        return [s.strip() for s in self.repo_allowlist.split(",") if s.strip()]

    @property
    def models(self) -> list[str]:
        return [s.strip() for s in self.model.split(",") if s.strip()]

    @property
    def auth_mode(self) -> Literal["proxy", "pat"]:
        if self.gh_proxy_url and self.gh_proxy_hmac_key:
            return "proxy"
        if self.github_token:
            return "pat"
        raise ValueError("must configure gh_proxy (URL + HMAC key) OR github_token, not neither")

    @model_validator(mode="after")
    def _validate_proxy_or_pat(self) -> "Settings":
        has_proxy = bool(self.gh_proxy_url or self.gh_proxy_hmac_key)
        has_pat = bool(self.github_token)
        if has_proxy and has_pat:
            raise ValueError("set either gh-proxy (URL + HMAC key) OR GITHUB_TOKEN, not both")
        if not has_proxy and not has_pat:
            raise ValueError("must set gh-proxy (URL + HMAC key) OR GITHUB_TOKEN")
        if has_proxy and not (self.gh_proxy_url and self.gh_proxy_hmac_key):
            raise ValueError("gh-proxy mode needs both GH_PROXY_URL and GH_PROXY_HMAC_KEY")
        return self

    @model_validator(mode="after")
    def _required(self) -> "Settings":
        if not self.github_webhook_secret:
            raise ValueError("GITHUB_WEBHOOK_SECRET is required")
        if not self.bot_login:
            raise ValueError("ROBOMP_BOT_LOGIN / MONKEY_BOT_LOGIN is required")
        if not self.repo_allowlist:
            raise ValueError("REPO_ALLOWLIST is required")
        return self


@lru_cache
def get_settings() -> Settings:
    return Settings()
