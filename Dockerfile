# xseed 小助手镜像：静态链接的单个二进制，数据放在 /data
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev gcc make perl
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p xseed-agent && strip target/release/xseed-agent

FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata && adduser -D -u 1000 xseed && mkdir /data && chown xseed /data
COPY --from=build /src/target/release/xseed-agent /usr/local/bin/xseed-agent
USER xseed
ENV XSEED_DATA=/data
VOLUME /data
EXPOSE 7731
ENTRYPOINT ["xseed-agent"]
