# Multi-stage build for monkey orchestrator: Rust binary on Node 22 (~250MB)
FROM rust:alpine AS chef
RUN apk add --no-cache musl-dev gcc git
# Pinned so dependency pre-building stays reproducible across base image updates.
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Compile dependencies in their own layer; only manifest changes invalidate it.
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release

# Runtime image: Node 22 on Alpine with git and pi coding agent engine
FROM node:22-alpine

RUN apk add --no-cache git ca-certificates

RUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent

COPY --from=builder /app/target/release/monkey /usr/local/bin/monkey

EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/monkey"]
CMD ["serve"]
