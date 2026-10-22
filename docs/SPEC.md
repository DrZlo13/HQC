# hqc — техническое задание

Версия 0.8, 2026-10-22. Статус: согласованная архитектура после шести ревью и обзора
аналогов (§22), до начала реализации. Изменения — в §23, старые версии —
в `SPEC-CHANGELOG.md`. Пометки: **[решение]** — принято, требует подтверждения;
**[проверить]** — требует проверки на реальной сборке.

Термины: **ход** — один цикл «ввод → работа агента → финальный ответ», начинается
событием `user.message` или `peer.message` с `mode=ask` и заканчивается
`agent.message` с `final: true`. **Голая модель** — LLM без доступа к файловой
системе. **Подхват** — продолжение (resume) сессии в другом месте: с VS Code на hqc
или обратно. **Hermes** — Hermes Agent от Nous Research; на llm-server работает как
демон (messaging-гейтвей) и дополнительно умеет запускаться отдельным процессом как
ACP-сервер (`hermes acp`). **ACP** — Agent Client Protocol (Zed), JSON-RPC по stdio
между клиентом и агентом: `initialize`, `session/new`, `session/prompt`,
`session/update`, `session/request_permission`, `session/cancel`, `session/load`.
**Владелец воркера** — пользователь, зарегистрировавший машину. **Владелец
сессии** — пользователь, создавший сессию.

## 1. Цель и границы

hqc — единый интерфейс для ведения проектов с LLM-агентами с любого устройства.
Агенты физически работают на разных машинах и разных ОС, пользователь общается с ними
через веб-клиент, похожий на Discord: проекты, внутри проектов сессии, в сессии агент.

Что hqc делает:

- хранит проекты, сессии и полную историю событий в одном месте;
- запускает и ведёт агентов на той машине, где лежит код и подключено железо;
- показывает сессии, начатые в VS Code, и позволяет подхватить их с телефона;
- даёт агентам инструменты для обмена сообщениями, артефактами и запросами друг к другу;
- пускает коллег как отдельных пользователей с явным шарингом.

Что hqc не делает:

- не реализует агентный цикл для Claude Code и ACP-агентов: цикл живёт в рантайме.
  Для голой модели хаб крутит минимальный цикл «модель ↔ MCP-инструменты hqc» (§8.3);
- не дублирует возможности Hermes (крон задач агента, kanban, память, Telegram);
- не является IDE: нет редактора файлов, диффы только как артефакт;
- не даёт жёсткой изоляции между сессиями на одной машине без контейнера или
  отдельного пользователя ОС (§14).

## 2. Топология

**Hub** — один всегда доступный сервис. Хранит состояние, маршрутизует события, отдаёт
API клиентам и воркерам. Исполняет только голые модели (§8.3).

**Worker** — демон на каждой машине, где должны работать агенты. Подключается к хабу
только наружу по WebSocket. Внутри: нативный адаптер Claude Code, общий ACP-адаптер
для Hermes и любых агентов из ACP-реестра, MCP-сервер hqc, зеркало сессий VS Code,
операции с workspace, локальный конфиг с ограничениями и окружениями.

**Client** — веб-приложение, позже Tauri и мобильное. Общается только с хабом.

Размещение: хаб в домашней DMZ за Cloudflare Tunnel. Люди входят через Cloudflare
Access (SSO), воркеры — через отдельный маршрут с собственными токенами (§6, §20).

### 2.1 Модель угроз

- Хаб — корень доверия для переписки и разрешений: кто контролирует хаб, может ответить
  `allow` на любой `approval.request` и писать в любые сессии.
- Хаб **не** может: выйти за `allowed_roots` воркера, выбрать окружение или профиль
  Hermes для пути, которые владелец воркера не привязал локально, включить
  MCP-серверы с машины. Это задаётся только локальным конфигом воркера (§14); воркер
  игнорирует расходящиеся значения из `session.start` и отвечает ошибкой.
- Обещание про локальные MCP держится для Claude Code (`--strict-mcp-config`)
  и Hermes (`HERMES_ACP_SKIP_CONFIGURED_MCP=1`, воркер ставит всегда). Для других
  ACP-агентов общего способа нет: в локальном конфиге каждый `acp_agent` объявляет
  `isolation: verified | none`; агент с `none` не допускается в чужие сессии
  (§11.1) и обещание на него не распространяется.
- Воркер — удалённое выполнение кода на машине владельца. Любой, кто может писать
  в сессию на этом воркере, выполняет код от имени владельца в пределах его политики.
- Между сессиями одного воркера граница мягкая (§14): процессы работают от одного
  пользователя ОС и могут читать окружение и файлы друг друга.
- Коллега с доступом к расшаренному проекту ограничен грантами (§11) и подтверждениями
  владельца воркера (§10).

## 3. Модель данных

Все сущности имеют `id` (uuid v7), `created_at`, `updated_at`. `owner_id` есть у Worker,
Project, Workspace, Session, Artifact, Grant, ScheduledMessage. У Event владельца нет,
есть `author`.

### User
`email` (из проверенного JWT Cloudflare Access), `display_name`, `api_keys`
(OpenRouter, Anthropic API; шифруются мастер-ключом хаба, который лежит в env/файле
хаба, не в базе), `timezone`, `personal_project_id` (неявный личный проект, создаётся
при первом входе; в нём живёт персональная сессия-ассистент).

### Worker
`owner_id`, `name`, `os`, `token_hash`, `status` (online/offline), `last_seen`,
`protocol_version`, `capabilities` (`claude_code: {version}`, `acp_agents[{name,
version, load_session, resume, image, isolation}]` — результат `initialize` каждого
агента при старте воркера), `local_models[]`, `share_local_models`, `allowed_roots[]`,
`environments[]` (имена), `env_bindings[]` (путь → окружение, профиль Hermes),
`allow_foreign_sessions`, `idle_timeout`, `system_session_id` (§10.2).

Поля `allowed_roots`, `environments`, `env_bindings`, `allow_foreign_sessions`,
`share_local_models`, `acp_agents` — **источник истины в локальном конфиге воркера**;
хаб хранит копию из `hello` для отображения и ранней валидации.

### Project
`owner_id`, `name`, `slug`, `description`, `default_workspace_id` (nullable),
`session_template` (§13.3), `is_personal`.

### Workspace
`project_id`, `worker_id`, `path`, `label`, `environment` (имя; отображение привязки
из локального конфига воркера), `ephemeral`, `created_by_hqc`, `kind`
(dir/clone/worktree), `origin_workspace_id`, `mirror_enabled`.
Workspace — **логическая** граница: все операции hqc (cwd сессий, inbox, зеркало,
удаление) ограничены его путём. Принудительной изоляции файловой системы он не даёт.

### Session
`project_id`, `owner_id`, `workspace_id` (nullable), `worker_id` (обязателен
для Claude Code без workspace; для голой модели null; ACP-агенты без workspace
не допускаются, §8.4), `cwd` (относительный путь внутри workspace, по умолчанию `.`),
`title`, `slug` (уникален в проекте, не меняется при переименовании), `agent`,
`auto_approve`, `wake_on_send` (по умолчанию false), `locked_by` (none/hqc/vscode),
`last_seq`, `archived`. `state` и `queue_len` (§17.1) **не хранятся**, вычисляются
хабом при отдаче. В первой версии в сессии ровно один агент; доступ людей к сессии
определяется только Grant.

### Agent (встроен в Session)
`runtime` (claude_code / acp / bare), `acp_agent` (имя агента из локального конфига
воркера для `acp`), `model`, `runtime_session_id` (строка: uuid jsonl у Claude Code,
произвольный id у ACP-агентов), `process_state`
(stopped/starting/running/stopping), `system_prompt`, `disallowed_tools[]`,
`restricted` (§8.4), `start_failures` (счётчик, §5.2). Смена рантайма между ходами
означает **новую сессию рантайма с пустым контекстом**: `runtime_session_id`
сбрасывается, история hqc остаётся, пользователю предлагается пересказ
последних N сообщений стартовым промптом.

### Event
`session_id`, `event_id` (uuid; для событий, порождённых записями jsonl Claude Code —
и адаптером, и зеркалом, и импортом — это uuid v5 от `uuid` записи, поэтому один
и тот же факт из двух источников даёт один `event_id`; для остальных — v7 от
отправителя), `seq` (назначает хаб, монотонный в сессии), `ts`, `author`
(`{kind: user|agent|system, id}`; для агента `id` = его `session_id`), `type`,
`payload`, `chain` (`{chain_id, depth, hops}` для цепочек агент→агент, иначе null),
`client_op_id` (эхо для клиента), `delivery` (для `user.message` и `peer.message`:
`pending | delivered | failed`, `delivered_at`).

### ReadCursor
`user_id`, `session_id`, `last_read_seq`. Источник «непрочитанного».

