FROM php:8.4-fpm-alpine

# System deps
RUN apk add --no-cache \
    bash curl git zip unzip \
    libpng-dev libjpeg-turbo-dev freetype-dev \
    nodejs npm \
    autoconf g++ make \
    && docker-php-ext-configure gd --with-freetype --with-jpeg \
    && docker-php-ext-install \
       pdo pdo_mysql bcmath gd opcache pcntl

# Redis PHP extension
RUN pecl install redis && docker-php-ext-enable redis \
    && apk del autoconf g++ make

# pnpm (for Vite)
RUN npm install -g pnpm

# Composer
COPY --from=composer:2 /usr/bin/composer /usr/bin/composer

WORKDIR /var/www/html

COPY docker-entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["entrypoint.sh"]
CMD ["php-fpm"]
