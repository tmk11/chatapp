FROM rust:1.95-slim AS builder
WORKDIR /app
COPY backend/Cargo.toml ./backend/Cargo.toml
COPY backend/src ./backend/src
RUN cd backend && cargo build --release

FROM debian:trixie-slim
RUN useradd --system --uid 10001 chatapp
WORKDIR /app
COPY --from=builder /app/backend/target/release/secure-chat-backend /usr/local/bin/secure-chat-backend
COPY frontend ./frontend
ENV FRONTEND_DIR=/app/frontend
USER chatapp
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/secure-chat-backend"]
