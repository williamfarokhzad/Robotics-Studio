# syntax=docker/dockerfile:1
FROM rust:1.95-slim-bookworm AS gateway
WORKDIR /gateway
COPY cloud/Cargo.toml cloud/Cargo.lock ./
COPY cloud/src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry CARGO_BUILD_JOBS=2 cargo build --release --locked

FROM rust:1.95-slim-bookworm AS cad
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libfontconfig1-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 RUSTFLAGS='-C codegen-units=1' cargo build --profile ci --locked -p solidcraft-cli --bin solidcraft-cli

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libfontconfig1 libvulkan1 mesa-vulkan-drivers libegl1 libgl1 && rm -rf /var/lib/apt/lists/* && useradd --create-home --uid 10001 studio
COPY --from=gateway /gateway/target/release/solidcraft-cloud /usr/local/bin/
COPY --from=cad /src/target/ci/solidcraft-cli /usr/local/bin/
COPY LICENSE-MIT LICENSE-APACHE NOTICE THIRD-PARTY-LICENSES.txt /usr/share/doc/solidcraft/
COPY cloud/THIRD-PARTY-LICENSES.txt /usr/share/doc/solidcraft/cloud-dependencies.txt
USER studio
ENV PORT=10000
EXPOSE 10000
CMD ["solidcraft-cloud"]
