# T4 — открытые вопросы и принятые решения

Задача T4 (адаптер Claude Code, `crates/hqc-worker/src/adapter/claude_code.rs` и подмодули
`engine`, `rules`, `paths`, `process`, `scripted`) делалась без человека. Где спека, заметка
T0 или постановка молчат, принято консервативное решение; оно записано здесь и в коде.
`hqc-proto` и `docs/SPEC.md` не менялись — пункты, требующие правки спеки, помечены
**→ спека**.

Версии CLI: фикстуры T0 сняты на **2.1.307**; к моменту T4 CLI на home-mac обновился до
**2.1.308** — на нём шли живой тест и разовая проверка `interrupt` (п. 5).

## Запуск

1. **Командная строка** (порядок фиксирован тестом `command_line_new_then_resume`):
   `claude -p --output-format stream-json --input-format stream-json
   --include-partial-messages --replay-user-messages --permission-prompts host
   --permission-prompt-tool stdio --strict-mcp-config [--mcp-config <json>]
   --allowedTools mcp__hqc (--session-id|--resume) <uuid> --verbose [--model <m>]`.
   Бинарник — `claude_bin` из конфига, иначе `claude` из `PATH`; рабочий каталог —
   `<workspace>/<cwd>` (`StartSpec::workdir`). `--mcp-config` не передаётся: точка
   расширения — `ClaudeOptions::mcp_config` (итерация 3). Не передаются и пока не
   проверены: `system_prompt` (`--append-system-prompt`?), `disallowed_tools`
   (`--disallowedTools`?), `MCP_TOOL_TIMEOUT` из `ask_timeout_s` (§7.1, нужен вместе с MCP
   hqc). Окружение процесса — окружение воркера (окружений §14 в итерации 1 нет).
2. **`--session-id` или `--resume` решает наличие транскрипта, а не метод трейта.** Если
   `<config>/projects/<encoded cwd>/<uuid>.jsonl` существует — `--resume`, иначе
   `--session-id`, и для `start`, и для `resume`. Так `start` после перезапуска воркера
   продолжает сессию (решение T3, п. 7), а `resume` процесса, который убили по
   `idle_timeout` до первого промпта (CLI ещё не создал файл), не падает на
   несуществующем id. `<config>` — `$CLAUDE_CONFIG_DIR` или `~/.claude`. Живой тест:
   первый запуск `--session-id`, повторный после `kill` — `--resume`, процесс поднялся.
   Решено в 0.11: записано в §8.1; `mode` из `session.start` выбирает метод трейта, флаг CLI по-прежнему решает транскрипт.
3. **Кодирование папки проекта.** Как функция `J0` в коде CLI 2.1.308: каждая UTF-16
   единица пути, не являющаяся ASCII-буквой или цифрой, → `-`; если результат длиннее
   200 символов — первые 200 + `-` + base36 от `abs` 32-битного хэша строки
   (`h = (h << 5) - h + unit | 0`). Кодируется `process.cwd()`, то есть реальный путь,
   поэтому адаптер делает `canonicalize` (`/tmp` → `/private/tmp`, в `~/.claude/projects`
   лежит `-private-tmp`). Сверено: имена папок прогонов T0
   (`-Users-home-mac-work-hqc-tmp-spike-once3` и т.д.; в фикстурах пути переписаны на
   `/tmp/hqc-fixture/...`, поэтому сверка шла по `~/.claude/projects`), хэш длинного пути —
   с эталоном, посчитанным самой функцией CLI в node; живой тест нашёл транскрипт по
   вычисленному пути `-Users-home-mac-work-hqc-t4-tmp-live-<v7>`. **→ спека §9 (зеркало
   будет искать те же файлы).**
   Решено в 0.11: правило кодирования (CLI 2.1.308) записано в §9 как уточнение к «обратно не декодируется».
4. **`runtime_session_id` не пришёл** (хаб по T3 всегда его кладёт): uuid сессии Claude =
   `session_id` hqc, адаптер сообщает его `AdapterEvent::RuntimeSessionId`. Не uuid —
   ошибка старта.
5. **Ранний выход.** Если процесс завершился в первые 500 мс (`ClaudeOptions::early_exit`),
   `start`/`resume` возвращает ошибку с хвостом stderr (→ `agent_start_failed`,
   retryable), а не `Exited`: иначе супервизор с непустой очередью поднимал бы его
   по кругу. Цена — +0,5 с к каждому старту. Нет бинарника — ошибка `spawn` сразу.
   Решено в 0.11: записано в §17.3.

## Остановка и завершение

