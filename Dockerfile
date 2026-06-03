# Stage 1: Build
FROM rust:slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev curl && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -rf src

COPY src ./src
RUN touch src/main.rs
RUN cargo build --release

# Stage 2: Run
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/* && apt-get clean

WORKDIR /app
COPY --from=builder /app/target/release/jntuh-api-rust .
COPY exam_codes.json .

EXPOSE 8000

ENV PORT=8000
ENV LOG_FORMAT=json
ENV RUST_LOG=info

CMD ["./jntuh-api-rust"]
