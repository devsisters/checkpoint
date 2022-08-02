FROM lukemathwalker/cargo-chef:latest-rust-1.62.1 AS chef
WORKDIR app

FROM chef AS planner

COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
COPY src src

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock
COPY src src

RUN cargo build --release

FROM docker.io/debian:stable-slim AS runtime

COPY --from=builder /app/target/release/checkpoint /usr/local/bin/

CMD ["/usr/local/bin/checkpoint"]