### Artifact
`owner_id`, `session_id`, `kind` (file/image/diff/html/text), `mime`, `size`,
`sha256`, `name`, `storage_key`. События ссылаются только на `artifact_id`;
ссылки на скачивание выдаются при рендере (§12).

### Grant
`owner_id` (кто выдал), `resource_type` (project/session), `resource_id`, `user_id`,
`level` (view/write/manage).

### PendingRequest
Открытые запросы, переживающие перезапуск хаба: `kind`
(approval/clarify/ask/worker_approval/mcp_call), `session_id`, `request_id`,
`payload`, `expires_at`, `from_session_id`, `from_event_id`, `worker_id`
(для ask и mcp_call: кому отдать результат), `result` (заполняется, когда ответ
получен, но ещё не доставлен воркеру).

### ScheduledMessage
`owner_id`, `session_id` или (`project_id`, `workspace_id`, шаблон), `cron` или `at`,
`timezone`, `message`, `enabled`, `last_run`, `catch_up`.

## 4. Схема событий

Единая схема для всех рантаймов. Адаптер переводит поток рантайма в эти события,
клиент рендерит только их. Неизвестные поля в `payload` допустимы. Колонка «источник» —
компонент, который производит событие; `author` задаётся по §3.

| type | payload | источник |
|---|---|---|
| `user.message` | `text`, `artifact_ids[]` | клиент |
| `agent.delta` | `text` | адаптер; только транспорт, в базу не пишется |
| `agent.message` | `text`, `final`, `in_reply_to` (event_id ввода, начавшего ход) | адаптер. `final: true` — конец хода; `final: false` — текст между вызовами инструментов |
| `agent.thinking` | `text` | адаптер, если рантайм отдаёт |
| `tool.call` | `call_id`, `tool`, `args_preview` | адаптер |
| `tool.result` | `call_id`, `result_preview`, `error`, `duration_ms`, `artifact_id?` (дифф или большой результат, §12) | адаптер |
| `approval.request` | `request_id`, `tool`, `description`, `options[{id, label, canonical}]`, `expires_at` | адаптер; `options` — **реальные варианты агента**, `canonical` ∈ once/session/always/deny/deny_always для кнопок клиента |
| `approval.response` | `request_id`, `option_id`, `by` (user id или `auto`) | клиент или хаб при `auto_approve` |
| `approval.cancel` | `request_id`, `reason` (timeout/stopped/answered_elsewhere/worker_restart/archived) | адаптер, воркер или хаб |
| `clarify.request` / `clarify.response` / `clarify.cancel` | `request_id`, `questions[]` / `answers` | адаптер Claude Code: инструмент `AskUserQuestion` приходит как `can_use_tool`, ответ уходит в `updatedInput.answers` (проверено на 2.1.307 при `--permission-prompt-tool stdio`, §8.1). У ACP-агентов источника нет |
| `attachment` | `artifact_id`, `kind`, `name`, `caption` | клиент или агент через `hqc_attach` |
| `peer.message` | `from_session_id`, `text`, `mode` (send/ask), `request_id` (для ask) | хаб по `hqc_send`/`hqc_ask` |
| `usage` | `input_tokens`, `output_tokens`, `cache_read`, `cache_write`, `cost_usd?`, `model` | адаптер Claude Code по `result`; у ACP-агентов необязательно и без `cost_usd` (стандартного usage в ACP v1 нет) |
| `status` | `state` (§17.1), `detail` | воркер или хаб |
| `system` | `text`, `code` | хаб: сессия создана, подхвачена, workspace удалён, ввод не доставлен и т.п. |

Правила:

- `artifact_ids[]` в `user.message` — вложения к сообщению; отдельное событие
  `attachment` — для файлов без текста.
- `*_preview` обрезаются воркером до 4 КБ; полный результат инструмента в хаб
  не передаётся, кроме диффов и результатов, сохранённых артефактом.
- `agent.delta` идёт клиентам по WebSocket и не сохраняется; сохраняется `agent.message`.
- История не переписывается. Защита от подделки заголовка peer-сообщений делается
  при доставке в рантайм (§7.1).

## 5. Протокол Worker ↔ Hub

Транспорт: одно постоянное WebSocket-соединение от воркера к хабу, JSON-фреймы,
heartbeat 20 с, переподключение с экспоненциальной задержкой.

Каждый фрейм: `{id, type, payload}`. Ответ на фрейм-запрос:
`{id, type: "ack"|"result"|"error", payload}` с тем же `id`. Ошибка:
`{code, message, retryable}`.

### 5.1 Регистрация и версия
`hqc-worker enroll <hub-url>` печатает код, пользователь подтверждает его в вебе под
своим логином, воркер получает долгоживущий токен (хранится в локальном secure store:
Keychain, libsecret, DPAPI). Токен отзывается из UI. В `hello` воркер шлёт
`protocol_version`; хаб отвечает `hello.ack` или `error {min_version}`. При
несовместимости воркер пишет в лог и в трей «нужно обновление», повторяет попытку
раз в час и не делает ничего другого; обновление — руками или `hqc-worker update`.

### 5.2 Гарантии доставки
Worker → hub:
- Воркер ведёт **outbox** на диске: событие сначала пишется в outbox, потом отправляется.
  Через outbox идут `event`, `mirror.event`, `mcp.call`.
- Хаб отвечает `ack {event_id, seq}`; до ack воркер повторяет после переподключения.
- Дубли отбрасываются по `event_id` (уникальный индекс). События для сессий, которых
  хаб не знает (удалены), получают `ack` и отбрасываются.
- `seq` назначает хаб; клиенты видят только события с `seq`.
- `agent.delta` в outbox не пишется и при обрыве теряется, это допустимо.

Hub → worker:
- `user.message` и `peer.message` хаб хранит с `delivery: pending` и шлёт как
  `session.input`. Воркер отвечает `ack` после передачи ввода в рантайм (Claude Code —
  после эха `--replay-user-messages`; ACP — после отправки `session/prompt`). После
  `hello` хаб досылает все `pending` вводы сессий этого воркера в порядке `seq`.
- Если процесс агента не стартует (нет бинарника, профиль не настроен), воркер
  отвечает `error {retryable: true}`; после **3 неудачных стартов** подряд хаб ставит
  вводу `delivery: failed`, пишет `system` с причиной, сессия переходит в `error`;
  кнопка «повторить» сбрасывает счётчик и возвращает ввод в `pending`.
- Очередь ввода воркера (§17.2) в памяти; источник истины — `pending` события
  в хабе, перезапуск воркера их не теряет.
- Смещения зеркала по каждому файлу jsonl и кэш «файл → cwd» воркер хранит на диске.

MCP-вызовы (worker → hub → worker):
- `mcp.call {request_id, session_token, tool, args}` → `mcp.result {request_id,
  result | error}`. Быстрые инструменты (`attach`, `list`, `search`, `schedule`, `send`,
  `spawn`) отвечаются сразу. `ask` — отложенный: хаб создаёт `PendingRequest`
  с `worker_id` и отвечает `mcp.result` только когда пришёл ответ или истёк таймаут.
  Если в этот момент воркер офлайн, результат ждёт в `PendingRequest.result`
  и досылается после `hello`; воркер, у которого блокирующего вызова уже нет
  (перезапустился), отвечает `ack` и отбрасывает.
- `mcp.cancel {request_id, reason}` — воркер сообщает, что отложенный вызов умер на его
  стороне (§7.2). Получив его, хаб закрывает `PendingRequest` с ошибкой и пишет в
  сессию-отправитель событие `system` с кодом `ask_cancelled`; целевой сессии ничего
  не шлёт, её ход идёт дальше.

### 5.3 Фреймы hub → worker
- `session.start` {session_id, runtime, acp_agent?, model, workspace_path | null, cwd,
  environment, runtime_session_id?, system_prompt?, disallowed_tools[], restricted,
  auto_approve, session_token, owner_is_worker_owner, ask_timeout_s}
  — `owner_is_worker_owner` нужен воркеру для `allow_foreign_sessions`, выбора
  профиля Hermes и допуска `isolation: none`; `ask_timeout_s` — чтобы выставить
  таймаут MCP-инструмента рантайма (§7.1).
- `session.input` {session_id, event_id, text, artifact_ids[], peer_prefix[]} —
  артефакты воркер скачивает через `GET /worker/artifacts/{id}` своим токеном
  (без TTL) в `<workspace>/<cwd>/.hqc/inbox/<event_id>/` и подставляет пути; папка
  `.hqc/` добавляется в `.git/info/exclude` workspace. `peer_prefix` — накопленные
  `send`-сообщения (§7.1).
- `session.stop` {session_id} — прервать текущий ход.
- `session.steer` {session_id, text} — вмешаться в ход без прерывания, если рантайм
  умеет (Hermes: `/steer`, §8.2.1); иначе `error`.
