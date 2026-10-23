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

## T15. Импорт, зеркало и подхват сессий Claude Code (сделано, SPEC 0.17 в ветке, после слияния 0.19)

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
  /api/workers/{id}/candidates`; миграция `0008` (`vscode_active_at`;
  в ветке была `0006`, переименована в T18).
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

## T16. Артефакты и `.hqc/inbox` (сделано, SPEC 0.17 в ветке, после слияния 0.18)

Разделы: §3 Artifact, §4 (`user.message.artifact_ids`, `attachment`), §5.3 `session.input`,
§6, §8.1, §12, §20. Без MCP: `hqc_attach` и `PUT /worker/artifacts` — итерация 3.
- hqc-proto: тела `Artifact` и `ArtifactUrl` (`api.artifact.json`, `api.artifact.url.json`).
- Хаб: хранилище на диске `HQC_ARTIFACT_DIR` по sha256, миграция `0007` (`artifacts`,
  `artifact_sessions`; в ветке была `0006`); `POST /api/artifacts` (multipart `file`, `kind?`; 100 МБ, `413
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

## T18. Сведение T15 с T16 и T17 (сделано, SPEC 0.19)

Разделы: §3 (Session, Agent), §4, §5, §5.3, §5.4, §6, §9, §10.2, §12, §16, §23. Зависит от
T15, T16, T17. T16 и T17 слиты раньше (SPEC 0.18), T15 сливается последней.
- Код: обе стороны каждого конфликта. hqc-proto — кадры `worker.approval.*` рядом с
  `workspace.list_candidates`, `mirror.set`, `mirror.event`, списки примеров покрывают все.
  Хаб — у сессии и `system`, и `vscode_active_at` (в `SESSION_COLS` тоже); `worker_ws`
  разбирает `worker.approval.request` и `mirror.event`, после `hello` досылает ответы на
  запросы воркера и шлёт `mirror.set`; `POST /takeover` служебной сессии — `409
  system_session` (новое, тест в `tests/system_session.rs`). Воркер — `HubLink::event_json`
  и `mirror_event`, супервизор держит ожидающие `workspace.use`, клиент артефактов и зеркало.
  Клиент — `Composer` с вложениями (T16) не рендерится у служебной сессии (T17), а у
  `locked_vscode` закрыт с подписью «Ведётся в VS Code» и плашкой «Подхватить» (T15); мок
  хаба отвечает на ввод `409 system_session` и `409 locked_vscode`.
- Миграция T15 `0006_mirror.sql` → `0008_mirror.sql` (`0006` — T17, `0007` — T16); она
  только добавляет `sessions.vscode_active_at` и индекс и от соседних не зависит.
- SPEC 0.19: записи §23 0.18 → 0.19 (T15), 0.17 → 0.18 (T16), 0.16 → 0.17 (T17); перенос
  отложенного тремя задачами в §3, §4, §5, §6, §10.2, §16 (список — в записи 0.19). Решение
  T16 «файлы пользователя всегда в `user.message.artifact_ids`» записано в §4.
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace`, `cd client && npm run typecheck && npm test && npm run
build`, `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh` → `PASS (rehearsal)`, живой
`HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_takeover` → PASS (44 с, 2026-10-23).
Открытое — вопросы в `docs/notes/t15-questions.md`, `t16-questions.md`, `t17-questions.md`.

## T19. Перерегистрация воркера без потери привязок и уборка отозванных (сделано, SPEC 0.21)

Разделы: §3 (Worker, Workspace, Session), §4, §5.1, §6, §10.2, §17.4. Фон: повторный `enroll`
той же машины (смена бинарника, хранилища токена) давал новый `worker_id`; сессии, workspace
и служебная сессия оставались на старом, «воркер офлайн» навсегда, а служебные сессии
отозванных воркеров висели в группе «Воркеры».
- hqc-proto: `EnrollInfo.replaceable[]` (`ReplaceableWorker {id, name, os, status, last_seen?,
  sessions_count}`), `EnrollConfirmRequest.replace_worker_id?`, `EnrollConfirmed.replaced?`,
  `EnrollPoll::Confirmed.replaced?`; примеры `api.enroll.*.replace*.json`.
- Хаб: `GET /api/workers/enroll/{code}` перечисляет офлайн и отозванных воркеров
  пользователя; `POST /api/workers/enroll/confirm` с `replace_worker_id` — проверка владельца
  (`400`) и что воркер не на связи (`409 worker_online`), код получает существующий
  `worker_id`, старый токен сбрасывается сразу, `name`/`os` новые, `revoked_at` и `boot_id`
  сбрасываются, служебная сессия разархивируется и переименовывается, в неё —
  `system worker_replaced` (`old_name`, `new_name`); опрос отдаёт `replaced`. `DELETE
  /api/workers/{id}` архивирует служебную сессию с `system worker_revoked`, её запросы
  закрываются `archived`. `DELETE /api/workers/{id}?purge=1` удаляет отозванного воркера,
  служебную сессию и workspace (`409 worker_not_revoked`, `409 worker_has_sessions` с
  `sessions_count`). `GET /api/workers?include_revoked=1`, `status: revoked`. Миграция `0009`:
  `enroll_codes.replaced_name`, архивация служебных сессий уже отозванных воркеров.
- Воркер: `enroll` печатает `Enrolled as worker <id> (replaced <имя>).`.
- Клиент: на экране подтверждения — выбор «Новый воркер» / «Это та же машина, заменить
  <имя>» (статус, последний раз на связи, число сессий); в «Настройках» у отозванных —
  «Удалить» с объяснением `worker_has_sessions`; группа «Воркеры» без архивных служебных
  сессий; мок хаба.
- Документация: SPEC §3 Worker, §4, §5.1, §6, §10.2, §17.4, §23 (0.21); RUNBOOK §2.3
  (перерегистрация той же машины), §2.6 (отзыв и удаление).
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace` (хаб: `tests/worker_replace.rs` — замена сохраняет id,
сессии и workspace, старый токен не работает, новый `hello` с тем же `system_session_id`;
нельзя заменить воркера на связи и чужого; отзыв архивирует служебную сессию, замена
отозванного возвращает её; purge с сессиями — `409`, без — удаляет; миграция `0009`;
воркер: `enroll_reports_the_replaced_worker`), `cd client && npm test && npm run build`
(`tests/enroll.test.tsx` — диалог замены; `tests/workers.test.tsx` — «Удалить», 409, группа
«Воркеры» без отозванных; e2e через мок), `E2E_CLAUDE_BIN=/usr/bin/false
tools/e2e-local.sh` → `PASS (rehearsal)`. Вопросы — `docs/notes/t19-questions.md`.

