FROM golang:1.25-alpine

RUN apk add --no-cache bash curl git wget

# Air for hot-reload in development
RUN go install github.com/air-verse/air@latest

WORKDIR /app

COPY docker-entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["entrypoint.sh"]
CMD ["air"]
