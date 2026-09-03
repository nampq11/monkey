# pi (the engine) needs Node >= 22.19. Use node:22-bookworm-slim as the base so
# node + npm are present and correct, then add Python 3.11 + git for the app.
FROM node:22-bookworm-slim

WORKDIR /app

# Python 3.11 + git. python3.11 on bookworm-slim; node:22 is Debian bookworm.
RUN apt-get update && apt-get install -y --no-install-recommends \
    git ca-certificates python3.11 python3.11-dev python3-pip \
    && ln -sf /usr/bin/python3 /usr/local/bin/python \
    && rm -rf /var/lib/apt/lists/*

# pi engine (prebuilt). --ignore-scripts keeps the slim install reliable.
RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent

COPY pyproject.toml ./
COPY monkey ./monkey
# --break-system-packages: Debian bookworm (PEP 668) blocks system-wide pip; this
# is a disposable runtime image, so overriding is safe and correct here.
RUN python3.11 -m pip install --no-cache-dir --break-system-packages .

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 8000 8080
ENTRYPOINT ["/entrypoint.sh"]
CMD ["python", "-m", "monkey.cli", "serve"]
