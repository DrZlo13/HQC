# T3 — открытые вопросы и принятые решения

Задача T3 (ядро воркера) делалась без человека. Где спека молчит или неоднозначна, принято
консервативное решение; оно записано здесь и в коде. `hqc-proto` и `docs/SPEC.md` не
менялись — пункты, требующие правки спеки, помечены **→ спека**.

## Регистрация (`hqc-worker enroll`)

1. **Воркерская сторона enroll не описана в спеке** (§5.1, §6 знают только
   `POST /api/workers/enroll/confirm {code}`). Принято (`crates/hqc-worker/src/enroll.rs`):
   - `POST {hub}/worker/enroll` `{name, os, protocol_version}` →
     `200 {enroll_id, code, poll_interval_s?, expires_at?}`;
   - `POST {hub}/worker/enroll/poll` `{enroll_id}` →
     `200 {status: "pending"}` | `200 {status: "confirmed", token, worker_id}` |
     другой `status` (`expired`, `rejected`) или HTTP 404/410 — конец с ошибкой.
   - `enroll_id` — секрет между воркером и хабом, передаётся в теле, не в URL (не попадает
     в access-логи). По умолчанию опрос раз в 2 с, ожидание до `expires_at` или 15 мин.
   **→ спека §5.1/§6, согласовать с T2.**
2. **Secure store.** macOS — Keychain (generic password, service `hqc-worker`,
   account = `hub_url`). Другие ОС — файл `$HQC_HOME/token` с правами 0600 (каталог 0700,
   запись через временный файл и rename). libsecret/DPAPI из §5.1 не сделаны.
   `HQC_TOKEN_STORE=file` включает файловый бэкенд на любой ОС (так работают тесты).
   **→ спека §5.1: пометка «вне macOS пока файл 0600».**
3. `enroll` не пишет `worker.toml`: `hub_url` в конфиге задаётся руками; если конфига нет,
   enroll подсказывает путь.

## Локальный конфиг (§14, подмножество итерации 1)

4. Формат `worker.toml` — см. модуль `config.rs` и отчёт ниже. `idle_timeout` — целое
   число секунд, `0` = не трогать (§17.3). Неизвестные поля — ошибка (`deny_unknown_fields`):
   опечатка в конфиге ограничений не должна молча ослаблять их.
5. Окружений и привязок в итерации 1 нет: единственное окружение — `default`.
   `session.start.environment != "default"` → `error invalid_frame`. В `hello` уходит
   `environments: ["default"]`, `env_bindings: []`.
6. Проверки `session.start` (все → `error {code: invalid_frame, retryable: false}`, ничего
   не создаётся и не запускается):
   - `workspace_path` после нормализации (canonicalize) не равен `[workspace] path`
     или вне `allowed_roots`;
   - `workspace_path: null` (сессия без workspace, §8.4) — в итерации 1 не поддержана;
   - `cwd` абсолютный, содержит `..`, или через симлинк выходит за workspace;
   - `acp_agent` задан (ACP-агентов в конфиге нет);
   - `owner_is_worker_owner: false` при `allow_foreign_sessions = false`;
   - `runtime`, которого нет в сборке. Спорно: можно было бы `agent_start_failed`, но
     это не временная ошибка, повтор бессмыслен.

## Протокол и доставка

7. **start или resume.** В `session.start` хаб всегда кладёт `runtime_session_id`, и по
   фрейму не видно, новая это сессия или продолжение. Супервизор вызывает
   `Adapter::resume`, если этот процесс воркера уже запускал сессию (после `kill`,
   `idle_timeout`, падения), иначе `Adapter::start`. После перезапуска воркера первый
   запуск — `start`; адаптер Claude Code (T4) должен сам перейти на `--resume`, если
   jsonl с этим uuid уже есть. **→ спека §5.3: флаг `resume` в `session.start`?**
8. **Ввод для сессии без `session.start`** в текущем процессе (например, хаб после `hello`
   досылает `pending`, не прислав `session.start`) → `error {code: unknown_session,
   retryable: true}`. Ожидаем, что хаб шлёт `session.start` перед досылкой вводов.
   **→ согласовать с T2.**
