FROM rust:alpine

RUN apk add --no-cache bash curl musl-dev postgresql-dev wget

# cargo-watch for hot-reload
RUN cargo install cargo-watch

WORKDIR /app

COPY docker-entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["entrypoint.sh"]
CMD ["cargo", "watch", "-x", "run"]
