FROM rust:1.95-slim AS builder
WORKDIR /app
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:trixie-slim
RUN useradd --system --uid 10001 chatapp
COPY --from=builder /app/target/release/secure-chat-backend /usr/local/bin/secure-chat-backend
USER chatapp
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/secure-chat-backend"]
