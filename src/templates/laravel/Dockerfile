FROM php:8.5-fpm-alpine
WORKDIR /var/www/html
RUN apk add --no-cache zip unzip \
    && docker-php-ext-install pdo pdo_mysql opcache
COPY --from=composer:2 /usr/bin/composer /usr/bin/composer
COPY composer*.json ./
RUN composer install --no-dev --no-scripts --prefer-dist
COPY . .
CMD ["php-fpm"]