- `session.kill` {session_id} — завершить процесс агента.
- `approval.response`, `clarify.response` {session_id, request_id, ...}
- `workspace.create` {request_id, kind: clone|worktree|dir, path, origin_url?,
  origin_path?, branch?, requested_by_owner} → `result {ok, path, error?}`
- `workspace.delete` {request_id, path, requested_by_owner} → `result` (§13.2)
- `workspace.list_candidates` {} → `result {candidates[{path, sessions_count, last_active}]}`
- `mirror.set` {workspace_path, enabled} → `ack`
- `model.proxy` {request_id, model, body} → поток `model.proxy.chunk {request_id, data}`
  и `model.proxy.end {request_id, error?}`
- `worker.approval.response` {request_id, option_id}
- `mcp.result` {request_id, result | error}

### 5.4 Фреймы worker → hub
- `hello` {protocol_version, capabilities, environments[], env_bindings[],
  allowed_roots[], allow_foreign_sessions, share_local_models, local_models[]}
- `event` {event} → `ack` (`session_id` внутри события)
- `session.state` {session_id, process_state, runtime_session_id, locked_by}
- `mirror.event` {workspace_path, runtime_session_id, cwd, event} — события из
  зеркала (§9.2); хаб создаёт сессию для неизвестного `runtime_session_id`.
- `worker.approval.request` {request_id, kind, description, expires_at}
- `mcp.call` {request_id, session_token, tool, args}
- `mcp.cancel` {request_id, reason} — отмена отложенного вызова (§5.2, §7.2)
- `result` / `error` на запросы хаба.
- Загрузка артефактов: HTTP PUT `/worker/artifacts` с токеном воркера, не по WebSocket.
  Лимит артефакта **100 МБ**, нарезки на части нет.

## 6. Протокол Client ↔ Hub

Авторизация: хаб проверяет подпись JWT `Cf-Access-Jwt-Assertion` по JWKS Cloudflare
(issuer и audience из конфига) и берёт email из токена. Заголовок с email без JWT
не принимается. Маршрут воркеров `/worker/*` исключён из Access и принимает только
токены воркеров; маршрут людей `/api/*` принимает только JWT Access. Хаб слушает только
интерфейс туннеля.

HTTP API итерации 1 (список не полный; остальное помечено «позже»):

- `GET /api/me`; позже `PATCH /api/me` (ключи, часовой пояс)
- `GET/POST /api/projects`, `GET/PATCH/DELETE /api/projects/{id}`
- `GET/POST /api/projects/{id}/sessions`, `GET/PATCH/DELETE /api/sessions/{id}` (§17.4)
- `GET /api/sessions/{id}/events?after_seq=&limit=` — пагинация по `seq`
- `POST /api/sessions/{id}/input` {text, artifact_ids[], client_op_id}
- `POST /api/sessions/{id}/stop`, `/steer`, `/takeover` (§9.3), `/archive`,
  `/unarchive`, `/retry` (§5.2)
- `POST /api/sessions/{id}/requests/{request_id}` {option_id | answers}
- `POST /api/artifacts` (multipart) → `{id}`; `GET /api/artifacts/{id}/url` →
  подписанная ссылка (§12)
- `PUT /api/sessions/{id}/read_cursor` {last_read_seq}
- `GET /api/workers`, `POST /api/workers/enroll/confirm` {code},
  `DELETE /api/workers/{id}`, `GET /api/workers/{id}/candidates`
- `GET/POST /api/workspaces`, `PATCH /api/workspaces/{id}` (зеркало, метка),
  `DELETE /api/workspaces/{id}`
- `GET /api/search?q=&project=&fuzzy=`
- `GET/POST/DELETE /api/grants`
- `POST /api/push/subscribe`
- позже: `/api/schedules` (ScheduledMessage), `/api/projects/{id}/template`

WebSocket `/api/ws`: клиент шлёт `subscribe {session_id, after_seq}` и получает
события с `seq > after_seq`, затем живой поток; `agent.delta` без `seq`. Событие,
сохранённое из ввода клиента, несёт его `client_op_id`, чтобы клиент показывал
сообщение сразу и сверял с эхом (приём из Agent Host Protocol, §22). Отдельная
подписка `subscribe_summary {}` даёт `state`, `queue_len` и непрочитанное по сессиям.
После переподключения клиент повторяет `subscribe` с последним виденным `seq`.

## 7. MCP-инструменты для агентов

Воркер поднимает MCP-сервер hqc и подключает его к рантайму каждой сессии.
Идентификация: MCP-сервер — stdio-процесс, который рантайм запускает сам (через
`--mcp-config` у Claude Code, JSON передаётся строкой, без файла; через поле
`mcpServers` в `session/new` у ACP-агентов); воркер передаёт ему `session_token`
из `session.start` через переменную окружения, а MCP-процесс ходит в демон воркера
по локальному сокету с этим токеном; демон пересылает вызов в хаб фреймом `mcp.call`
(§5.2). Токен выдаёт хаб, он не попадает к агенту в контекст и в транскрипт: Hermes
хранит `mcpServers` из `session/new` только в памяти работающего агента, в базу
сессий не пишет (проверено по исходникам). Ограничение: другой процесс того же
пользователя ОС может прочитать окружение процесса; это та же мягкая граница, что
в §14. Голая модель на хабе получает те же инструменты напрямую. Инструменты hqc
вызываются без подтверждения: Claude Code запускается с `--allowedTools mcp__hqc` (§8.1).

Инструменты действуют с правами владельца сессии; у агента своих прав нет.

- `hqc_attach(path?, content?, name?, kind?, caption?)` — загрузить файл как артефакт
  в текущую сессию; `path` для рантаймов с файловой системой, `content` для голой модели.
- `hqc_send(text, session?)` — сообщение в другую сессию без ожидания ответа; без
  `session` — в персональную сессию-ассистент владельца.
- `hqc_ask(text, session, timeout_s?)` — сообщение с ожиданием ответа. В свою же
  сессию — ошибка сразу.
- `hqc_spawn(project, workspace, runtime?, model?, prompt)` — создать сессию и агента,
  вернуть адрес. `workspace` обязателен, если у проекта нет `default_workspace_id`.
- `hqc_list_sessions(project?)` — адреса, названия, статусы.
- `hqc_search_history(query, project?, fuzzy?)` — поиск по истории с учётом грантов.
- `hqc_schedule(session, at|cron, message)` — крон хаба (§16).

Адрес сессии: `<project-slug>/<session-slug>` или id. Slug уникален в проекте и не
меняется при переименовании.

### 7.1 Семантика send и ask **[решение]**

Доставка:
- `peer.message` всегда доставляется агенту как **текст ввода** с заголовком
  `[hqc peer from <project>/<session> | mode=ask | request_id=... | reply with your final message]`
  или `... | mode=send]`. Так агент отличает ask от send в любом рантайме.
- Защита от подделки: при сборке `session.input` воркер экранирует подстроку
  `[hqc peer` (в любом месте текста, регистронезависимо) в `[hqc-peer-text`
  во **всём** тексте, который кладёт в рантайм, — в тексте пользователя, в тексте
  peer-сообщения от агента и в блоке `peer_prefix`; настоящий заголовок добавляется
  после экранирования. История в хабе не переписывается.
- Права: `hqc_send`/`hqc_ask` в чужую сессию требуют у владельца сессии-отправителя
  гранта `write` на целевую.

Очереди (две разные):
- **Очередь ввода** сессии (§17.2): `user.message` и `peer.message` с `mode=ask`.
  Каждый элемент запускает отдельный ход, строго по `seq`.
- **Буфер send**: `peer.message` с `mode=send` при `wake_on_send: false` хода не
  запускает; хаб копит такие сообщения и передаёт их в `peer_prefix` следующего
  `session.input`; воркер подклеивает их перед текстом ввода блоком «Сообщения от
  других сессий». Буфер очищается по `ack` на ввод, который его нёс. Лимит буфера —
  50 сообщений или 64 КБ; старшие вытесняются с событием `system`. При
  `wake_on_send: true` send идёт в очередь ввода как ask без ожидания ответа.

Ответ на ask:
- Ответом считается `agent.message` с `final: true` и `in_reply_to`, равным `event_id`
  этого `peer.message`. Возвращается как `mcp.result` блокирующему вызову.
- Целевая сессия `locked_vscode` — немедленная ошибка. `worker_offline` — ожидание
  до таймаута. Таймаут по умолчанию 10 минут (`ask_timeout_s` в `session.start`).
