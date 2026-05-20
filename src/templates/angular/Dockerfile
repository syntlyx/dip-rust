FROM node:24-alpine

ENV PNPM_HOME=/pnpm
ENV COREPACK_HOME=/pnpm/corepack
ENV PNPM_STORE_DIR=/pnpm/store
ENV PATH="${PNPM_HOME}:${PATH}"

RUN npm install -g corepack@latest \
  && corepack enable pnpm \
  && corepack prepare pnpm@latest --activate \
  && mkdir -p "${PNPM_HOME}" "${COREPACK_HOME}" "${PNPM_STORE_DIR}" /app \
  && chown -R node:node "${PNPM_HOME}" /app

WORKDIR /app

COPY docker-entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["entrypoint.sh"]
CMD ["pnpm", "exec", "ng", "serve", "--host", "0.0.0.0", "--poll", "500"]
