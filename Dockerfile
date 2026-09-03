FROM python:3.11-slim

WORKDIR /app

# System deps for git + build.
RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    && rm -rf /var/lib/apt/lists/*

# Install pi (the coding-agent engine) + its Node runtime.
RUN apt-get install -y --no-install-recommends nodejs npm \
    && npm install -g @oh-my-pi/pi-coding-agent \
    && rm -rf /var/lib/apt/lists/*

COPY pyproject.toml ./
COPY monkey ./monkey
RUN pip install --no-cache-dir .

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# The orchestrator refuses to run if it sees GITHUB_TOKEN in ITS OWN env; the
# proxy holds it instead. We scaffold the model config for the host gateway.
# (See README: point ~/.omp/agent/models.container.yml at your LiteLLM gateway.)

EXPOSE 8000 8080
ENTRYPOINT ["/entrypoint.sh"]
