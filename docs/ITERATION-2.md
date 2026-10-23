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

## T17. Служебная сессия воркера и несессионные подтверждения (сделано, SPEC 0.17)

Разделы: §3 (Worker, Session, PendingRequest), §4, §5.3, §5.4, §10.2, §13.1, §13.2, §16, §17.4.
- hqc-proto (только добавлением): кадры worker → hub `worker.approval.request {request_id,
  kind, description, expires_at}` и hub → worker `worker.approval.response {request_id,
  option_id}` с примерами в `examples/wire`; необязательные `hello.ack.system_session_id` и
  `session.start.requested_by_owner`; `Runtime::None`; `Session.system`.
- Хаб: у каждого воркера служебная сессия (`workers.system_session_id`; при регистрации,
  миграцией `0006` для существующих, на `hello`, если нет) в личном проекте владельца, без
  workspace, `runtime: none`; ввод, стоп, повтор, архивация, удаление — `409 system_session`.
  `worker.approval.request` → событие `approval.request` с `once`/`deny` и `PendingRequest`
  `worker_approval`: отвечает только владелец воркера, ответ уходит кадром
  `worker.approval.response` (и после переподключения), истечение — `approval.cancel
  timeout` и `deny`, новый `boot_id` — `worker_restart`; push как на обычный approval.
  Строки `worker_log` не считаются непрочитанными.
- Воркер: слой `tracing` пересылает warn/error в служебную сессию (`system worker_log`,
  `level`) через outbox, одинаковый текст — не чаще раза в секунду, всего ≤ 60 в минуту,
  остальное — строкой «пропущено N». `session.start` чужой сессии (`owner_is_worker_owner:
  false`, без `requested_by_owner`) в папке, которой нет в `$HQC_HOME/known_workspaces.json`,
  ждёт подтверждения владельца (`workspace.use`): `once` запоминает папку, `deny` или
  таймаут — `error {invalid_frame}`. Закрывает A2 аудита частично.
- Клиент: группа «Воркеры» в левой колонке, лента служебной сессии без поля ввода, карточки
  подтверждений, строки `worker_log` со временем и уровнем, фильтр «только запросы»; мок
  хаба со служебной сессией.
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (хаб: `tests/system_session.rs` — создание служебной
сессии и миграция, запрет ввода, `worker.approval` туда и обратно, только владелец,
истечение и перезапуск, досылка ответа после `hello`, push; воркер:
`tests/system_session.rs` — подтверждение новой папки для чужой сессии, отказ, без
подтверждения для владельца, повтор запроса после переподключения, логи с ограничением
частоты; юнит-тесты `worker_log` и `known_paths`), `cd client && npm test && npm run
build` (рендер `worker_log` и карточки, фильтр, сценарий через мок). Вопросы —
`docs/notes/t17-questions.md`.
