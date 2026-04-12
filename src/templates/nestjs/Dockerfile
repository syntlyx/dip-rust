FROM node:24-alpine

RUN corepack enable && corepack prepare pnpm@latest --activate

# nest cli globally — for scaffold on first boot
RUN npm install -g @nestjs/cli

WORKDIR /app

COPY docker-entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["entrypoint.sh"]
CMD ["pnpm", "run", "start:dev"]
