// ─── shared files (same for every template) ──────────────────────────────────

pub const PRE_START: &str = include_str!("shared/pre-start");
pub const COLOR_SH: &str = include_str!("shared/color.sh");
/// Hello command template — replace `{name}` with the project name before writing.
pub const HELLO: &str = include_str!("shared/hello");

// ─── template definition ──────────────────────────────────────────────────────

pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    pub compose: &'static str,
    /// Optional Dockerfile written to the project root.
    pub dockerfile: Option<&'static str>,
    /// Extra lines appended to default.env — replace `{name}` with project name.
    pub extra_env: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "default",
        description: "Bare nginx placeholder",
        compose: include_str!("default/docker-compose.yml"),
        dockerfile: None,
        extra_env: "",
    },
    Template {
        name: "nestjs",
        description: "NestJS + PostgreSQL + Redis",
        compose: include_str!("nestjs/docker-compose.yml"),
        dockerfile: Some(include_str!("nestjs/Dockerfile")),
        extra_env: "POSTGRES_DB={name}\nPOSTGRES_USER={name}\nPOSTGRES_PASSWORD=secret\n",
    },
    Template {
        name: "nextjs",
        description: "Next.js + PostgreSQL",
        compose: include_str!("nextjs/docker-compose.yml"),
        dockerfile: Some(include_str!("nextjs/Dockerfile")),
        extra_env: "POSTGRES_DB={name}\nPOSTGRES_USER={name}\nPOSTGRES_PASSWORD=secret\n",
    },
    Template {
        name: "node",
        description: "Node.js (bare)",
        compose: include_str!("node/docker-compose.yml"),
        dockerfile: Some(include_str!("node/Dockerfile")),
        extra_env: "",
    },
    Template {
        name: "laravel",
        description: "Laravel + MySQL + Redis",
        compose: include_str!("laravel/docker-compose.yml"),
        dockerfile: Some(include_str!("laravel/Dockerfile")),
        extra_env: "MYSQL_DATABASE={name}\nMYSQL_ROOT_PASSWORD=secret\n",
    },
];

pub fn find(name: &str) -> anyhow::Result<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name).ok_or_else(|| {
        let available: Vec<&str> = TEMPLATES
            .iter()
            .filter(|t| t.name != "default")
            .map(|t| t.name)
            .collect();
        anyhow::anyhow!(
            "Unknown template '{}'. Available: {}",
            name,
            available.join(", ")
        )
    })
}

pub fn default() -> &'static Template {
    &TEMPLATES[0]
}

#[allow(dead_code)]
pub fn list() -> impl Iterator<Item = &'static Template> {
    TEMPLATES.iter().filter(|t| t.name != "default")
}
