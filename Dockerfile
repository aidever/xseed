# xseed 镜像：默认运行小助手（数据在 /data）；运行社区节点用 --entrypoint xseed-node（数据在 XSEED_NODE_DATA）
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc make perl
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p xseed-agent -p xseed-node

FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata && adduser -D -u 1000 xseed && mkdir /data && chown xseed /data
COPY --from=build /src/target/release/xseed-agent /src/target/release/xseed-node /usr/local/bin/
USER xseed
ENV XSEED_DATA=/data
VOLUME /data
ENV XSEED_NODE_DATA=/data/node
EXPOSE 7731 7800
ENTRYPOINT ["xseed-agent"]