9. `ack` на `session.start` — пустой `{}` после успешного `start`/`resume`; повторный
   `session.start` для живого процесса — тоже `ack {}` (процесс не пересоздаётся).
   `ack {}` на `session.stop`, `session.kill`, `approval.response`, `clarify.response`;
   ошибка адаптера на них — `error {internal}`. `session.kill` для неизвестной сессии —
   `ack {}` (убивать нечего).
10. **`session.kill` очищает очередь воркера.** Вводы остаются `pending` в хабе, но хаб
    досылает их только после следующего `hello`. **→ вопрос к T2/спеке: что хаб делает
    с очередью при kill.**
11. **Ввод без подтверждения.** Если ход закончился (`TurnEnded`) или процесс умер, а
    адаптер так и не подтвердил ввод (`InputAccepted`), ввод считается не дошедшим и
    возвращается в голову очереди (для Claude эхо приходит сразу при чтении stdin).
12. Повторный `session.input` с уже известным `event_id`: если ввод уже подтверждён —
    сразу `ack {event_id}` с новым `id` фрейма, повторно в рантайм не идёт; если в очереди
    или в полёте — ack уйдёт на новый `id`. Память подтверждённых — 256 последних на сессию.
13. При неудачном старте `error {agent_start_failed, retryable: true}` получает фрейм,
    вызвавший старт (`session.start` или `session.input`), и все вводы в очереди
    (хаб считает неудачи по вводам, §5.2).
14. Неожиданное завершение процесса агента (`Exited` с ошибкой или во время хода) —
    событие `system {code: "agent_exited"}` через outbox; если в очереди есть вводы,
    процесс поднимается через `resume`. Код `agent_exited` новый. **→ спека §4: список
    кодов `system`.**
15. `session.state`: `locked_by: hqc`, пока процесс не `stopped`, иначе `none`. Все
    состояния переотправляются после каждого `hello.ack`.
16. `error` от хаба на фрейм `event`: `retryable: false` — событие удаляется из outbox
    (иначе оно отправлялось бы вечно), `retryable: true` — остаётся до реконнекта.
    `ack` с `seq: null` (сессия неизвестна хабу) удаляет событие (§5.2).
17. Heartbeat — WebSocket ping каждые 20 с; если 60 с не пришло ни одного кадра,
    соединение считается мёртвым. Отдельного прикладного heartbeat-фрейма спека не задаёт.
18. Несовместимая версия: лог `error` «worker update needed», повтор через час (§5.1),
    больше ничего. Трея нет. `hello.ack` с другой `protocol_version` — warn и работа дальше.
19. 401/403 на апгрейде или `error {unauthorized}` на `hello` — лог «run enroll again»
    и реконнект с обычной задержкой (до 60 с), процесс не завершается: токен могли
    перевыпустить, а супервизор (launchd) всё равно перезапустил бы.
20. Корректное завершение (SIGTERM/SIGINT): всем агентам `kill`, `session.state stopped`,
    флаш outbox до 5 с ожидания `ack`; неподтверждённое остаётся на диске.

## Прочее

21. `capabilities.claude_code.version` — первый токен вида `цифры.точки` из
    `claude --version` (ожидается `2.1.307 (Claude Code)`). Формат вывода не проверялся
    на живой сборке. **[проверить]**. Путь — `claude_bin` из конфига или `claude` из PATH.
22. Токен в логах: `tungstenite` на уровне `trace` пишет запрос апгрейда вместе с
    `Authorization`. `main` принудительно ограничивает `tungstenite`, `hyper`, `reqwest`,
    `h2`, `rustls` уровнем `debug`, даже если `RUST_LOG` просит больше; тест проверяет
    вывод при `RUST_LOG=trace`.
23. Пока T4 не влит, `hqc-worker run` использует заглушку адаптера: любой старт —
    `agent_start_failed` (retryable).
24. `steer?` в трейте `Adapter` нет: фрейма `session.steer` в итерации 1 нет.