6. **`stop` → `control_request {subtype: "interrupt"}`, а не SIGINT.** В заметке T0
   прерывания нет, но в коде CLI 2.1.308 (print.ts) `control_request` с `subtype:
   "interrupt"` обрабатывается. Проверено разовым живым прогоном (haiku, один ход, не
   закоммичен): через ~20 мс после запроса пришёл `result` с `subtype:
   "error_during_execution"`, `usage` нулевой, частичный текст ответа пришёл отдельной
   записью `assistant` (адаптер отдал его как `agent.message final: true`), процесс
   остался жив. Запасной путь: если `result` нет за 10 с (`ClaudeOptions::stop_grace`),
   группе процесса уходит SIGINT (CLI на SIGINT завершается — тогда `Exited`, и
   супервизор поднимет процесс через `resume`). **→ спека §8.1: прерывание хода.**
   Решено в 0.11: записано в §8.1 (`control_request interrupt`, запасной SIGINT через 10 с).
7. **`kill`**: SIGTERM группе процессов, через 5 с SIGKILL; `Exited` после `kill` не
   шлётся. Процесс запускается в своей группе (`process_group(0)`): сигналы доходят до
   запущенных им оболочек `Bash`, а Ctrl-C в терминале воркера не доходит до агентов
   (их гасит супервизор). Плюс `kill_on_drop`.
8. **Выход сам по себе** → `Exited {error}`: `None` при коде 0, иначе «exit code N» /
   «killed by signal N» и последняя строка stderr. stderr пишется в лог на `debug`.
   Придержанный текст при любом выходе отдаётся как `agent.message final: false`.

## Маппинг событий

9. **`event_id`.** `Event::id_from_claude_record(uuid записи)` для `tool.call`,
   `tool.result`, `agent.message`, `agent.thinking`. CLI пишет один блок контента на
   запись (во всех фикстурах и в живом прогоне); если блоков в записи несколько, блок
   `i > 0` получает v5 от строки `"<uuid>/<i>"` в том же пространстве имён — зеркалу
   нужно то же правило. **→ спека §3 Event / §9.2.**
   Решено в 0.11: правило `"<uuid>/<i>"` обязательно, записано в §3 Event и §9.2. `usage`, `status`, `agent.delta`,
   `approval.request`, `clarify.request` — свежий v7 (у `result` есть `uuid`, но в jsonl
   его нет).
10. **`final`.** Текст придерживается; любой следующий блок ассистента (текст,
    `tool_use`, непустой `thinking`) или `tool_result` выпускает его с `final: false`,
    `result` — с `final: true`. Ход без текста (например, `error_max_turns` после
    вызова инструмента) не даёт `agent.message` вовсе. `in_reply_to` — `event_id`
    последнего отправленного ввода.
11. **`agent.thinking`** — только для непустого `thinking`: на haiku приходят блоки с
    пустым текстом и одной подписью, они пропускаются.
12. **`result`** → `usage` (`input_tokens`, `output_tokens` из `usage`;
    `cache_read` ← `cache_read_input_tokens`, `cache_write` ←
    `cache_creation_input_tokens`; `cost_usd` ← `total_cost_usd`; `model` — из
    `system/init`, иначе первый ключ `modelUsage`), затем `status {state: idle}`, затем
    `TurnEnded`. Если `subtype != success` или `is_error`, в `status.detail` кладётся
    `subtype` (`error_max_turns`, `error_during_execution`). **→ T2: считать ли это
    «ход завершился ошибкой» для `state: error` (§17.1).**
13. **Эхо `isReplay: true`** → `InputAccepted` для самого старого отправленного и ещё не
    подтверждённого ввода (FIFO); uuid эха с `event_id` хаба не связан и не нужен.
14. **Сабагенты.** Строки с непустым `parent_tool_use_id` (`stream_event`, `assistant`,
    `user`) не маппятся: их записи живут в отдельных sidechain-файлах, событие с их uuid
    зеркало не повторит. `can_use_tool` от сабагента обрабатывается как обычно.
15. **Превью** режутся до 4 КБ по границе символа; `args_preview` — компактный JSON
    `input`; `result_preview` — строка или текстовые блоки, прочие блоки как `[type]`;
    `error_text` — первые 300 байт текста ошибки. `duration_ms` не заполняется.
16. `artifact_ids` и `peer_prefix` в `session.input` в итерации 1 не поддержаны: уходит
    только текст, в лог — warn.
17. Строки stdout читаются как UTF-8 с заменой битых байтов; неразборчивая строка — warn
    и пропуск. Неизвестные `type`/`subtype` пропускаются молча.

## Разрешения

18. **`approval.request`**: `request_id` — `request_id` CLI; `tool` — `tool_name`;
    `description` — `description` из запроса и под ним команда / `file_path` / `url` /
    `pattern` / `blocked_path` (на телефоне видно, что именно подтверждается);
    `options` — `once` «Allow once», `session` «Allow for this session», `always`
    «Always allow» (только при `addRules` с `destination: localSettings` и не в
    `restricted`), `deny` «Deny»; `expires_at` не ставится.
19. **`always`** → `allow` + `updatedPermissions` только из элементов `addRules` с
    `destination: localSettings` (сверено с фикстурой `permission-always`, где спайк
    передал все подсказки). `always`, пришедший без таких элементов или в
    `restricted`, работает как `session` (§8.1).
