# Optional container image for the JeffyCoin relay.
#
# Build context must be the cargo package directory (repo's `jcoin/`), e.g.:
#   docker build -f deploy/Dockerfile -t jcoin-relay ./jcoin
#
# rust:alpine builds a static musl binary, and --no-default-features drops the
# wallet's reqwest/TLS stack, so the relay links nothing extra. The result runs
# on a tiny Alpine image (no OpenSSL, no glibc).

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --no-default-features --bin relay

FROM alpine:3.20
COPY --from=build /src/target/release/relay /usr/local/bin/jcoin-relay
EXPOSE 9000
ENTRYPOINT ["/usr/local/bin/jcoin-relay", "--addr", "0.0.0.0:9000"]