- Таймаут инструмента в рантайме воркер выставляет не меньше `ask_timeout_s` плюс
  минута: Claude Code — `MCP_TOOL_TIMEOUT` в env процесса, в миллисекундах; по умолчанию
  он практически не ограничен (1e8 мс), поэтому воркер всегда выставляет его явно как
  `(ask_timeout_s + 60) * 1000` (проверено на 2.1.307). Отдельно у stdio MCP-серверов Claude Code действует idle-таймаут
  30 минут (`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT`): вызов без ответа и без progress-уведомлений
  обрывается через 1800 с даже при большом `MCP_TOOL_TIMEOUT`, поэтому MCP-сервер hqc
  во время ожидания `hqc_ask` шлёт progress (§7.2; проверено на 2.1.307); Hermes — `mcp.tool_call`
  в конфиге профиля (по умолчанию 300 с, проверено по исходникам); другие
  ACP-агенты — по их документации **[проверить]**.
- Открытые `ask` хранятся в `PendingRequest`; после перезапуска воркера-отправителя
  блокирующий вызов потерян, `ask` закрывается с ошибкой (§5.2).

Цепочки:
- `chain_id` создаётся при первом `ask`/`send` из хода, начатого человеком; `depth`
  растёт на каждом `ask` (лимит 3), `hops` — на каждом `peer.message` в цепочке
  (лимит 20). Хаб хранит `chain` ввода, начавшего текущий ход, и отдаёт его
  MCP-серверу в ответе на первый `mcp.call` сессии, чтобы тот ставил его на исходящие.
- При превышении лимита инструмент возвращает ошибку; продолжить цепочку может только
  ход человека. Взаимная блокировка (A ждёт B, B ждёт A) разрешается таймаутом.

### 7.2 Требования к MCP-серверу hqc
- Stdio-процесс `hqc-worker mcp`, который запускает сам рантайм: Claude Code — через
  `--mcp-config` строкой JSON, ACP-агенты — через `mcpServers` в `session/new` (§7).
- `session_token` получает только из переменной окружения; не пишет его в ответы
  инструментов, логи рантайма и файлы (§7).
- Каждый вызов пересылает демону воркера по локальному сокету с этим токеном; демон
  отправляет его в хаб фреймом `mcp.call` (§5.2). Своего состояния, кроме `chain` текущего
  хода (§7.1), не держит.
- Имя сервера в конфиге рантайма — `hqc`, инструменты — `hqc_*`; у Claude Code они
  разрешены заранее через `--allowedTools mcp__hqc` (§8.1).
- Пока вызов ждёт ответа (`hqc_ask`), шлёт `notifications/progress` с `progressToken` из
  `params._meta` вызова не реже раза в минуту. Без этого Claude Code обрывает вызов через
  30 минут простоя (§7.1); серверу при этом `notifications/cancelled` не приходит.
- Если рантайм отменил вызов (`notifications/cancelled`, например по `MCP_TOOL_TIMEOUT`)
  или MCP-процесс завершился с открытым вызовом (EOF на stdio), демон воркера закрывает
  соответствующий `PendingRequest` ask фреймом `mcp.cancel {request_id, reason}` (§5.4);
  обрыв по простою без уведомления предотвращается progress-уведомлениями, так что
  отдельной детекции для него нет.

## 8. Адаптеры рантаймов

Два адаптера на воркере: нативный для Claude Code (§8.1) и общий ACP (§8.2) для Hermes
и любого агента из ACP-реестра. Третий, голая модель, живёт в хабе (§8.3). Все
реализуют один интерфейс: `start`, `input`, `stop`, `steer?`, `kill`, `resume`,
`answer_approval`, `answer_clarify` плюс поток событий §4. Очередь ввода —
ответственность воркера, не адаптера (§17.2).

### 8.1 Claude Code
Процесс `claude -p --output-format stream-json --input-format stream-json
--include-partial-messages --replay-user-messages --permission-prompts host
--permission-prompt-tool stdio --strict-mcp-config --mcp-config '<json>'
--allowedTools mcp__hqc --session-id <uuid>` для новой сессии
(uuid назначает хаб, кладёт в `runtime_session_id`) и `--resume <uuid>` для
продолжения. `--permission-prompt-tool stdio` обязателен: без него CLI ничего
не спрашивает у хоста, а сразу отклоняет всё, что требует подтверждения
(`system/permission_denied`, `permission_decision.source: config`), и `AskUserQuestion`
не попадает в список инструментов (проверено на 2.1.307). `--allowedTools mcp__hqc`
разрешает все инструменты MCP-сервера hqc, чтобы их вызовы не шли через `can_use_tool`
(§7); на 2.1.307 одинаково работают формы `mcp__hqc` и `mcp__hqc__*`, без флага
вызов MCP-инструмента приходит как `can_use_tool`. Проверено на 2.1.300: `--resume` продолжает тот же uuid и дописывает тот
же файл jsonl. `--strict-mcp-config` отключает MCP-серверы из `~/.claude.json`
и `.mcp.json` проекта; разрешить их можно только через окружение (§14,
`allow_local_mcp`). В env процесса — `MCP_TOOL_TIMEOUT` (§7.1): миллисекунды, по умолчанию
1e8 мс, минимум 1000 мс (проверено на 2.1.307).

Идентификаторы. Проверено на 2.1.307: `uuid` у сообщений `assistant` и `user`
(включая `tool_result`) в stream-json **совпадает** с `uuid` соответствующих записей
в jsonl. Поэтому адаптер выводит `event_id` как uuid v5 от этого `uuid`, как и зеркало
(§3 Event, §9.2). Запись исходного промпта пользователя в stream-json без эха
не приходит: её `uuid` адаптер берёт из эха `--replay-user-messages` (`isReplay: true`);
проверено на 2.1.307: `uuid` эха равен `uuid` записи промпта в jsonl.

Разрешения: при `--permission-prompts host --permission-prompt-tool stdio` запрос приходит в stdout как
`control_request` с `subtype: can_use_tool` (`tool_name`, `input`,
`permission_suggestions`); адаптер отвечает `control_response` с `behavior:
allow|deny`. Варианты для клиента: `once`, `session`, `always`, `deny`
(`always` только если среди `permission_suggestions` есть `addRules` в `localSettings`). Маппинг: `once` → allow;
`deny` → deny; `always` → allow плюс `updatedPermissions`, Claude Code сам пишет
правило в `.claude/settings.local.json` workspace. Правило адаптера: в
`updatedPermissions` идут только элементы `permission_suggestions` с `type: addRules`
и `destination: localSettings`, остальные подсказки (`setMode`, `addDirectories`
и т.п.) отбрасываются — иначе сессия переключается в `acceptEdits` и расширяет
список каталогов (проверено на 2.1.307). Если таких элементов нет, `always`
не предлагается; `session` → allow и правило в адаптере
до конца процесса. В `--restricted` (§8.4) локальные настройки не применяются,
`always` маппится в `session`. Хаб постоянные разрешения не хранит. Инструмент
`AskUserQuestion` приходит тем же `can_use_tool`: адаптер превращает его
в `clarify.request`, ответы возвращает через `updatedInput.answers`. Формат
`control_request`/`control_response` проверен на 2.1.307 при `--permission-prompt-tool stdio`
: точные JSON — `docs/notes/claude-control-protocol.md`, фикстуры —
`crates/hqc-claude-jsonl/fixtures/*-2.1.307.jsonl`.

Маппинг stream-json на §4: `stream_event` text delta → `agent.delta`; `assistant`
с текстом → `agent.message`, причём адаптер придерживает последний текст до
следующего элемента потока: `tool_use` → `final: false`, `result` → `final: true`;
`tool_use` → `tool.call`; `tool_result` → `tool.result`; `result` → `usage` и
`status idle`; эхо `--replay-user-messages` — сообщение `user` с `isReplay: true` —
→ `ack` на `session.input` (по этому полю эхо отличается от `user` с `tool_result`).
CLI повторяет в stdout каждый `control_response`, который адаптер отправил в stdin;
адаптер такие строки пропускает. `in_reply_to` = `event_id` ввода, который адаптер
отправил последним.

Рабочий каталог — `<workspace>/<cwd>`. Окружение процесса — из привязки пути
workspace в локальном конфиге воркера (§14). Авторизация — подписка, уже залогиненная
на машине (Keychain на маке); хаб её не хранит и не видит.

### 8.2 ACP-агенты (общий адаптер)
Адаптер запускает **отдельный ACP-процесс на каждую сессию hqc** (stdio JSON-RPC)
с окружением workspace (§14). Команда запуска — из локального конфига воркера по
имени `acp_agent`, абсолютным путём (в неинтерактивной оболочке `hermes` не в PATH).
При старте воркер делает `initialize` каждому сконфигурированному агенту и кладёт
в `hello.capabilities.acp_agents` его `load_session`, `resume`, `image`, `isolation`
и версию; отсутствие бинарника видно до создания сессии.

