# Итерация 1 — задачи

Цель (SPEC §19.1): начать сессию Claude Code из веба, вести её с телефона, подтвердить
запрос на разрешение. Упрощения итерации: один проект (личный), один workspace из конфига
воркера, без MCP hqc, без зеркала.

Каждая задача самодостаточна: её можно отдать отдельному агенту вместе с `CLAUDE.md`
и указанными разделами `docs/SPEC.md`. Зависимости указаны; задачи без общих зависимостей
можно вести параллельно.

## T0. Спайки по пометкам [проверить] (блокирует T4)

Разделы: §8.1, §9.3. Результат: фикстуры в `crates/hqc-claude-jsonl/fixtures/` и заметка
`docs/notes/claude-control-protocol.md` с точными JSON-формами. Прогоны `claude -p` только
с `--model claude-haiku-5-5` и `--max-turns` не больше 2 (прогоны с MCP требуют двух ходов:
первый уходит на ToolSearch).

1. `control_request` / `control_response` для `can_use_tool` при `--permission-prompts host`
   и `--input-format stream-json`: поля запроса (`tool_name`, `input`,
   `permission_suggestions`), форма ответа `allow`/`deny`, `updatedPermissions`.
   Проверить, что `always` через `updatedPermissions` пишет правило
   в `.claude/settings.local.json`.
2. Эхо `--replay-user-messages`: несёт ли оно `uuid` записи промпта из jsonl.
3. `AskUserQuestion` при `--permission-prompts host`: приходит ли как `can_use_tool`,
   принимает ли ответы через `updatedInput`.
4. `MCP_TOOL_TIMEOUT`: значение по умолчанию и единицы.
Приёмка: фикстуры лежат в репо, пометки в §8.1/§9.3 заменены на найденное, версия CLI
записана.

## T1. hqc-proto (блокирует T2, T3, T4)

Разделы: §3, §4, §5.3, §5.4. Скелет уже есть. Довести: все фреймы §5.3/§5.4 итерации 1
(`session.start`, `session.input`, `session.stop`, `session.kill`, `approval.response`,
`clarify.response`, `hello`/`hello.ack`, `event`/`ack`, `session.state`, `error`;
`approval.cancel` — событие §4 внутри `event`, не фрейм),
типы `Author`, `Delivery`, `SessionState`; тесты сериализации на каждый фрейм с
JSON-примером из спеки. Приёмка: `cargo test -p hqc-proto`, JSON-имена совпадают со спекой.

## T2. Хаб: ядро (зависит от T1)

Разделы: §2, §3, §5.1, §5.2, §6, §10.1, §16 (push), §17, §20.
- Postgres, миграции `sqlx`: users, workers, projects, sessions, events (уникальный
  индекс по `event_id`, `seq` на сессию), pending_requests, read_cursors,
  push_subscriptions, enroll_codes.
- Хранение событий: `payload` пишется в `jsonb` ровно в том виде, в каком пришёл от
  воркера или клиента, и клиентам отдаётся тот же сырой JSON. Типы `hqc-proto` — только
  для логики хаба; из них обратно в хранилище ничего не сериализуется, иначе значения
  перечислений от более нового воркера превратятся в `unknown` (§4, §5.2).
- Проверка JWT `Cf-Access-Jwt-Assertion` по JWKS; dev-режим `HQC_DEV_USER=<email>`
  без Access для локальной разработки, явно только при `HQC_DEV=1`.
- Маршрут `/worker/ws`: токен воркера, `hello`/`hello.ack` с версией, `event` → `ack`
  с `seq`, дедуп по `event_id`, досылка `pending` вводов после `hello`, политика
  `failed` после 3 неудачных стартов (§5.2).
- Маршрут `/api/ws`: `subscribe {session_id, after_seq}`, `subscribe_summary`,
  `agent.delta` без `seq`, `client_op_id`.
- REST из §6, подмножество итерации 1: me, projects (личный), sessions, events с
  пагинацией, input, stop, retry, requests, read_cursor, workers enroll/confirm/delete,
  push/subscribe.
- Вычисление `state` и `queue_len` (§17.1), `approval.cancel` при stop/перезапуске.
- Web Push (VAPID) на `approval.request` и финальный `agent.message` без активной подписки.
Приёмка: интеграционный тест с фейковым воркером по WebSocket: hello → session.start →
событие → ack → клиент видит событие; повторная отправка того же `event_id` не создаёт
дубль; при обрыве `pending` ввод досылается.

## T3. Воркер: ядро (зависит от T1)

