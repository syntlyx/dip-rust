FROM ruby:3.4-alpine

RUN apk add --no-cache \
    bash curl git build-base wget \
    postgresql-dev nodejs npm yaml-dev

RUN npm install -g pnpm

WORKDIR /app

COPY docker-entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["entrypoint.sh"]
CMD ["rails", "server", "-b", "0.0.0.0", "-p", "3000"]