Инициализация: адаптер объявляет `clientCapabilities.fs` и `terminal` **недоступными** —
воркер не реализует чтение/запись файлов и терминал по просьбе агента. Если
`initialize` требует `authenticate` (провайдер не настроен), адаптер пишет `system`
«агент не настроен на воркере» и завершает старт ошибкой (§5.2).

Маппинг на §4: `session/new {cwd, mcpServers}` → создание; `session/prompt` → ход,
`ack` на `session.input` сразу после отправки; `session/update` с
`agent_message_chunk` → `agent.delta`/`agent.message`, `agent_thought_chunk` →
`agent.thinking`, `tool_call`/`tool_call_update` → `tool.call`/`tool.result`.
Конец хода — ответ на `session/prompt` со `stopReason`: адаптер придерживает
последний текст, при `tool_call` ставит `final: false`, при ответе на `session/prompt` —
`final: true`; `stopReason: cancelled` → без финального сообщения, `status idle`.
Диффы из `tool_call` сохраняются артефактом `kind: diff` и ссылаются из
`tool.result.artifact_id`, в `result_preview` — первые 4 КБ. `session/request_permission`
→ `approval.request` с **реальными** `options[]` агента (по `optionId`), `canonical`
по `kind` и имени; ответ — выбранный `optionId`. `session/cancel` → `session.stop`.
Resume — `session/load`, если агент объявил `loadSession`; иначе `session/new`
и пересказ истории адаптером. Адаптер **сверяет `sessionId` в ответе
`session/load`/`session/resume` с запрошенным** и при расхождении останавливается
с ошибкой: агент мог молча создать новую сессию. `usage` — только если агент отдаёт
токены; `clarify` в ACP нет. Повторный `session/prompt` во время хода адаптер
не отправляет никогда: очередь ввода ведёт воркер (§17.2), даже если агент умеет
ставить промпты в собственную очередь — иначе расходится `in_reply_to`.

Сессии без workspace для ACP-агентов не допускаются (§8.4): у них нет аналога
`--restricted`.

Claude Code через ACP (`@agentclientprotocol/claude-agent-acp`, Node, поверх Agent
SDK) **не используется**: нативный адаптер §8.1 проверен, даёт `--restricted`,
`--strict-mcp-config`, `--session-id` и control-протокол напрямую, без Node
на воркере.

#### 8.2.1 Особенности Hermes
Проверено по исходникам на llm-server, версия canary
`v0.21.4+canary.20261005T071900Z-434-g4787e4d56f` (коммит `4787e4d5`); у canary нет
стабильного номера, фиксируется хэш.

- Запуск: `~/.local/bin/hermes-acp` (или `hermes acp`) с профилем через `HERMES_HOME`.
  Профиль задаётся **локальной привязкой** в `env_bindings`, не хабом (§14).
  Это не messaging-гейтвей (`hermes-gateway.service`), тот не трогается.
- `initialize` объявляет `load_session`, `image`, `fork`, `list`, `resume`
  (методы ACP v2 включены через `use_unstable_protocol`).
- Воркер **всегда** ставит `HERMES_ACP_SKIP_CONFIGURED_MCP=1`, если окружение
  не разрешает `allow_local_mcp`. MCP hqc передаётся в `session/new` полем
  `mcpServers`; Hermes регистрирует их на сессию, переподключает при
  `session/resume` и не персистит.
- Варианты разрешений строятся динамически: `allow_once` всегда; `allow_session`
  и `deny_always`, если не `once_only`; `allow_always` только при `allow_permanent`;
  правки файлов идут отдельным запросом только с `allow_once`/`deny`. Маппинг
  по `option_id`: `allow_once` → once, `allow_session` → session, `allow_always` →
  always, `deny` / `deny_always` → deny. `allow_session` имеет `kind: allow_always`,
  поэтому по `kind` маппить нельзя.
- Таймаут `approvals.timeout` (300 с, читается на каждый запрос) →
  `approval.cancel {timeout}`.
- `session/resume` с неизвестным id **молча создаёт новую сессию** с warning в логе
  **[проверить]** — поэтому сверка id из §8.2 обязательна.
- `usage_update` в ACP у Hermes — оценка занятости контекста, не токены хода:
  в `usage` не переводится.
- Steer: текст промпта `/steer <text>` вмешивается в активный ход, `/queue <text>`
  ставит в очередь Hermes. Адаптер реализует `session.steer` через `/steer`;
  `/queue` не используется (очередь ведёт воркер).
- Toolset `hermes-acp` — без messaging, cron и `clarify` (проверено в `toolsets.py`).
  Память Hindsight и скиллы профиля работают как обычно, поэтому в чужих сессиях
  используется отдельный профиль (§11.1, §14).
- Сессия Hermes однописательная: сессию, которую ведёт Telegram-гейтвей, hqc не
  ведёт параллельно; её транскрипт можно только продолжить новой сессией hqc.
- На гейтвее дополнительно включается `api_server` (нужен `hermes peer` и Jobs API);
  адаптер им не пользуется.
- Каждая сессия — отдельный Python-процесс; рекомендуемый `idle_timeout` 30 минут.

### 8.3 Голая модель
Исполняется в хабе: OpenRouter, Anthropic API или локальная модель через прокси
воркера (§8.5). Цикл: модель ↔ MCP-инструменты hqc (§7), файловой системы нет.
Рантайм персональной сессии-ассистента по умолчанию, если при её создании нет
онлайн-воркера с Claude Code; позже меняется в настройках сессии.

### 8.4 Сессия без workspace
Для архитектурных обсуждений. Варианты: голая модель (§8.3) или Claude Code
на воркере `worker_id` во временной пустой папке `<worker-data>/scratch/<session_id>`
с флагом `--restricted` (убирает инструменты, запускающие команды, и ограничивает
файловые инструменты рабочим каталогом). Работает на подписке. Воркер выбирает
пользователь при создании; по умолчанию — последний использованный онлайн-воркер
с Claude Code. ACP-агенты здесь не допускаются.

### 8.5 Локальные модели
Воркер объявляет в `hello` локальные OpenAI-совместимые endpoint'ы (на llm-server это
`ninfer` на 127.0.0.1:8082, модель qwen3.8-27b). Хаб проксирует запросы голой модели
фреймами `model.proxy` со стримингом чанков. По умолчанию локальные модели доступны
только сессиям владельца воркера; `share_local_models: true` в локальном конфиге
открывает их голым моделям коллег в расшаренных проектах.

## 9. Зеркало и импорт сессий Claude Code

Плагин VS Code и CLI пишут одни и те же файлы
`~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`. Имя папки получается из пути заменой
символов на `-` и **обратно не декодируется** (`/a/b.c` и `/a/b-c` совпадают).
Поэтому рабочий каталог сессии воркер берёт из поля `cwd` в записях jsonl, держит
кэш «файл → cwd» на диске (§5.2) и имя папки использует только как ключ. Подпапки
с транскриптами субагентов и файлы, не являющиеся `<uuid>.jsonl`, игнорируются.
Проверено на 2.1.307: кроме `user`/`assistant` в файле есть записи `attachment`,
`file-history-snapshot`, `api-request*`, `last-prompt`, `ai-title`, `cost-state`,
`queue-operation`, `system` и другие; парсер их пропускает.

### 9.1 Импорт
При регистрации воркер отдаёт `workspace.list_candidates` (пути из `cwd` записей).
Пользователь выбирает, какие папки станут workspace и в каких проектах. Для
выбранных воркер читает jsonl и отдаёт историю как события §4 с исходными `ts`
и детерминированными `event_id`. Начальный `locked_by`: `vscode`, если в файл
писали за последние 30 с, иначе `none`.

### 9.2 Зеркало
На workspace с `mirror_enabled` воркер следит за всеми папками `~/.claude/projects/*`,
чей `cwd` лежит внутри пути workspace. Правило зависит от `locked_by` сессии,
а не от того, кто её начал:

- `locked_by: hqc` — зеркало **не эмитит**, события даёт адаптер; зеркало только
  детектирует чужого писателя (§9.3).
- `locked_by: vscode` или `none` — зеркало эмитит новые записи основной цепочки
  через `mirror.event`; неизвестный uuid файла становится новой сессией hqc
  с `locked_by: vscode` и `cwd` из записи.
- Поскольку адаптер и зеркало выводят `event_id` из одного и того же `uuid` записи,
  стык при переключении `locked_by` в любую сторону гасится уникальным индексом.

Сессия с `locked_by: vscode` видна в хабе read-only с пометкой «ведётся в VS Code».

### 9.3 Подхват **[решение]**
- Кнопка «подхватить» (`POST /takeover`) при `locked_by: vscode` и отсутствии записи
  в файл за последние 30 с: воркер запускает `--resume <uuid>`, `locked_by: hqc`.
  Хаб пишет `system`: «если сессия открыта в VS Code, её контекст устарел, после
  возврата переоткройте её там».