20. **`session`** (`rules.rs`): запоминается точная пара `(tool_name, input)` и правила из
    `permission_suggestions[].rules` того же инструмента, которые адаптер умеет
    сопоставлять безопасно: без `ruleContent` — весь инструмент; Bash `"<префикс> *"` или
    `"<префикс>:*"` — команда равна префиксу или начинается с `"<префикс> "` и **не
    содержит** `; & | \` $ ( ) < >` и перевода строки (иначе `touch x && rm -rf ~` прошёл
    бы по правилу `touch x *`); Bash без `*` — точная команда. Прочие формы (глобы путей,
    `domain:` у WebFetch) не интерпретируются — только точное совпадение. Правила живут в
    `Core`, который создаётся на каждый процесс, то есть до конца процесса. Совпавший
    запрос отвечается `allow` сразу, `approval.request` не создаётся.
21. **`deny`** — `{"behavior": "deny", "message": "The user denied this tool call."}`
    (текст отличается от спайка, тест сравнивает stdin без поля `message`). Без
    `interrupt: true`: модель продолжает ход.
22. **Ответ на неизвестный или уже закрытый `request_id`**, неизвестный `option_id` —
    ошибка адаптера (супервизор отвечает хабу `error {internal}`), запрос остаётся
    открытым при неизвестном варианте.
23. Запросы, открытые к моменту `result` (после `interrupt`), молча забываются:
    `approval.cancel` при `stop` пишет хаб (§10.1, T2).
    Решено в 0.11: отмены при `stop` пишет только хаб, адаптер их не эмитит (§10.1).
24. `control_request` других подтипов (`hook_callback`, `mcp_message`, …) получает
    `control_response {subtype: "error"}` — CLI не ждёт вечно.
25. `--allowedTools mcp__hqc` передаётся всегда; адаптер инструменты `mcp__hqc__*`
    отдельно не пропускает — флаг проверен в T0 (п. 5 заметки).

## AskUserQuestion

26. Распознаётся по `tool_name == "AskUserQuestion"` с непустым `input.questions`
    (`requires_user_interaction` не требуется). Вопросы получают `id` `"1"`, `"2"`, … по
    порядку; `options` — `label` вариантов, `multi` — `multiSelect`.
27. Ответ: `allow` с исходным `input` плюс `answers {текст вопроса: label}` (совпадает
    с фикстурой байт в байт как JSON). Ответ-массив для `multi` склеивается через `", "`
    — **[проверить]** формат мультивыбора на живой сборке. Ответы на неизвестные `id`
    отбрасываются; если не осталось ни одного (в том числе `answers: {}` — отмена
    хабом) — `deny` с текстом «The user did not answer the questions.».
    `approval.response` с `deny` на clarify-запрос тоже закрывает его `deny`.

## Интерфейс и тесты

28. Трейт `Adapter` не менялся. `BuiltinAdapters` стал структурой с
    `BuiltinAdapters::from_config(&Config)` (был unit-структурой-заглушкой, T3 п. 23);
    единственный пользователь — `main.rs`. Новые публичные типы для тестов и будущих
    адаптеров: `ClaudeCodeAdapters::new(ClaudeOptions, Arc<dyn Spawner>)`, трейт
    `process::Spawner` (stdin/stdout как `AsyncWrite`/`AsyncRead`, сигналы, будущее
    выхода), `scripted::ScriptedClaude` (проигрывает stream фикстуры, отвечает на
    `control_request` только после строки в stdin). В `tests/common` добавлен
    `start_worker_factory`.
29. **Живой тест: промпт изменён.** С заданным промптом «Run the shell command: echo
    hqc-live…» CLI 2.1.308 выполнил `echo` **без** `can_use_tool` (команда только для
    чтения разрешается автоматически) — первый прогон упал на отсутствии
    `approval.request`, при этом порядок остальных событий и маппинг были верны. В
    тесте команда — `touch hqc-live && echo hqc-live`. **→ спека §8.1: не каждый вызов
    Bash спрашивает разрешение.**
    Решено в 0.11: записано в §8.1.
30. Наблюдение для T2: `ack` на `session.input` и `agent.delta` уходят мимо outbox, а
    события хода — через outbox, поэтому хаб может получить `ack` ввода и дельты позже
    `tool.call`/`approval.request` того же хода. Интеграционный тест на порядок между
    ними не опирается.
    Решено в 0.11: записано в §5.2; хаб не открывает ход по `ack`, если финальный ответ на этот ввод уже есть (тест `late_input_ack_does_not_reopen_the_turn`); ход без финального текста — открыто.
31. Логи: на `info` — старт/выход процесса, ввод отправлен (`event_id`), запрос
    разрешения (имя инструмента), ответ (вариант), конец хода (`subtype`, `num_turns`,
    `duration_ms`); содержимого сообщений, команд и stderr на `info` нет (stderr — на
    `debug`, типы строк — на `trace`).
