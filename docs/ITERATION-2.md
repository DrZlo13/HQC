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

## T15. Импорт, зеркало и подхват сессий Claude Code (сделано, SPEC 0.17)

Разделы: §3 (Session, Workspace, Event), §4, §5.2–§5.4, §8.1 (идентификаторы, resume), §9,
§17.1. Зависит от T14. Критерий итерации: подхватить с телефона сессию, начатую в VS Code, и
вернуть обратно.
- hqc-proto (только добавления): hub → worker `workspace.list_candidates {request_id}` →
  `result {candidates[{path, sessions_count, last_active}]}` и `mirror.set {workspace_path,
  enabled}`; worker → hub `mirror.event {workspace_path, runtime_session_id, cwd, event,
  title?, recent}`; REST `CandidateList`, `CreateWorkspaceImport` (`mirror_enabled`);
  `Event::id_from_claude_block`; примеры в `examples/wire`.
- hqc-claude-jsonl: `Message::is_main` (не sidechain, не `isMeta`, не сводка `/compact`),
  `stop_reason`, `blocks`, `first_cwd`; фикстуры 2.1.308 `vscode-takeover` (сессия `claude -p`,
  подхват адаптером через `--resume` и ещё ход) и `adapter-then-outside` (сессия адаптера и
  `claude -p --resume` снаружи), снятые `tools/spike/driver.py --arg-prompt / --resume`.
- Воркер: зеркало — опрос `<config>/projects/*/<uuid>.jsonl` раз в 2 с для папок из
  `mirror.set` (только внутри `allowed_roots`), смещения и кэш «файл → cwd» в
  `$HQC_HOME/mirror/mirror.sqlite3`, первый проход — импорт истории; маппинг записей на §4;
  реестр своих uuid (`OwnRecords`), который заполняет адаптер из stream-json; чужая запись
  при живом процессе — через 5 с остановка агента после хода, `session.state locked_by:
  vscode`, ввод отклоняется `locked_vscode` до следующего `session.start`; кандидаты по `cwd`;
  `mirror.event` через outbox.
- Хаб: сессия на неизвестный транскрипт в проекте workspace (`cwd` из записи, `ai-title`,
  `locked_by` по `recent`), авторы от хаба, без push; `POST /api/sessions/{id}/takeover`
  (`409 vscode_active` 30 с, `session.start mode: resume`, `system session_taken_over`);
  `409 locked_vscode` с `hint: takeover`; освобождение (`system session_released`, `pending`
  вводы `failed`); `mirror.set` при импорте и после `hello`; `GET
  /api/workers/{id}/candidates`; миграция `0006` (`vscode_active_at`).
- Клиент: на экране Workspaces «Импорт из Claude Code» (кандидаты воркера, выбранные папки
  становятся workspace с зеркалом); в чате `locked_vscode` — «ведётся в VS Code», ввод закрыт,
  кнопка «Подхватить».
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (парсер на фикстурах T15; зеркало: свои записи не чужие на фикстуре
а, чужой писатель на фикстуре б, импорт, недописанные строки, кандидаты; воркер с фейковым
хабом: освобождение после хода и `locked_vscode`; хаб с фейковым воркером: импорт, `409
locked_vscode`, `409 vscode_active`, takeover, освобождение, `mirror.set` после `hello`), `cd
client && npm test && npm run build`; живой `HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test
live_takeover` («VS Code»-сессия → импорт → takeover → ход → освобождение). Вопросы —
`docs/notes/t15-questions.md`.