- Сообщение в сессию с `locked_by: vscode` **отклоняется** с предложением подхватить;
  тихой очереди нет.
- Детекция чужого писателя: учитываются только записи `type: user` и `type: assistant`
  основной цепочки (`isSidechain: false`). Запись с `uuid`, которого адаптер не видел
  в stream-json (и который не является его собственным промптом, §8.1), во время
  работы hqc-процесса означает, что сессию продолжили в VS Code: воркер
  останавливает своего агента после текущего хода, ставит `locked_by: vscode`, хаб
  пишет `system`. Пока hqc-процесс не запущен, любая такая запись переводит сессию
  в `locked_by: vscode`. Проверено на 2.1.307 фикстурами T0 (тест
  `t0_stream_and_transcript_share_uuids`): uuid всех записей `user`/`assistant` основной
  цепочки, включая `tool_result` и эхо промпта, совпадают в stream-json и jsonl.
- Одновременно сессию ведёт ровно один из двух.

### 9.4 Парсер
Формат jsonl неофициальный и меняется между версиями. Парсер терпит незнакомые типы
записей, логирует их и не падает. Один парсер обслуживает импорт, зеркало и
отображение подхваченной сессии. Покрывается фикстурами с реальных файлов.

## 10. Разрешения

### 10.1 В сессии
`approval.request` — первоклассное событие с кнопками в клиенте. Клиент показывает
**варианты из `options[]` запроса**, с подписями по `canonical`; у Claude Code это
once / session / always / deny, у ACP-агентов — то, что предложил агент. Запрос
уходит **владельцу воркера**; коллега с `write` видит запрос, но ответить не может.
Первый ответ побеждает; истечение, `session.stop`, ответ с другого устройства,
перезапуск воркера и архивация дают `approval.cancel`, клиент убирает кнопки.
При `auto_approve` хаб отвечает первым вариантом с `canonical: once`, `by: auto`;
режим включает только владелец воркера, в UI подсвечен; на расшаренном проекте он
означает выполнение кода коллегами без подтверждений, UI предупреждает об этом при
включении режима и при выдаче гранта `write`. Push-уведомление на каждый запрос
обязательно (§16).

### 10.2 Вне сессии
У каждого воркера есть служебная сессия (`system_session_id`), видимая только владельцу.
В неё приходят `worker.approval.request` как события `approval.request`: подтверждение
`workspace.create`, `workspace.delete`. Если операцию запросил сам владелец воркера
(`requested_by_owner`), подтверждение считается встроенным в запрос и отдельно
не спрашивается. Туда же воркер пишет логи уровня warning и выше.
Привязки окружений и профилей не подтверждаются через хаб вовсе — они меняются
только локально (§14).

## 11. Доступ и шаринг

Модель Google Docs: всё приватно, Grant даёт доступ. Функция
`can(user, action, resource)` стоит на каждом запросе к хабу с первого дня.

| действие | view | write | manage | владелец проекта |
|---|---|---|---|---|
| читать историю, артефакты | да | да | да | да |
| писать в сессию, прикладывать файлы | | да | да | да |
| создавать сессии в проекте | | да | да | да |
| остановить ход, steer | | да | да | да |
| отвечать на `clarify` | | да | да | да |
| `hqc_send`/`hqc_ask` в сессию (через своего агента) | | да | да | да |
| `hqc_schedule` в сессию | | да | да | да |
| отвечать на `approval` | | | | только владелец воркера |
| переименовать, архивировать, разархивировать, удалить сессию | | | да | да |
| создавать/удалять workspace | | | да (с подтверждением владельца воркера) | да |
| выдавать гранты | | | да | да |
| удалить проект | | | | да |
| менять окружение и профиль workspace | | | | только владелец воркера, локально |

- Грант на проект распространяется на все его сессии; грант на сессию — только на неё.
- Воркеры не шарятся. Коллега получает доступ к машине только через workspace
  расшаренного проекта, внутри его пути.
- Агент действует с правами владельца сессии.
- Отзыв гранта: текущий ход не прерывается, следующий ввод отклоняется, подписка
  клиента закрывается.
- Персональный проект не шарится. Несколько агентов разных пользователей в одной
  сессии — запрещено.
- Организаций и ролей нет.

### 11.1 Кто платит и чужие сессии **[решение]**
- **Все рантаймы на воркере** (Claude Code, Hermes, любой ACP-агент) работают
  с локальными учётными данными машины, то есть платит **владелец воркера**, кто бы
  ни создал сессию. Передавать ключи коллеги на чужую машину нельзя: владелец
  машины их увидит.
- **Голая модель** — ключи владельца сессии (`owner_id`) в хабе.
- Шаринг проекта с workspace на воркере означает «одалживаю свои учётные данные»;
  при выдаче гранта `write`/`manage` UI говорит это явно. Владелец воркера может
  запретить чужие сессии флагом `allow_foreign_sessions: false` (локальный конфиг,
  виден хабу через `hello`); флаг не запрещает чужой ввод в сессии самого
  владельца — это покрывается предупреждением при выдаче гранта.
- Чужая сессия (`owner_is_worker_owner: false`) на ACP-агенте с `isolation: none`
  отклоняется воркером. Чужая сессия на Hermes получает профиль, указанный
  в привязке как `foreign_profile`; если его нет — отклоняется: иначе коллега
  получил бы долгосрочную память владельца (Hindsight) на чтение и запись.
- Событие `usage` пишется, где рантайм его отдаёт; для ACP-агентов без usage
  расход по сессии в UI показывается как «нет данных».

## 12. Артефакты

Хранилище: диск хаба или S3-совместимое, ключ по sha256. Типы: file, image, diff,
html, text. Лимит артефакта 100 МБ. События хранят только `artifact_id`; клиент
при рендере запрашивает `GET /api/artifacts/{id}/url` и получает подписанную ссылку
с TTL 10 минут на публичный домен `artifacts.<hub-domain>` (вне Cloudflare Access).
HTML рендерится в iframe с этого домена, `sandbox="allow-scripts"` без
`allow-same-origin`, CSP без сетевых запросов, поэтому страница не видит cookie
и API хаба. Воркер качает артефакты по `/worker/artifacts/{id}` своим токеном
(§5.3), без TTL. Diff рендерится как дифф, image inline. Пользователь → агент:
через `.hqc/inbox/`; картинки для рантаймов с vision дополнительно отдаются
как изображение. Агент → пользователь: `hqc_attach`; диффы ACP-агентов адаптер
сохраняет артефактом сам (§8.2).

## 13. Жизненный цикл workspace

### 13.1 Создание
`dir` (существующая папка), `clone` (git clone в новую папку), `worktree` (git worktree
от существующего workspace на ветку). Только внутри `allowed_roots`, с подтверждением
владельца воркера через служебную сессию (§10.2), кроме случая, когда он сам
инициатор.

### 13.2 Удаление
`dir` — удаляется только запись в хабе, папка не трогается. `clone` и `worktree`
удаляются с диска только при `created_by_hqc: true`: `worktree` — через
`git worktree remove`, `clone` — удаление папки. Перед удалением воркер проверяет
незакоммиченные изменения и непушенные коммиты; при их наличии запрос подтверждения
содержит предупреждение. `ephemeral` workspace при архивации последней сессии
**не удаляется автоматически**, а помечается «к удалению» и показывает кнопку;
удаление всегда с подтверждением.

### 13.3 Шаблон сессии проекта
Необязательная настройка проекта: `{workspace_id, cwd_pattern: "<slug>", prompt}`.
Новая сессия по шаблону создаёт подпапку `<workspace>/<slug>` и начинает со
стартового промпта. Workspace в шаблоне обязателен. Для проектов типа cad и KiCad,
где сессия соответствует детали или плате.

### 13.4 Выбор workspace
При создании сессии из UI workspace выбирается явно; `hqc_spawn` и крон требуют
`workspace` или используют `default_workspace_id` проекта; без того и другого —
ошибка.

## 14. Локальный конфиг и окружения воркера

Локальный конфиг воркера — единственный источник истины для:

- `allowed_roots[]`;
- `environments{}` — именованные окружения;
- `env_bindings[]` — записи `{path, environment, hermes_profile?, foreign_profile?}`.
  Привязка ищется **по пути workspace** (не по `cwd` сессии), берётся самая длинная
  совпадающая, иначе `default`. `environment` из `session.start` только сверяется;
- `acp_agents{}` — `{name: {command: <абсолютный путь>, args[], isolation:
  verified|none}}`;
- `allow_foreign_sessions`, `share_local_models`.

Хаб получает копию в `hello` и показывает её; менять может только владелец воркера
на машине (`hqc-worker env ...`).

