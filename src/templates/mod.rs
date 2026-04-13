use include_dir::{Dir, include_dir};

// ─── embedded template directories ───────────────────────────────────────────
//
// Each directory mirrors the final project structure:
//   shared/hooks/pre-start          →   <project>/.dip/hooks/pre-start
//   nestjs/Dockerfile               →   <project>/Dockerfile
//   nestjs/docker-compose.yml       →   <project>/.dip/docker-compose.yml
//
// `shared` is always applied first; a named template is layered on top,
// overwriting any file that exists in both.

static SHARED: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/shared");
// ── Node.js ──────────────────────────────────────────────────────────────────
static NESTJS: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/nestjs");
static NEXTJS: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/nextjs");
static NUXT: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/nuxt");
static SVELTEKIT: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/sveltekit");
static REACT: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/react");
static ANGULAR: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/angular");
static EXPRESS: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/express");
static NODE: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/node");
// ── Python ───────────────────────────────────────────────────────────────────
static DJANGO: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/django");
static FASTAPI: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/fastapi");
// ── PHP ──────────────────────────────────────────────────────────────────────
static LARAVEL: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/laravel");
static WORDPRESS: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/wordpress");
static DRUPAL: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/drupal");
// ── Ruby ─────────────────────────────────────────────────────────────────────
static RAILS: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/rails");
// ── Go ───────────────────────────────────────────────────────────────────────
static GO: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/go");
// ── Rust ─────────────────────────────────────────────────────────────────────
static RUST_AXUM: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates/rust");

// ─── template registry ────────────────────────────────────────────────────────

pub struct TemplateMeta {
    pub name: &'static str,
    pub description: &'static str,
    pub dir: &'static Dir<'static>,
}

pub const TEMPLATES: &[TemplateMeta] = &[
    // ── Node.js ───────────────────────────────────────────────────────────────
    TemplateMeta {
        name: "nestjs",
        description: "NestJS + PostgreSQL + Valkey",
        dir: &NESTJS,
    },
    TemplateMeta {
        name: "nextjs",
        description: "Next.js + PostgreSQL + Valkey + Prisma",
        dir: &NEXTJS,
    },
    TemplateMeta {
        name: "nuxt",
        description: "Nuxt 3 + PostgreSQL + Valkey + Prisma",
        dir: &NUXT,
    },
    TemplateMeta {
        name: "sveltekit",
        description: "SvelteKit + PostgreSQL + Prisma",
        dir: &SVELTEKIT,
    },
    TemplateMeta {
        name: "react",
        description: "React + Vite (TypeScript)",
        dir: &REACT,
    },
    TemplateMeta {
        name: "angular",
        description: "Angular CLI (TypeScript)",
        dir: &ANGULAR,
    },
    TemplateMeta {
        name: "express",
        description: "Express 5 + PostgreSQL",
        dir: &EXPRESS,
    },
    TemplateMeta {
        name: "node",
        description: "Node.js (bare, bring your own stack)",
        dir: &NODE,
    },
    // ── Python ────────────────────────────────────────────────────────────────
    TemplateMeta {
        name: "django",
        description: "Django + PostgreSQL + Valkey + Celery",
        dir: &DJANGO,
    },
    TemplateMeta {
        name: "fastapi",
        description: "FastAPI + PostgreSQL + Valkey + Alembic",
        dir: &FASTAPI,
    },
    // ── PHP ───────────────────────────────────────────────────────────────────
    TemplateMeta {
        name: "laravel",
        description: "Laravel + MySQL + Valkey + nginx",
        dir: &LARAVEL,
    },
    TemplateMeta {
        name: "wordpress",
        description: "WordPress + MySQL + nginx + WP-CLI",
        dir: &WORDPRESS,
    },
    TemplateMeta {
        name: "drupal",
        description: "Drupal + MySQL + nginx + Drush",
        dir: &DRUPAL,
    },
    // ── Ruby ──────────────────────────────────────────────────────────────────
    TemplateMeta {
        name: "rails",
        description: "Ruby on Rails + PostgreSQL + Valkey + Sidekiq",
        dir: &RAILS,
    },
    // ── Go ────────────────────────────────────────────────────────────────────
    TemplateMeta {
        name: "go",
        description: "Go + PostgreSQL + Air (hot-reload)",
        dir: &GO,
    },
    // ── Rust ──────────────────────────────────────────────────────────────────
    TemplateMeta {
        name: "rust",
        description: "Axum + PostgreSQL + sqlx + cargo-watch",
        dir: &RUST_AXUM,
    },
];

pub fn shared() -> &'static Dir<'static> {
    &SHARED
}

pub fn find(name: &str) -> anyhow::Result<&'static TemplateMeta> {
    TEMPLATES.iter().find(|t| t.name == name).ok_or_else(|| {
        let available: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
        anyhow::anyhow!(
            "Unknown template '{}'. Available: {}",
            name,
            available.join(", ")
        )
    })
}
