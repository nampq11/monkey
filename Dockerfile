FROM python:3.11-slim

WORKDIR /app

# System deps: git + node/npm (engine runtime), in one apt pass so the updated
# package lists are still present. --ignore-scripts for pi: skip postinstall
# hooks that may pull native deps or fail in a slim container.
RUN apt-get update && apt-get install -y --no-install-recommends \
    git nodejs npm \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent

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
# Default to the orchestrator; compose overrides this for the gh-proxy service.
CMD ["python", "-m", "monkey.cli", "serve"]