Окружение определяет, с чем запускается процесс агента (Claude Code или ACP-процесс —
оба запускаются воркером):

- `ssh_keys`: ключи, выдаваемые через отдельный сокет ssh-agent только этому
  процессу; по умолчанию агента нет;
- `env`: переменные и токены (GitHub и т.п.);
- `settings`: дополнительные настройки Claude Code через `--settings` (например,
  `permissions.deny` для `Bash(ssh*)`, `Bash(scp*)`); **только для Claude Code**,
  у ACP-агентов аналога нет, поэтому для них окружение `default` слабее: граница —
  только отсутствие ключей;
- `allow_local_mcp`: разрешить MCP-серверы с машины (Claude Code: без
  `--strict-mcp-config`; Hermes: без `HERMES_ACP_SKIP_CONFIGURED_MCP`); по умолчанию
  false;
- `network`: политика для песочного режима bash Claude Code, если включён;
- `roots`: дополнительные разрешённые пути;
- `launch`: `process` (единственное значение в первой версии; `os-user` и `container`
  зарезервированы).

Окружение `default`: нет ключей, нет токенов, `permissions.deny` на ssh/scp,
локальные MCP выключены. Ключи одного проекта (например `infra`) выносятся из
`~/.ssh` в хранилище воркера (Keychain на маке) и доступны только сессиям путей
с этим окружением.

Ограничение: без контейнера или отдельного пользователя ОС агент видит файлы
и окружение процессов пользователя, включая чужие workspace и токены сессий на той же
машине. Граница — отсутствие ключей, а запреты в настройках — первая линия,
не граница. Windows в первой версии: только `env` и `settings`, без `ssh_keys`.

## 15. Поиск

Postgres: `tsvector` с конфигурацией `simple` (смешанный русский и английский текст)
плюс `pg_trgm` для fuzzy по `event.payload.text` и названиям сессий. Результаты
фильтруются по грантам. Ограничение: у `simple` нет стемминга, словоформы находит
только триграммный поиск. Один индекс, два потребителя: строка поиска в клиенте
и `hqc_search_history`.

## 16. Уведомления и крон хаба

Web Push на события: `approval.request`, `clarify.request`, `worker.approval.request`
(владельцу воркера); финальный `agent.message`, `peer.message` и `system` о
недоставленном вводе — владельцу сессии и пользователям с грантом, которые писали
в эту сессию, если у них нет активной WS-подписки на неё. На iOS Web Push работает
только из установленного PWA — ограничение первой версии. Мост в Telegram —
опционально, через `hermes send`.

Крон хаба: `ScheduledMessage` — в `at` или по `cron` в `timezone` пользователя
отправить `message` в `session_id`, либо создать сессию по шаблону проекта
в `workspace_id` и отправить туда. Пропущенные запуски (хаб лежал): при `catch_up`
выполняется один раз, иначе пропускаются. Никакого планировщика задач для агентов;
всё сложнее — крон Hermes.

## 17. Состояния и жизненный цикл

### 17.1 Состояние сессии
`state` вычисляется хабом при отдаче из `archived`, `locked_by`, статуса воркера,
открытых запросов, `process_state` и доставки. Приоритет сверху вниз, первое истинное;
отдельно отдаётся `queue_len` — число вводов в `pending`:

| state | когда |
|---|---|
| `archived` | архив |
| `locked_vscode` | `locked_by: vscode` |
| `worker_offline` | воркер недоступен |
| `waiting` | есть открытый `approval`/`clarify` |
| `error` | есть ввод с `delivery: failed` или последний ход завершился ошибкой процесса |
| `running` | идёт ход |
| `queued` | `queue_len > 0`, хода нет (агент поднимается) |
| `idle` | иначе |

Событие `status.state` использует тот же набор.

### 17.2 Ввод во время хода **[решение]**
Ввод из очереди ввода (§7.1), пришедший во время хода, ждёт финального ответа
и запускает следующий ход; один ход на каждый элемент, в порядке `seq`. Кнопка
«прервать и отправить» делает `session.stop` и затем ввод. Для рантаймов с `steer`
(Hermes) доступна кнопка «вмешаться»: `session.steer` без прерывания. Два пользователя
с `write` — та же очередь.

### 17.3 Процессы
- Простаивающий агент по умолчанию не трогается. `idle_timeout` воркера > 0 —
  процесс завершается, следующий ввод поднимает его через resume.
- Воркер офлайн: ввод копится в хабе (`pending`), сессия `worker_offline`,
  доставка при подключении в порядке `seq`. Открытые `approval` на офлайн-воркере
  остаются открытыми до его возвращения или `expires_at`.
- Перезапуск воркера: все агенты `stopped`; открытые `approval` получают
  `approval.cancel {worker_restart}`; незавершённый ход теряется, хаб пишет `system`;
  `pending` вводы досылаются после `hello`.
- Перезапуск хаба: воркеры переподключаются, досылают outbox; `PendingRequest`
  восстанавливаются из базы, недоставленные `mcp.result` досылаются.

### 17.4 Архивация и удаление
- Архивация: хаб шлёт `session.kill`, открытые запросы получают
  `approval.cancel`/`clarify.cancel {archived}`, `pending` вводы помечаются `failed`,
  буфер send очищается. Разархивация возвращает `idle`; процесс поднимется через
  resume по первому вводу.
- Удаление сессии: события, `ReadCursor`, `PendingRequest`, артефакты сессии
  (если на них не ссылается другая сессия) удаляются; файлы jsonl на воркере
  **не трогаются**; поздние события из outbox воркера получают `ack` и отбрасываются
  (§5.2).

## 18. Стек

- Hub: Rust, axum, tokio, sqlx, Postgres. Артефакты — диск или S3.
- Worker: Rust, один статический бинарник на macOS/Linux/Windows, systemd/launchd/
  служба Windows, опциональная иконка в трее. MCP-сервер на `rmcp` (подкоманда
  `hqc-worker mcp`). ACP-клиент — крейт `agent-client-protocol` (официальный Rust SDK
  протокола) **[проверить]** актуальность. Secure store для токенов и ключей.
- Client: TypeScript, адаптивный веб, PWA; Tauri на том же коде позже.
- Общие крейты: `hqc-proto` (схема событий и фреймов, версионируется),
  `hqc-claude-jsonl` (парсер транскриптов), адаптеры — отдельные крейты.

## 19. Итерации

1. Хаб с Postgres и проверкой JWT Access, воркер с регистрацией и outbox, доставка
   в обе стороны с ack и политикой `failed`, адаптер Claude Code с `can_use_tool`,
   веб-чат, кнопки разрешений, Web Push. Упрощения: workspace один и задаётся
   конфигом воркера, проект один (личный), MCP hqc нет. Критерий: начать сессию
   из веба, вести её с телефона, подтвердить разрешение.
2. Проекты и workspace, служебная сессия воркера, импорт и зеркало VS Code, подхват
   в обе стороны, артефакты, `.hqc/inbox`. Критерий: подхватить с телефона сессию,
   начатую в VS Code.
3. MCP hqc: attach, send, ask, list, search, schedule; фреймы `mcp.call`;
   MCP-сервер по §7.2. Персональная сессия.
4. Общий ACP-адаптер (первый агент — Hermes, включая steer); голая модель
   и локальные модели через прокси.
5. Workspace: clone, worktree, ephemeral, шаблон сессии. Окружения, привязки
   и `acp_agents` на воркере.
6. Шаринг: Grant, диалог «поделиться», правила «кто платит» и чужих сессий. Крон хаба.
7. Tauri, нативные уведомления, несколько агентов в одной сессии.

## 20. Хостинг и безопасность

- Хаб слушает только интерфейс туннеля; `/api/*` за Cloudflare Access с проверкой
  JWT; `/worker/*` вне Access, только токены воркеров, rate limit на enroll;
  `artifacts.<hub-domain>` публичный, только подписанные ссылки с TTL.
- Токены воркеров — случайные 256 бит, в базе только хэш; отзыв из UI.
- `api_keys` пользователей шифруются мастер-ключом из env хаба.
- Cloudflare Tunnel: лимит тела запроса ~100 МБ (совпадает с лимитом артефакта),
  долгие соединения — heartbeat и переподключение.
- Бэкап: Postgres и хранилище артефактов; outbox воркеров переживает перезапуск хаба.

## 21. Риски

- Формат jsonl Claude Code и протокол stream-json/control неофициальные; держать
  парсер и адаптер терпимыми, покрыть фикстурами, фиксировать версию CLI.
- Подхват опирается на общий файл сессии у плагина и CLI; смена хранилища плагином
  ломает зеркало.
- ACP у Hermes стабилен, но методы ACP v2 (`session/resume`, `session/fork`) ещё
  черновик; адаптер держится на v1 плюс `session/load` и фиксирует хэш коммита
  Hermes (`4787e4d5` на llm-server).
