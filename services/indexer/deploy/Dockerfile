# shortwin-indexer image (docs/design/INDEXER.md §12.1). Build context: the repository root
# (the indexer links programs/shortwin and crates/* by path).
FROM rust:1.98.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --manifest-path services/Cargo.toml -p shortwin-indexer --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 shortwin
COPY --from=build /src/services/target/release/shortwin-indexer /usr/local/bin/shortwin-indexer
USER shortwin
ENTRYPOINT ["shortwin-indexer"]