Разделы: §2, §5.1, §5.2, §5.3, §5.4, §14 (только чтение конфига), §17.3.
- `hqc-worker enroll <hub>`: код, подтверждение в вебе, токен в secure store
  (Keychain на macOS; на других ОС пока файл с 0600 и пометка в спеке).
- Локальный конфиг `~/.hqc/worker.toml`: `allowed_roots`, один workspace для итерации 1,
  `idle_timeout`.
- WebSocket-клиент с heartbeat 20 с, реконнект с экспоненциальной задержкой, `hello`.
- Outbox на диске (SQLite или append-лог с индексом), `ack` по `event_id`, повтор после
  реконнекта.
- Супервизор процессов агентов: `session.start` → адаптер (интерфейс из §8), `session.input`
  с `ack` только после подтверждения от адаптера, `session.stop`, `session.kill`,
  `session.state`.
- `error {retryable}` при неудачном старте процесса.
Приёмка: тест с фейковым хабом: воркер регистрируется, переживает обрыв, досылает outbox,
не теряет ввод при собственном перезапуске (ввод приходит заново от хаба).

## T4. Адаптер Claude Code (зависит от T0, T1; интегрируется в T3)

Разделы: §8.1, §4, §10.1.
- Запуск `claude -p` с флагами из §8.1 (включая `--permission-prompt-tool stdio`),
  `--session-id` от хаба, `--resume` для продолжения.
- `--allowedTools mcp__hqc` при запуске, чтобы инструменты MCP-сервера hqc не шли через
  `can_use_tool` (§7, §8.1); в итерации 1 сервера hqc ещё нет, флаг закладывается сразу.
- Маппинг stream-json → события §4, правило `final` по следующему элементу потока,
  `event_id` = uuid v5 от `uuid` сообщения, `in_reply_to`.
- `can_use_tool` → `approval.request` с `options` (once/session/always/deny), ответ
  `control_response`; `session` — правило в адаптере; `always` — `updatedPermissions`
  только из `addRules` с `destination: localSettings`, остальные подсказки отбрасываются.
- Эхо собственных `control_response` в stdout пропускается; `ack` на `session.input` —
  по `user` с `isReplay: true`.
- `AskUserQuestion` → `clarify.request`, ответ — `updatedInput.answers` (T0 подтвердил).
  Фрейм `clarify.response` с `answers: {}` — отмена хабом: адаптер отвечает на
  `can_use_tool` для `AskUserQuestion` поведением `deny`, модель продолжает ход без
  ответов (§5.3, §10.1).
- `usage` по `result`, `status idle`.
- Ввод во время хода не отправляется в процесс: очередь ведёт супервизор (§17.2).
Приёмка: тесты на фикстурах stream-json без запуска `claude`; один живой тест за
`HQC_LIVE_CLAUDE=1`: ход с вызовом `Bash`, разрешение подтверждено адаптером, события
в правильном порядке, `event_id` совпадают с uuid из jsonl.

## T5. Веб-клиент (зависит от T2 для интеграции; UI можно начать по §4/§6 на моках)

Разделы: §6, §4, §10.1, §16, §19.1.
- TypeScript, PWA, адаптивная вёрстка под телефон. Фреймворк на выбор исполнителя,
  обосновать в `client/README.md` одним абзацем.
- Экран: список сессий личного проекта со `state` и непрочитанным; чат сессии с
  потоком `agent.delta`, карточками `tool.call`/`tool.result`, кнопками
  `approval.request` по `options[]`, статусом; ввод текста; кнопки stop и retry.
- Подписка `/api/ws` с `after_seq`, реконнект, `client_op_id` для оптимистичного вывода.
- Web Push: подписка, показ уведомления, переход в сессию.
- Регистрация воркера: экран подтверждения кода.
Приёмка: ручной сценарий с телефона по Cloudflare Access: открыть сессию, написать,
получить push на approval, подтвердить, увидеть ответ.

## T6. Сборка и приёмка итерации (зависит от всех)

- `docs/RUNBOOK.md`: как поднять хаб в DMZ (systemd/launchd, Postgres, переменные,
  Cloudflare Tunnel и Access, VAPID-ключи), как поставить воркер на home-mac.
- Сквозной прогон критерия §19.1 с телефона, запись результата и найденных расхождений
  со спекой в §23.

## Порядок

T0 и T1 — первыми, параллельно. Затем T2, T3, T5 параллельно; T4 после T0 и T1,
встраивается в T3. T6 — последним.
