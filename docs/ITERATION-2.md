# Итерация 2 — задачи

Цель (SPEC §19.2): проекты и workspace, служебная сессия воркера, импорт и зеркало VS Code,
подхват в обе стороны, артефакты, `.hqc/inbox`. Критерий: подхватить с телефона сессию,
начатую в VS Code. Упрощения итерации 1 «один проект (личный)» и «один workspace из конфига
воркера» сняты в T14.

Каждая задача самодостаточна: её можно отдать отдельному агенту вместе с `CLAUDE.md`
и указанными разделами `docs/SPEC.md`. Зависимости указаны. Остальные задачи итерации
добавляются сюда по мере постановки.

## T14. Проекты и несколько workspace (сделано, SPEC 0.16)

Разделы: §3 (Project, Workspace, Session, Worker), §5.3, §5.4, §6, §13, §14, §19.2.
- hqc-proto: кадры hub → worker `fs.stat {request_id, path}` → `result {exists, is_dir,
  canonical, inside_allowed_roots}` и `fs.list_dirs {request_id, path | null}` → `result
  {entries[{name, path}]}`; тела REST проектов и workspace; примеры в `examples/wire`;
  `workspace_path` убран из примера `hello`.
- Хаб: CRUD проектов (личный не удаляется и не переименовывается), `default_workspace_id`
  — первый workspace проекта; `POST /api/workspaces` проверяет путь через `fs.stat`
  (офлайн — `409 worker_offline`), хранит `canonical`; на одном воркере пути уникальны и не
  вложены (`409 workspace_conflict` с `existing_workspace_id`, `existing_project_id`,
  `existing_path`); `GET /api/workers/{id}/dirs` проксирует `fs.list_dirs`; сессия
  привязана к `workspace_id` (обязателен) и `cwd`; миграция переносит workspace итерации 1
  и архивирует сессии без известной папки.
- Воркер: без `[workspace]` в `worker.toml` и `workspace_path` в `hello`; путь из
  `session.start` — внутри `allowed_roots`, `cwd` — внутри workspace; ответы на `fs.*`
  только внутри `allowed_roots`.
- Клиент: левая колонка — проекты и сессии выбранного; создание проекта; экран
  «Workspaces» (список, добавление с деревом папок и ручным путём, удаление записи);
  выбор workspace и `cwd` при создании сессии; «эта папка уже в проекте <имя>» со ссылкой.
- `tools/dev.sh`, `tools/e2e-local.sh`: workspace создаётся через REST после того, как
  воркер online.
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (интеграционные тесты хаба: создание workspace с
проверкой фейковым воркером, 409 на дубликат, вложенный и содержащий путь, 409 при офлайн
воркере, миграция существующих сессий, `list_dirs` только внутри `allowed_roots`),
`cd client && npm test && npm run build`, `E2E_CLAUDE_BIN=/usr/bin/false
tools/e2e-local.sh` печатает `PASS (rehearsal)`. Вопросы — `docs/notes/t14-questions.md`.

## T16. Артефакты и `.hqc/inbox` (сделано, SPEC 0.17)

Разделы: §3 Artifact, §4 (`user.message.artifact_ids`, `attachment`), §5.3 `session.input`,
§6, §8.1, §12, §20. Без MCP: `hqc_attach` и `PUT /worker/artifacts` — итерация 3.
- hqc-proto: тела `Artifact` и `ArtifactUrl` (`api.artifact.json`, `api.artifact.url.json`).
- Хаб: хранилище на диске `HQC_ARTIFACT_DIR` по sha256, миграция `0006` (`artifacts`,
  `artifact_sessions`); `POST /api/artifacts` (multipart `file`, `kind?`; 100 МБ, `413
  too_large`; дедупликация `(owner, sha256, name)`), `GET /api/artifacts/{id}`, `GET
  /api/artifacts/{id}/url` (HMAC, 10 минут); `GET /artifacts/{id}?exp=&sig=` (`403`/`410`,
  sandbox CSP, HTML inline только на хосте `HQC_ARTIFACT_PUBLIC_BASE`, этот хост отдаёт
  только артефакты, его origin — в CSP клиента); `GET /worker/artifacts/{id}` токеном
  воркера для сессий этого воркера; ввод проверяет `artifact_ids` (`400 unknown_artifact`)
  и связывает их с сессией.
- Воркер: `.hqc/inbox/<event_id>/` в рабочем каталоге (санация и уникальность имён, без
  символических ссылок на пути, временный файл и `rename`), `.hqc/` в `info/exclude`, блок
  «Вложения:» в тексте, image content block в stream-json Claude Code **[проверить]**;
  фикстура `crates/hqc-worker/fixtures/inbox`.
- Клиент: скрепка, перетаскивание и вставка файлов с превью картинок; загрузка перед
  вводом, повтор без повторной загрузки; рендер вложений (image inline, diff, html — iframe
  только с домена артефактов, text/file — карточка); ссылки перезапрашиваются на
  `403`/`410`; пути вида `src/main.rs:42` в inline-коде и относительные ссылки копируются
  в буфер («путь скопирован», t11 #1).
- `Dockerfile` (`/var/lib/hqc/artifacts`), RUNBOOK §1.2, §1.6, §1.8, `tools/dev.sh`,
  `tools/e2e-local.sh`.
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (хаб: `tests/artifacts.rs` — загрузка и виды,
лимит, дедупликация, подпись и срок ссылки, чужой артефакт, заголовки и домен артефактов,
скачивание воркером; воркер: `inbox_scenario`, `inbox_refusals`,
`image_attachments_are_image_blocks`, модульные тесты `inbox`), `cd client && npm test &&
npm run build` (`tests/attachments.test.tsx`). Вопросы — `docs/notes/t16-questions.md`.