- Граница между сессиями на одной машине без контейнера мягкая (§14); для
  ACP-агентов кроме Hermes она ещё слабее (§2.1).
- Цепочки агент→агент ограничены таймаутами и бюджетами, а не семантикой; при
  злоупотреблении `ask` деньги тратятся до лимита.

## 22. Аналоги и заимствования

Проверены на 2026-10-22.

- **Codync** (Rust-хост на каждой машине, ACP к 40+ агентам, SQLite, iPhone/Electron/TUI,
  Cloudflare-relay с E2E). Ближе всего по духу: боты с папкой и политикой разрешений,
  групповые чаты с @ и лимитом в 3 раунда, встроенный `team` MCP для вопросов другим
  ботам. Архитектура обратная нашей: много хостов, клиент подключается к каждому.
  Нет веба и Android, зеркала VS Code, шаринга, общего поиска и очереди при офлайне.
- **Paseo** (TS-демон, Expo-клиенты, relay). Зрелее по клиентам, без Hermes и без
  одного хаба над многими машинами.
- **Claude Code Remote Control** — официально, только Claude Code, без проектов
  и Hermes.
- **CloudCLI / claudecodeui** — одна машина, читает `~/.claude/projects`; единственный
  аналог с зеркалом сессий VS Code.
- **Agent Host Protocol (Microsoft, MIT)** — протокол «хост между многими клиентами
  и ACP-агентами»: действия с `serverSeq`, реконнект по `lastSeenServerSeq`,
  write-ahead с эхом `origin`, «первый `toolCallConfirmed` побеждает», один ход
  на чат. Это ровно наш §5–§6 и §17, но без воркеров, пользователей, грантов,
  агент→агент и хранения. Целиком не берём; взяты: эхо `client_op_id` (§6), правило
  первого ответа на approval (§10.1), словарь действий при именовании событий.
  Совместимый AHP-фасад над `/api/ws` возможен позже, если появятся сторонние клиенты.
- **OpenAB / OpenACP** — мосты ACP-агентов в Discord, Telegram, Slack. Запасной UI:
  если веб-клиент задержится, служебный бот в Telegram с темами форума покрывает
  уведомления и ответы на approval. В ТЗ не входит.

### 22.1 Отвергнутая альтернатива: без хаба
Модель Codync — каждый воркер сам хост, клиент подключается ко всем. Отвергнута,
потому что `ask`/`send` между машинами, общая история и поиск, шаринг коллегам,
очередь ввода при офлайн-воркере и один адрес для push требуют общего состояния;
без хаба каждую из этих вещей пришлось бы делать peer-to-peer.

## 23. История изменений

### 0.7 → 0.8 (выводы T0 в тексте)
Расхождения из 0.7 закрыты в §8.1: в команде запуска `--permission-prompt-tool stdio`
и `--allowedTools mcp__hqc` (проверено живым прогоном: работают `mcp__hqc` и
`mcp__hqc__*`); правило адаптера для `always` — в `updatedPermissions` только `addRules`
с `destination: localSettings`, `always` предлагается только при их наличии; эхо
собственных `control_response` пропускается, `ack` — по `user` с `isReplay: true`.
§7: инструменты hqc без подтверждения; новый §7.2 «Требования к MCP-серверу hqc».
§7.1: `MCP_TOOL_TIMEOUT` в мс выставляется как `(ask_timeout_s + 60) * 1000`; idle-таймаут
stdio MCP-серверов 30 минут (проверено: молчащий вызов обрывается через 1800 с,
progress раз в 60 с его удерживает), отсюда обязательные progress-уведомления в §7.2.
§19: итерация 3 — MCP-сервер по §7.2. Новый фрейм `mcp.cancel {request_id, reason}`
(§5.4): воркер закрывает отложенный `ask`, если рантайм отменил вызов или MCP-процесс
завершился (§7.2); хаб закрывает `PendingRequest` с ошибкой, пишет `system` с кодом
`ask_cancelled` в сессию-отправитель, целевой сессии ничего не шлёт (§5.2).

### 0.6 → 0.7 (T0)
Проверено на Claude Code 2.1.307 (`docs/notes/claude-control-protocol.md`, фикстуры
`*-2.1.307.jsonl`): формы `can_use_tool` и ответов allow/deny/`updatedPermissions`, `always`
пишет правило в `.claude/settings.local.json` рабочей папки (§8.1); эхо
`--replay-user-messages` несёт `uuid` промпта из jsonl (§8.1, §9.3); `AskUserQuestion` приходит
как `can_use_tool`, ответы — `updatedInput.answers` (§4, §8.1); `MCP_TOOL_TIMEOUT` в мс,
по умолчанию 1e8 мс (§8.1). **Расхождения, текст §8.1 пока не менялся:**
(1) одного `--permission-prompts host` мало — без `--permission-prompt-tool stdio` запросы
отклоняются сразу (`permission_denied`, `source: config`), а `AskUserQuestion` нет в `tools`;
(2) `updatedPermissions` из всех `permission_suggestions` включает и `setMode acceptEdits`,
и `addDirectories` на сессию — для `always` надо передавать только `addRules` с
`destination: localSettings`; (3) вызовы MCP-инструментов тоже идут через `can_use_tool`,
инструменты hqc (итерация 3) придётся разрешать заранее.

### 0.5 → 0.6 (сверка со скелетом кода)
Фрейм `event` без дублирующего `session_id`: он берётся из события (§5.4);
`runtime_session_id` — строка, у ACP-агентов не обязательно uuid (§3 Agent);
версия CLI в проверках uuid исправлена на 2.1.307 по фикстурам (§8.1, §9, §9.3).

### 0.4 → 0.5 (по ревью 5 и 6)
Проверено на сборках: uuid сообщений stream-json совпадают с uuid записей jsonl,
включая `tool_result`; Hermes не персистит `mcpServers` из `session/new`; toolset
`hermes-acp` без `clarify`; варианты разрешений Hermes динамические; версия
Hermes — canary `4787e4d5`; `MCP_TOOL_TIMEOUT` у Claude Code существует, у Hermes
`mcp.tool_call` 300 с. Правки: зеркало по `locked_by`, общий детерминированный
`event_id` у адаптера и зеркала (§9.2, §3, §8.1); фреймы `mcp.call`/`mcp.result`,
отложенный `ask` и досылка после `hello` (§5.2, §3 PendingRequest); §2.1 сужен,
`isolation` у `acp_agents`, `HERMES_ACP_SKIP_CONFIGURED_MCP` всегда (§2.1, §14);
профиль Hermes из локальной привязки, `foreign_profile` для чужих сессий (§11.1,
§14, §8.2.1); §8.2 разделён на общий ACP и Hermes: конец хода по `stopReason`,
ack после `session/prompt`, `fs`/`terminal` недоступны, варианты разрешений
от агента, `session/load` по capability, сверка `sessionId` после resume,
`usage` необязателен, диффы артефактом, `acp_agent` в `session.start`,
абсолютный путь команды, capabilities в `hello` (§8.2, §5.3, §3 Worker); `clarify`
из `AskUserQuestion` Claude Code (§4, §8.1); таймаут MCP-инструмента ≥
`ask_timeout_s` (§7.1); экранирование `[hqc peer` при доставке во всём тексте
(§7.1, §4); политика `delivery: failed` после 3 неудачных стартов, кнопка
«повторить» (§5.2, §17.1); смена рантайма = новая сессия рантайма (§3 Agent);
артефакты воркеру по `/worker/artifacts/{id}` без TTL, ссылки клиенту при
рендере (§5.3, §12); `running` выше `queued`, `queue_len` (§17.1); привязка
окружения по пути workspace (§14); `hqc_ask` в свою сессию — ошибка;
`share_local_models` (§8.5); `client_op_id` в POST (§6); назначение
`owner_is_worker_owner` (§5.3); буфер send: очистка по ack, лимит 50/64 КБ (§7.1);
ACP без workspace запрещён (§3, §8.4); «все рантаймы на воркере» в §11.1; поведение
при несовпадении версии протокола (§5.1); §17.4 архивация и удаление; steer для
Hermes в итерации 4 (§19, §17.2); кэш «файл → cwd» (§9); термин «ход» уточнён;
ссылки в шапке исправлены; §2 и §7 переформулированы; старая история вынесена
в `SPEC-CHANGELOG.md`.

### 0.3 → 0.4 (обзор аналогов)
ACP как общий адаптер вместо самописного `tui_gateway` для Hermes; MCP hqc через
`mcpServers` в `session/new`; рантайм `acp` с `acp_agent`; Claude Code остаётся
на нативном адаптере; `client_op_id`; раздел аналогов и отвергнутая альтернатива
без хаба; риск по ACP v2.
