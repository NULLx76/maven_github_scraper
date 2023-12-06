FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
RUN cargo build --release --bin pom-aggregator

FROM debian:bookworm-slim AS runtime
COPY --from=builder /app/target/release/pom-aggregator /usr/local/bin

ENV RUST_LOG=info
CMD ["/usr/local/bin/pom-aggregator", "/data/poms"]
