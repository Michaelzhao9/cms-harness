# CMS Harness: one binary and one SQLite file. Data lives in /data (mount a volume there).
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked && strip target/release/cms-harness

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home /data harness && mkdir /data && chown harness /data
COPY --from=build /src/target/release/cms-harness /usr/local/bin/cms-harness
USER harness
ENV HC_HOST=0.0.0.0 HC_PORT=4200 HC_DATA=/data
VOLUME /data
EXPOSE 4200
CMD ["cms-harness"]