## T20. Подхват без развилок (сделано, SPEC 0.22)

Разделы: §3 (Worker, Session), §4, §5.2, §5.4, §6, §9.2, §9.3. Фон (2026-10-23, мак
пользователя): сессия была открыта в VS Code; hqc подхватил её после 30 с тишины в файле, воркер
дописал ходы через `--resume`; затем пользователь написал в ту же открытую сессию в VS Code, и
плагин прицепил ход к своей последней записи в памяти, а не к хвосту файла. В jsonl у записи два
потомка, VS Code видит только свою ветку, 37 записей hqc в его контекст не попали. Правило «30 с
тишины» не отличает закрытую сессию от открытой и бездействующей.
- hqc-proto (только добавление): `HeldBy {entrypoint, status}`; `held_by?` в `session.state` и
  `mirror.event` (поля нет / `null` / объект); `held_by?` в объекте сессии и `session.summary`;
  `system` с полями `session_forked` (`SessionForked {at_uuid, branch_from, orphaned_turns}`,
  `Event::id_of_fork`); примеры `worker.session.state.held.json`,
  `worker.session.state.released.json`, `worker.mirror.event.forked.json`,
  `api.session.vscode.json`.
- hqc-claude-jsonl: `Link` (`uuid`, `parentUuid`, `isSidechain` любой записи); фикстура
  `fork-2.1.308` (живой прогон без VS Code: `--resume --resume-session-at`).
- Воркер: модуль реестра (`mirror/holders.rs`) — `<config>/sessions/<pid>.json` раз в проход
  зеркала, живые pid (`kill -0`), без своих pid и групп процессов (pid детей адаптера — в
  `OwnRecords`), терпим к незнакомым полям и битым файлам; изменения держателя — отчёт о
  держателе (`session.state` с nil `session_id`) через outbox, не чаще раза в 2 с на транскрипт,
  заново после каждого `hello`; сессия, чей транскрипт открыли вне hqc, останавливается после
  хода; `held_by` в `mirror.event`; `capabilities.claude_code.sessions_registry`. Дерево записей
  транскрипта (`mirror/fork.rs`) и `system session_forked` через `mirror.event`.
- Хаб: миграция `0010` (`sessions.held_by`, `vscode_held_at`); держатель → `locked_by: vscode`
  (из `hqc` — `session_released` с `reason: vscode_open`, `pending` → `failed`), держатель исчез →
  `none` сразу, если hqc-процесса нет; при данных реестра `recent` не запирает; `POST /takeover`
  при держателе — `409 vscode_open` с `held_by`, без держателя у воркера с реестром — сразу,
  30 с — запасное правило; `held_by` у `409 locked_vscode`; на `hello` держатели забываются;
  `system` из зеркала — с автором `system`.
- Клиент: плашки «Сессия открыта в VS Code … закройте её там» (без кнопки) и «ведётся в VS
  Code» (кнопка «Подхватить»); `409 vscode_open` — текстом хаба; строка `session_forked` с
  иконкой предупреждения; мок: импорт приносит открытую в VS Code сессию с развилкой,
  `closeVsCode` для тестов.
- Документация: SPEC 0.22 (§3, §4, §5.2, §5.4, §6, §9.2, §9.3, §19, §23);
  `docs/notes/t15-questions.md` — вопрос про освобождение закрыт ссылкой; RUNBOOK §4 — сценарий
  подхвата с шагом «закройте сессию в VS Code».
Приёмка: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (воркер: `mirror::holders` — живой/мёртвый pid, свои pid и группа,
`sdk-cli` своего транскрипта, битые файлы; `mirror::fork` — параллельные вызовы не развилка,
развилка один раз, цикл; `mirror::tests::fork_is_reported_once`, `holder_in_registry`;
`tests/mirror.rs::holder_releases_the_session`; хаб: `tests/mirror.rs` —
`holder_blocks_takeover_until_closed`, `holder_releases_hqc`, `forked_event_is_stored`; парсер:
`t20_fork_in_one_transcript`), `cd client && npm test && npm run build` (`projects.test.tsx` —
открытая сессия без «Подхватить», `409 vscode_open` текстом, освобождение; `render.test.tsx` —
строка развилки), `E2E_CLAUDE_BIN=/usr/bin/false tools/e2e-local.sh` → `PASS (rehearsal)`.
Живой `HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_holder` подтвердил держателя и `409
vscode_open`, освобождение после выхода процесса упало и исправлено (порядок отчётов и
`recent`), повторный живой прогон не делался. Вопросы — `docs/notes/t20-questions.md`.
