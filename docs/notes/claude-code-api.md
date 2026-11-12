# Claude Code: что CLI отдаёт клиенту и что принимает

Справочник API Claude Code, восстановленный по коду. Что CLI шлёт в stdout в режиме
`-p --input-format stream-json --output-format stream-json`, что принимает на stdin, какие
запросы управления ходят в обе стороны, что пишет в транскрипт JSONL и как всё это разбирает
вкладка VS Code, — с граничными случаями, которые нашлись в коде. Версии: CLI 2.1.312 (на воркерах
ноутбука, agent-node-227 и agent-node-129; на agent-node-226 — 2.1.304, на agent-node-3 и
vvzvladmbpm1 — 2.1.318 по `claude --version`), типы Agent SDK 0.3.312 (для того же CLI) и 0.3.319 (для сравнения),
расширение VS Code 2.1.319. Приложение Claude (5.1231.2) несёт тот же Agent SDK и запускает тот же
CLI с теми же ключами stream-json, так что протокол у него этот же.

Документ собран по коду; то, что код не решает, проверено живыми прогонами на стенде
agent-node-227 (помечено «вживую» с именем записи, см. «Источники»). Что проверено живыми
запусками раньше, помечено ссылкой на SPEC (`hqc:docs/SPEC.md`, `docs/SPEC.md` этого репозитория,
`cc_remote/docs/SPEC.md`).

## Источники

Файлы лежат в `vendor-src/` этого репозитория (`/Users/vvzvlad/Data/Projects/hqc_dev/vendor-src/`,
в git не входит).

| Ярлык | Файл | Что это |
| --- | --- | --- |
| `cli:N` | `cli-2.1.312.pretty.js`, строка N | JS из бинаря `~/.local/share/claude/versions/2.1.312`: каждый модуль от метки `// @bun` до первого нулевого байта, отформатирован prettier 3.3.3; модули разделены строкой `/* ==== module at byte N of the binary ==== */`, имена минифицированы. Часть вхождений есть только в сыром бинаре (байткод) — сверять `LC_ALL=C /usr/bin/grep -a -c` по нему |
| `ext:N` | `ext-2.1.319-extension.js` | хост расширения VS Code 2.1.319 — запускает `claude` и говорит с ним stream-json; отформатирован из `extension.js.orig` (оригинал, без местной правки `patch-claude-vscode`) |
| `web:N` | `ext-2.1.319-webview.js` | сама вкладка (webview), отформатирована из `webview/index.js.orig` |
| `d312:N`, `d319:N` | `sdk-0.3.312/sdk.d.ts`, `sdk-0.3.319/sdk.d.ts` | официальные типы `@anthropic-ai/claude-agent-sdk` из npm (`claudeCodeVersion` 2.1.312 и 2.1.319); рядом `sdk-tools.d.ts` — схемы инструментов |
| `hqc:…` | `/Users/vvzvlad/Data/Projects/hqc/…` на `9f08283` | SPEC, RUNBOOK и код hqc: живые проверки и сравнение с воркером |

- Главный источник формы сообщений — zod-схема протокола внутри самого CLI (`cli:105050`–`cli:114280`).
  Она шире `sdk.d.ts`: поля `@internal` из d.ts вырезаны (на их месте пустые строки, например
  `d312:4839`–`d312:4842`), а в схеме CLI они есть. Списки полей в части I сняты со схемы скриптом
  `vendor-src/scripts/fields.py` (результат — `scripts/fields.txt`, `scripts/ctlfields.json`);
  `type`, `subtype`, `uuid`, `session_id` в списках опущены — они есть у всех. Пометки: `[i]` —
  `@internal`, `[?]` — необязательное, `[n]` — может быть `null`. Схема описывает форму, но не
  гарантирует, что сообщение уходит в stdout: что доходит, решают фильтры (2.0), это отмечено у
  каждого типа. Все 101 подтип запросов к CLI — `scripts/dispatcher.txt`.
- В хосте расширения две копии читателя истории из SDK: `ZTe` (`ext:57468`, когда задано хранилище
  `ip()`) и `ADn` (`ext:87619`, по умолчанию через `XKe`, `ext:112675`). Нормализованный diff (имена
  → `I`): 36 строк расхождения из 571, только форматирование и одна лишняя функция во второй копии.
  В части II — строки первой копии.
- Счётчики по транскриптам — `scripts/scan.py` и `scripts/scan2.py` (печатают только ключи, типы и
  счётчики) по всем `~/.claude/projects/**/*.jsonl`, кроме `journal.jsonl`: 14 174 файла (1 633
  сессии и 12 541 файл субагентов), 4 380 776 строк, ~13,7 ГБ, ни одна строка не отброшена, битых
  JSON — 0. Результаты — `scripts/scan-all.json`, `scripts/scan2.json`, запросы к ним —
  `scripts/q.py`, `scripts/q2.py`, таблица 7.3 — `scripts/table.py`. Выборка смещена: это ноутбук,
  почти все записи писал VS Code разных версий (2.1.260–2.1.319). Записей с `version: "2.1.312"` —
  68 168, в 67 файлах сессий и 93 файлах субагентов («файлы с 2.1.312» ниже). Транскрипты воркеров
  hqc на нодах сюда не входят — они в 7.7.
- Живые проверки — 35 прогонов на agent-node-227 2026-11-12: CLI 2.1.312, `--model haiku`,
  `-p --input-format stream-json --output-format stream-json --verbose`,
  `CLAUDE_CODE_ENTRYPOINT=claude-vscode`; от вкладки отличались `--strict-mcp-config --mcp-config
  '{"mcpServers":{}}'`, `--permission-mode bypassPermissions` и `--session-id`. Каждый случай снят
  одним прогоном, кроме места эха первого промпта (24 прогона). Ещё 3 прогона `w*` с теми же флагами и
  22 прогона `x*` с флагами воркера hqc (`--permission-prompt-tool stdio`, `--setting-sources
  user,local`, режим `auto` или `default`, sonnet или haiku), запросы прав в них отвечает сам
  прогонщик (`drive.py`, `auto_respond`); макеты вопросов — по три прогона на вариант. Записи —
  `/home/claude/cc-api-probe/captures/<имя>.jsonl` на стенде (рядом `.stderr`, `.times` — мс от
  старта на каждую строку, `.stdin.jsonl` — что и когда отправлено, `.meta.json` — команда и код
  выхода); сценарии — `scen/<имя>.json`, драйвер — `drive.py`. В тексте: «вживую, `<имя>`, строка N».
- Вкладка VS Code 2.1.319 запускает свой встроенный CLI 2.1.319
  (`resources/native-binary/claude` расширения), воркеры hqc — 2.1.312. Поэтому разбор вкладки
  описан для потока 2.1.319, а протокол и запись на диск — по коду 2.1.312.
- «Неизвестное игнорировать»: «Consumers should ignore types and subtypes they do not recognize: the
  set grows over time» (`d312:5591`, `cli:109875`).

## Коротко

Случаи, на которых ловили, — по коду протокола (подробности и строки — разделы 5.3–5.8):

1. **Bash субагента уходит в фон через ~180 с только во вкладке VS Code.** Потолок времени Bash у фонового субагента берётся из серверного флага `tengu_maple_ferry` (по умолчанию 0 — потолка нет), но только когда `WP()` ложна, а `WP()` всегда ложна при `CLAUDE_CODE_ENTRYPOINT=claude-vscode` (`cli:36939`–`cli:36945`, `cli:518285`–`cli:518299`, вызов `cli:587148`, применение `cli:377239`–`cli:377248`, `cli:587511`). Субагент видит «Command was moved to the background (ID: …) after running for Ns» (`cli:518320`). В коде флаг по умолчанию 0, значение приходит с сервера: в кэше флагов (`cachedGrowthBookFeatures.tengu_maple_ferry` в `~/.claude.json`) и на agent-node-227, и на ноутбуке — `180000` мс, отсюда ~180 с.
2. **`task_notification completed` с текстом «This agent has not reported yet…» без `handback`, потом `task_started` того же `task_id`.** Субагент кончил ход, но держит свою фоновую работу (`vyt`, `cli:582087`–`cli:582097`), — это промежуточный отчёт: в текст идёт `Ynr` (`cli:195067`, `cli:727680`), а поле `handback` выбрасывается именно для промежуточного `withheld` (`cli:728549`). Терминальная заявка `terminalEmitClaims` после этого снимается, и при следующем запуске той же задачи CLI выдаёт новый `run_id` и снова `task_started` (`cli:332849`–`cli:332914`). Поэтому одна задача может прислать несколько `task_notification`, и каждый — со своим `run_id`.
3. **Ход закрылся на уведомлении фонового субагента без вызова модели.** Когда отчёт субагента уже доставлен главному агенту вызовом `SubagentHandback` (handback `send`), уведомление о завершении ставится в очередь с `shouldQuery:false, handbackPointer:true` (`cli:582337`, условие `cMt` — `cli:422524`–`cli:422531`, флаг `tengu_hushed_kestrel`, по умолчанию true). Такой ход идёт по ветке без запроса: `result` с `num_turns:0`, `duration_api_ms:0`, `stop_reason:null`, `total_cost_usd` — накопленный итог сессии, а не стоимость хода (`cli:1497509`–`cli:1497553`). Вкладка опознаёт его как `num_turns === 0 && origin.kind === "task-notification"` (`ext:114647`) и не считает законченным ходом.
4. **`--thinking-display summarized`.** Без флага в stream-json режим показа не задаётся вовсе (`KXo`, `cli:144218`–`cli:144223`) и решает API; с флагом размышление приходит кусками `stream_event` с `thinking_delta` (только при `--include-partial-messages`), у `stream_event` и `assistant` есть `thinking_display` (`cli:108096`, `cli:723345`–`cli:723349`). Вкладка передаёт флаг, только если в настройках Claude включено `showThinkingSummaries` (`ext:123928`–`ext:123931`, `ext:129514`–`ext:129525`).
5. **Сообщение посреди хода встаёт после результата инструмента; сообщение во время финального ответа уходит в следующий ход.** Очередь вливается в идущий ход только после того, как отработала пачка вызовов инструментов (после хука `PostToolBatch`) и перед следующим запросом к модели (`cli:762448`–`cli:762527`; документация — «folded into the running turn between tool rounds», `d312:5987`–`d312:5989`). Эхо `isReplay:true` для влитого сообщения рождается в этот момент из вложения `queued_command` (`cli:1489207`–`cli:1489210`, `vAr` — `cli:723351`). Если модель уже пишет последний ответ без вызовов инструментов, следующего запроса в этом ходе нет — сообщение остаётся в очереди и открывает свой ход. Судьбу каждого сообщения с `uuid` сообщает `command_lifecycle` (`queued` → `started` → один из `completed/cancelled/discarded/refused`, `cli:109493`–`cli:109519`).
6. **`/compact`, автокомпакция и `--resume`.** Сжатие: `status:"compacting"` → `compact_boundary` → сводка как `user` (у `/compact` — `isReplay:false`, `isSynthetic:true`, `cli:1489282`–`cli:1489293`). После `--resume` CLI сам рассылает уведомления о задачах, которые прошлый процесс не довёл до конца: фоновые команды — `status:"stopped"` с текстом «Background shell command didn't finish before the previous session ended» (`cli:1353889`–`cli:1353893`, `cli:1354471`–`cli:1354486`).

Ещё граничные случаи, снятые живыми прогонами:

- Ход, который CLI начал сам по уведомлению фоновой задачи, не присылает никакого `user` — даже с
  `--replay-user-messages`; текст уведомления есть только в транскрипте (5.3).
- У «пульса» долгого вызова (`tool_progress`, `heartbeat: true`) в главном потоке `parent_tool_use_id`
  — id самого вызова, а `tool_use_id` — `<id>-heartbeat-N`: признак «это субагент» по
  `parent_tool_use_id` тут ошибётся (2.1).
- Выжимка автосжатия и `/compact` приходят по-разному: у автосжатия `content` — массив и нет ключа
  `isReplay`, эхо промпта — до границы; у `/compact` — строка и `isReplay:false`. Сжатие может не выйти
  (`too_few_groups`), и ход идёт дальше переполненным (5.7).
- `[Request interrupted by user for tool use]` приходит без `isSynthetic` (5.4).
- `priority:"now"` уводит в фон идущий Bash, вызов MCP или субагент и вливает сообщение в тот же ход
  только при `uuid`, `origin.kind:"human"` и идущем вызове; MCP-вызов при этом впервые становится
  задачей `mcp_task` со своим `task_started` и уведомлением. Иначе ход обрывается, во время текста —
  с недописанным текстом в `result` (3.2).
- `end_session` посреди хода — код выхода 1, в простое — 0 (5.9).
- При `claude-vscode` `Agent` уходит в фон и без `run_in_background`; Bash отказывает в `sleep N` с
  N ≥ 25 в переднем плане (5.6).

Транскрипт и вкладка VS Code (подробно — разделы 7–9):

1. Видов записей верхнего уровня в коде 2.1.312 — 41 (две таблицы политик, `cli:409328` и
   `cli:411518`, совпадают по составу). В настоящих файлах встречено 27 видов: 26 из этих 41 и
   `session-owner`, которого в 2.1.312 нет (пишет 2.1.319). В файлах с 2.1.312 — 23 из 41 и
   `session-owner`. Беседу несут `user`, `assistant`, `system`, `attachment` (у них есть
   `uuid`/`parentUuid`; ещё `progress`, но в файлах его нет); остальные 36 — служебные записи
   сессии без места в цепочке.
2. Строку беседы процесс CLI пишет один раз на `uuid`: перед записью сверяет `uuid` с уже
   записанными в этой сессии (`cli:414107`). Исключение — субагенты: их строки идут в свой файл без
   этой проверки. В файлах повторы `uuid` всё же есть (7.5), их причина не проверена.
3. Одно сообщение API пишется несколькими строками `assistant` — по строке на блок содержимого, с
   одним `message.id` и сквозной цепочкой (в файлах с 2.1.312 разбито 16 215 из 23 093 сообщений).
   Строка `user` с `tool_result` ссылается `parentUuid` на строку `assistant`, где был `tool_use`
   (`sourceToolAssistantUUID`, `cli:413847`), а не на предыдущую строку файла.
4. Сжатие пишет `system/compact_boundary` с `parentUuid: null` и `logicalParentUuid` на последнюю
   строку до сжатия, затем вложения и строку `user` с `isCompactSummary: true` и
   `isVisibleInTranscriptOnly: true` — в её `message.content` (строкой) и лежит выжимка. Хвост
   беседы, сохранённый сжатием, остаётся выше границы в файле и пришивается к выжимке только при
   чтении (`compactMetadata.preservedSegment`).
5. Метаданные (`last-prompt`, `ai-title`, `agent-setting`, `mode`, `atis-latch` и др.) CLI
   переписывает в хвост файла заново каждые 32 КиБ записи и на выходе (`cli:412948`,
   `cli:413123`) — поэтому их в файлах сотни тысяч копий.
6. CLI умеет переписать файл целиком, выкинув всё до последней границы сжатия (временный файл +
   rename, `cli:413480`), но в режиме `-p` только при `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1`
   (`cli:793877`); по умолчанию выключено.
7. Вкладка читает файл один раз — при открытии (`loadFromServer`, `web:168861`) через читатель
   SDK в хосте; дальше живёт только потоком своего процесса. Чужие дописки в файл (другой процесс,
   `claude -p --resume`) она не видит, пока вкладку не открыть заново.
8. История и живой поток выглядят во вкладке по-разному: в истории нет строк `system` (в том
   числе границы сжатия: выжимка приходит обычной строкой `user`), нет строк `isMeta`, сообщения,
   взятые посреди хода, превращаются из `attachment/queued_command` в `user` с
   `isQueuedCommand`; в живом потоке выжимка прячется в свёрнутую строку «Compacted chat».
9. В ленте вкладки не больше ~600 строк: старые вытесняются (`web:171897`, `Sq = 600`,
   `web:167672`). Подряд идущие удачные `Read` склеиваются в одну строку «Successfully read N files»
   (`web:163312`).

---

# Часть I. Протокол stdin/stdout

## 1. Запуск

### 1.1 Флаги, с которыми вкладка VS Code запускает CLI

Аргументы собирает `ProcessTransport.initialize` вшитого SDK (`ext:109352`–`ext:109473`) из опций `spawnClaude` (`ext:138007`–`ext:138073`). Всегда первыми идут `--output-format stream-json --verbose --input-format stream-json` (`ext:109396`).

| Флаг | Откуда у вкладки | Что меняет в потоке |
| --- | --- | --- |
| `--output-format stream-json --input-format stream-json --verbose` | всегда, `ext:109396` | построчный JSON в обе стороны; без `--verbose` CLI выходит с «Error: When using --print, --output-format=stream-json requires --verbose» (`cli:1503886`–`cli:1503887`) |
| `--permission-prompt-tool=stdio` | есть `canUseTool`, `ext:109429`–`ext:109433` | вопросы о правах приходят как `control_request can_use_tool` (раздел 4.1); без флага CLI отказывает сам (`permission_denied`), это подтверждено вживую в `hqc:docs/SPEC.md:2242` |
| `--include-partial-messages` | `includePartialMessages: !remoteName` — только не в Remote-окне, `ext:138050`, `ext:109454` | включает `stream_event` (`cli:1489250`) и `status:"requesting"` (`cli:1489167`) |
| `--replay-user-messages` | `extraArgs`, `ext:138069` | эхо `user` с `isReplay:true`, эхо входящих `control_response` и `assistant` (`cli:1512337`–`cli:1512339`, `cli:1512409`–`cli:1512412`), эхо влитых сообщений (`cli:1489207`) |
| `--enable-auth-status` | `extraArgs`, `ext:138067` | сообщения `auth_status` (`cli:1505025`, `cli:1513252`) |
| `--debug --debug-to-stderr` | `extraArgs`, `ext:138065`–`ext:138066` | только stderr |
| `--no-chrome` | `extraArgs`, `ext:138068` | без Claude in Chrome |
| `--safe-mode` | если `CLAUDE_CODE_SAFE_MODE` в переменных пользователя, `ext:112977`–`ext:112979` | вживую (`p13-safe-mode` против `p13-mcp-default`): в `init` нет `mcp_servers` (сервер из `--mcp-config` отброшен) и его инструментов, нет `memory_paths`, `slash_commands` 80 → 57 (без пользовательских скиллов и команд плагинов), `skills` 42 → 22, `agents` 7 → 5; до первого хода — лишний кадр `system/ui_invalidate {event:"ui.render"}` |
| `--setting-sources user,project,local` | `ext:138062`, `ext:109442` | какие файлы настроек читаются |
| `--resume <id>` | при открытии старой сессии, `ext:138019`, `ext:109433` | см. 5.8 |
| `--model`, `--permission-mode`, `--allow-dangerously-skip-permissions`, `--add-dir` | опции вкладки (`ext:138034`), `ext:109416`, `ext:109444`–`ext:109445`, `ext:109457` | модель и режим прав в `system/init` |
| `--thinking=disabled` или `--max-thinking-tokens 31999` | `BQ()`: «off» → disabled, иначе `budgetTokens: 31999` (`ext:123927`–`ext:123931`, перевод в `thinkingConfig` — `ext:112382`–`ext:112391`, флаг — `ext:109401`, `ext:109404`) | бюджет размышления |
| `--thinking-display summarized` | только при `showThinkingSummaries: true` в настройках Claude (`ext:129521`–`ext:129525`; `thinkingSummariesDefaultOn()` — `false`, `ext:129514`) | текст размышления кусками (5.11) |
| `--mcp-config <json>` | если вкладка передала MCP-серверы, `ext:109441` | — |

Чего вкладка **не** передаёт (сравнение для воркера hqc): `--session-mirror` (`ext:109455`; без него нет `transcript_mirror`, `cli:1504868`–`cli:1504871`), `--include-hook-events` (`ext:109453`; без него из hook-событий идут только `SessionStart`/`Setup` и, по переменной окружения, `UserPromptSubmit`, `cli:436627`–`cli:436633`), `--permission-prompts`, `--strict-mcp-config`, `--session-id`.

### 1.2 Переменные окружения

Окружение собирают `Go()` (`ext:139558`–`ext:139580`), `spawnClaude` (`ext:138080`–`ext:138088`) и SDK (`ext:112343`–`ext:112360`).

| Переменная | Значение у вкладки | Что меняет в CLI |
| --- | --- | --- |
| `CLAUDE_CODE_ENTRYPOINT` | `claude-vscode` (`ext:139574`; SDK ставит `sdk-ts`, только если пусто — `ext:112343`, `ext:109476`) | `$m()` истинна (`cli:36939`–`cli:36942`) → `WP()` ложна (`cli:36943`–`cli:36946`) → у фоновых субагентов включается потолок Bash из `tengu_maple_ferry` (случай 1); пишется в транскрипт как `entrypoint`, и по нему вкладка показывает сессию в своей истории (подтверждено вживую на 2.1.309, `hqc:docs/RUNBOOK.md:858`–`hqc:docs/RUNBOOK.md:861`) |
| `CLAUDE_CODE_SDK_READS_SESSION_STATE` | `1` (`ext:112346`–`ext:112347`) | CLI шлёт `session_state_changed` с `sdk_host_only:true` (`cli:390593`–`cli:390596`); SDK вкладки такие кадры глотает (`ext:110294`) |
| `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` | не ставится | без `sdk_host_only` — кадры видны клиенту (`cli:390593`) |
| `CLAUDE_CODE_ENABLE_TASKS` | `0` (`ext:139563`) | `gM()` ложна (`cli:374072`–`cli:374075`): у модели старый `TodoWrite` (`cli:743559`) вместо четырёх инструментов списка задач (`cli:746996`) |
| `CLAUDE_CODE_EMIT_STARTUP_TIMING` | `1` (`ext:139564`) | поле `startup_timing[i]` в `system/init` (`cli:107940`) |
| `CLAUDE_CODE_STARTUP_FAILURE_RESULTS` | `1` (`ext:139565`) | при известной ошибке старта CLI пишет нулевой `result error_during_execution` со `startup_failure_reason` (`d312:5981`) |
| `MCP_CONNECTION_NONBLOCKING` | `true` (`ext:139562`) | по коду без переменной режим уже неблокирующий (`cli:801525`). Вживую (`p13-*`, MCP-сервер, отвечающий через 8 и 40 с): первый ход в `-p` ждёт подключения MCP при любом значении — до ~27 с, потом сервер в `init.mcp_servers` со `status:"failed"`, повторной попытки во втором ходе нет; переменная меняет только момент чтения stdin — `command_lifecycle queued` через 0,7 с против 5,8 с при `false`. Откуда ~27 с, в коде не найдено (`MCP_CONNECT_TIMEOUT_MS` по умолчанию 5000, `MCP_TIMEOUT` 30000, `cli:221653`–`cli:221660`) |
| `CLAUDE_CODE_EMIT_USER_PROMPT_SUBMIT_HOOK_EVENTS` | `1`, если гейт `tengu_optimized_puzzle` не `false` (`ext:138081`–`ext:138083`) | `hook_started`/`hook_response` для `UserPromptSubmit` (`cli:436708`, `cli:436631`) |
| `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING` | `true` (`enableFileCheckpointing`, `ext:138048`, `ext:112345`) | снимки файлов для `rewind_files` |
| `CLAUDE_CODE_QUESTION_PREVIEW_FORMAT` | `markdown` (`toolConfig.askUserQuestion`, `ext:138063`, `ext:112354`) | формат макетов у вариантов `AskUserQuestion`; в `-p` с `claude-vscode` вживую на макеты не влияет: с просьбой о макетах — у всех вариантов в 3 из 3 прогонов и с ней, и без неё, без просьбы — 0 из 3 и там, и там (`x5-*`, `x5n-*`, sonnet) |
| `CLAUDE_CODE_RESUME_INTERRUPTED_TURN`, `…_MAX_AGE_MS=3600000` | только при повторном запуске после сбоя (`IJe`, `ext:132857`–`ext:132864`) | CLI сам перезапускает прерванный ход; у его `result` есть `resume_reason` (`d312:5993`) |
| `DISABLE_TELEMETRY` | `1`, если в VS Code выключена телеметрия (`ext:137228`–`ext:137230`) | без метрик и флагов с сервера — тогда серверные гейты берут значения по умолчанию |
| `CLAUDE_AGENT_SDK_VERSION` | `0.3.319` (`ext:112344`) | — |
| удаляются | `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `TRACEPARENT`, `TRACESTATE`, `NODE_OPTIONS` (`ext:139575`–`ext:139578`, `ext:109477`) | иначе CLI счёл бы себя дочерним (`$m()` требует `!childSession && !claudecode`) |

### 1.3 Запрос `initialize`, который шлёт вкладка

Первым кадром на stdin SDK шлёт `control_request initialize` (`buildInitializeRequest`, `ext:110591`–`ext:110642`). Поля, которые вкладка реально заполняет (`ext:138047`–`ext:138063`), и что они делают в CLI:

- `hooks` (`ext:138055`–`ext:138060`) — свои `PreToolUse` (`Edit|Write|MultiEdit`, `Edit|Write|Read`) и `PostToolUse`; каждому колбэку присваивается `hook_N`, CLI потом зовёт его запросом `hook_callback` (раздел 4.2). Проверка формы — `cli:1508863`–`cli:1508886`, ошибка — «initialize: hooks must map hook events to arrays of matchers…».
- `perTaskStopAffordance: true` — при `interrupt` CLI **не** останавливает фоновых субагентов и workflow (`cli:1504650`–`cli:1504653`, `cli:1513148`, описание `cli:110014`). Без этого поля прерывание убивает их (`Kle`, `cli:742336`–`cli:742345`).
- `forwardNestedSubagentToolCalls: true` — в поток идут `tool_use`/`tool_result` субагентов второго уровня с `parent_tool_use_id` (`cli:110019`, `cli:1513150`).
- `systemPrompt: {preset: "claude_code", append}`, `supportedDialogKinds` (виды `request_user_dialog`, которые вкладка умеет рисовать; без объявления CLI диалог не шлёт, `cli:110004`), `agentProgressSummaries`/`promptSuggestions` — `undefined` у вкладки (`ext:138051`–`ext:138052`).
- `rapidFollowupPreempt` вкладка не ставит: без него сообщение, пришедшее до первого вывода хода, ждёт своей очереди, а не прерывает ход (`cli:110024`).

Ответ на `initialize` — `success` с `commands`, `agents`, `output_style`, `models`, `account`, `pid` и т. п. (схема `cli:110094`), плюс `pending_permission_requests` и `pending_user_dialog_requests` — незакрытые вопросы этого процесса (`d312:418`, `d312:422`). Ошибка формы — `error` с текстом `initialize: …` (`cli:1508851`, `cli:1508856`, `cli:1508890`). Повторный `initialize` не отвергается: «First-attached-client-wins… later initializes do not change it» (`cli:110014`, `cli:110019`).

### 1.4 Для сравнения: воркер hqc

Воркер hqc запускает CLI сам (флаги — `hqc:docs/SPEC.md:2211`–`hqc:docs/SPEC.md:2217`) и шлёт только три control-запроса — `interrupt`, `set_permission_mode`, `set_model` (`hqc:crates/hqc-worker/src/adapter/claude_code/engine.rs:390`–`hqc:crates/hqc-worker/src/adapter/claude_code/engine.rs:417`); `initialize` не шлёт. Следствия по коду CLI: `perTaskStopAffordance` не объявлен, поэтому `interrupt` от hqc убивает фоновых субагентов (`cli:1504653`); `session_state_changed` нет, пока в окружении не стоит одна из двух переменных выше; `CLAUDE_CODE_ENTRYPOINT=claude-vscode` воркер передаёт по настройке `show_in_vscode` (`hqc:docs/SPEC.md:4784`), и тогда действует случай 1.

## 2. CLI → клиент: сообщения stdout

### 2.0 Как сообщение попадает в stdout

- **Одна строка — один JSON.** Всё, что уходит в stdout, — член `StdoutMessage`: `SDKMessage`, `SDKActiveGoalMessage`, `control_response`, `control_request`, `control_cancel_request`, `keep_alive` (`d312:9980`).
- **Два пути.** (1) События цикла запроса идут через генератор движка: он отдаёт сырое событие и, где нужно, его «двойника» в форме SDK (`yield-twin`, `cli:768700`–`cli:768850`), а затем фильтр `RC` в `print.ts` решает, что пропустить (`cli:1489154`–`cli:1489276`). (2) Остальное кладётся в очередь вывода напрямую через `ea()` (задачи, хуки, состояние сессии, `command_lifecycle`, `rate_limit_event` и т. д.) и фильтр `RC` не проходит.
- **Что `RC` пропускает из цикла запроса:** `assistant` (без готового `parent_tool_use_id`, `cli:1489157`–`cli:1489158`), `user` и `progress` (через конвертер `Xge`, `cli:1489160`–`cli:1489162`, `cli:723739`), из `system` — только `status` (`requesting` — лишь с `--include-partial-messages`), `model_refusal_fallback` (без внутреннего `provisional`), `init`, `notification`, `api_retry`, `model_refusal_no_fallback`, `memory_recall`, `thinking_tokens`, `compact_boundary`, `model_fallback`, `model_consent_fallback`, `informational` (`cli:1489166`–`cli:1489200`); `tool_use_summary`, вложения `queued_command` (как эхо), `result`, `stream_event` (только с partial). **Все прочие `system`-подтипы цикла отбрасываются** (`default: return`, `cli:1489201`–`cli:1489202`), а сырые типы `tombstone`, `stream_request_start`, `tool_progress`, `command_lifecycle`, `auth_status`, `prompt_suggestion`, `rate_limit_event`, `sdk_status`, `compact_progress`, `post_turn_summary`, `active_goal`, `conversation_reset` и др. — тоже (`cli:1489252`–`cli:1489275`); их SDK-формы, если есть, приходят по пути (2).
- **Порядок относительно `result`.** `result` — один на ход, после всех `assistant`/`user`/`stream_event` хода; информационные `system` (уведомления о задачах, состояние сессии, подсказки) могут прийти после него (`d312:6013`, текст над `SDKResultMessage`). Что `print.ts` считает «последним сообщением хода», решает `cV` (`cli:1503369`–`cli:1503425`): `control_*`, `session_state_changed`, `permission_denied`, все `task_*`, `background_tasks_changed`, хуки, `stream_event`, `keep_alive`, `prompt_suggestion`, `conversation_reset`, `transcript_mirror`, `command_lifecycle`, `active_goal`, `autocompact_state` и ещё ~20 подтипов в этот счёт не входят.
- **Неизвестное игнорировать.** «Consumers should ignore types and subtypes they do not recognize: the set grows over time» (`d312:5591`, `cli:109875`).

### 2.1 Члены union `SDKMessage` (0.3.312: 39 членов, `d312:5594`)

Форма записи: «Когда», «Поля», «Граничные случаи», «Расширение». Нет пункта «Граничные случаи» — отдельных веток для типа сверх сказанного в «Когда» я не искал. Нет пункта «Расширение» — хост вкладки пересылает тип в webview без обработки (`io_message`, `ext:125715`). Исключения у хоста: `hook_*` кроме `SessionStart`/`Setup` выбрасываются (`ext:123961`), `bridge_state` и `session_title_changed` обрабатываются и не пересылаются (`ext:125642`–`ext:125652`, `ext:125708`–`ext:125712`); на уровне SDK проглатываются `keep_alive`, `transcript_mirror`, `session_state_changed` с `sdk_host_only` и все `control_*` (`ext:110214`–`ext:110294`).

### `assistant`
- **Когда:** каждый ответ модели. Конвертер `Xge` режет сообщение на отдельные `assistant` — по одному на блок содержимого (`for (let y of km([e]))`, `cli:723743`; разбиение `lTs`, `cli:588845`–`cli:588852`; `Xge` импортирует `km` из модуля `cli:394416`–`598147`, где она определена в `cli:588808`, `cli:721664`). Ответ субагента идёт с `parent_tool_use_id` вызова `Agent`.
- **Поля** (`cli:107133`): `message`, `parent_tool_use_id[n]`, `error[?]`, `request_id[?]`, `supersedes[?]`, `aborted[?]`, `user_message_uuid(s)[?]`, `resume_reason[?]`, `agent_id[?]`, `subagent_type[?]`, `task_description[?]`, `timestamp[?]`, `context_usage[?]`, `usage_report[?]`, внутренние `thinking_display`, `thinking_duration_ms`, `tool_use_meta`, `is_api_error_message`, `api_error*`, `local_command_*`, `attribution_*` и др. (всего 47).
- **Граничные случаи:** ошибка API приходит как `assistant` с `error` и текстом ошибки, а `result` при этом `success` с `is_error:true` (`cli:768983`–`cli:768990`). Вывод локальной слэш-команды тоже приходит «двойником» `assistant` (`Mtn`, `cli:768813`–`cli:768821`). От субагентов в поток идут вызовы и результаты инструментов, а их текст и размышления — только при `initialize.forwardSubagentText` (`zHt`, `cli:725265`–`cli:725272`; для субагентов второго уровня — `Tje`, `cli:724979`–`cli:724986`; условие `s` в `zHt` не прослежено).
- **Расширение:** пересылает в webview как есть (`io_message`, `ext:125715`); правки `Edit/Write` считает принятыми по паре `tool_use` → `tool_result` (`IVe`, `ext:123951`).

### `user`
- **Когда:** результаты инструментов (`tool_result` + `tool_use_result`), синтетические сообщения цикла (`isSynthetic`), сообщения субагентов с `parent_tool_use_id`.
- **Поля** (`cli:106665`, `cli:106813`): `message`, `parent_tool_use_id[n]`, `isSynthetic[?]`, `tool_use_result[?]`, `priority[?]`, `origin[?]`, `shouldQuery[?]`, `timestamp[?]`, `client_composed[?]`, `pasted_content[?]`, `inline_pastes[?]`, `agent_id[?]`, `subagent_type[?]`, `task_description[?]` и 20 внутренних.
- **Граничные случаи:** вызов, уступивший место сообщению пользователя (сейчас `WebFetch`/`WebSearch`), несёт `tool_use_result: {detachedToolCall: true}` — вызов ещё идёт (`d312:6569`–`d312:6572`).
- **Расширение:** пересылает как есть (`ext:125715`).

### `user` с `isReplay: true` (`SDKUserMessageReplay`)
- **Когда:** только с `--replay-user-messages`. Три источника: (а) повторный кадр с тем же `uuid` — CLI его не исполняет, а отвечает эхом (`cli:1512487`–`cli:1512498`); (б) сообщение из очереди, влитое в идущий ход, — эхо рождается из вложения `queued_command` в момент вливания (`vAr`, `cli:723351`–`cli:723364`, `cli:1489207`–`cli:1489210`); (в) эхо локальной команды и сводки сжатия (`CC`, `cli:1489282`–`cli:1489293`): у сводки `isReplay:false`.
- **Поля** (`cli:106845`): всё от `user` + `uuid`, `session_id`, `isReplay: true`, `file_attachments[?]`; у эха из очереди ещё `isSynthetic` (если команда служебная) и `origin`.
- **Граничные случаи:** эхо первого промпта хода приходит не на `command_lifecycle started`, а прямо перед первым `stream_event message_start` (без partial — перед первым `assistant`), через 1,1–3,6 с после `started`; у локальной команды — после её `assistant`, перед `result` (вживую, 24 прогона, `p7-first-echo` и др.). `uuid` эха — присланный; у кадра без `uuid` — тот, что CLI дал записи в транскрипте, а `command_lifecycle` у такого хода нет и в `result` нет `user_message_uuid(s)`. Без `--replay-user-messages` эха нет. В коде: кандидаты на эхо — `Fi = mA(Er)` в генераторе хода (`cli:1497448`; `mA` берёт `user` без `toolUseResult`, не служебные, и `compact_boundary`, `cli:1495783`–`cli:1495789`), форма — `gA` (`cli:1495817`–`cli:1495831`, `uuid` — тот, что пишется в транскрипт, `isSynthetic: uXe(e)`); выпуск — лениво, один раз, перед первым `assistant`, `user`, `compact_boundary` или не-`ping` `stream_event` (`cli:1497664`–`cli:1497667`, `cli:1498005`–`cli:1498011`), а в ходе без модели (локальная команда, `shouldQuery:false`) — сразу после `system/init` (`cli:1497514`). Перед `status requesting` эхо выходит только на удалённом транспорте. `user_message_uuids` хода несут ещё `system/init`, `system/status requesting`, `stream_event`, `assistant`, `result`; `system/thinking_tokens` — `user_message_uuid` первого сообщения хода и во второй итерации.
- **Расширение:** пересылает как есть.

### `result/success`
- **Когда:** конец каждого хода, включая ход без модели (`cli:1497509`–`cli:1497553`) и локальную команду.
- **Поля** (`cli:107485`, 89 полей): `duration_ms`, `duration_api_ms`, `is_error`, `num_turns`, `result`, `stop_reason[n]`, `total_cost_usd`, `usage`, `modelUsage`, `permission_denials`, `queued_turn_count[?]`, `structured_output[?]`, `deferred_tool_use[?]`, `terminal_reason[?]`, `result_index[?]`, `user_message_uuid(s)[?]`, `resume_reason[?]`, `origin[?]`, `subagent_stats[?]`, `safety_stops[?]`, `fast_mode_state[?]`, `api_error_status[?n]`, тайминги `ttft_ms`, `first_*_ms` и ~50 внутренних метрик.
- **Граничные случаи:** `total_cost_usd` и `modelUsage` — накопленные с начала процесса, читать последнее, не складывать (`d312:5958`–`d312:5962`); `usage` — только главный цикл этого хода. `success` бывает с `is_error:true`, когда ход кончился ошибкой API (`cli:768983`–`cli:768990`). При отложенном вызове инструмента `RC` переписывает его в `is_error:false, stop_reason:"tool_deferred", result:""` (`cli:1489233`–`cli:1489244`). `queued_turn_count > 0` — после этого результата без нового ввода придёт ещё ход (`d312:5975`). `terminal_reason` — 19 значений (`d312:10128`), отсутствует у локальной команды (`HA`, `cli:107397`–`cli:107399`).
- **Расширение:** `lv()` выделяет ход без модели (`num_turns === 0 && origin.kind === "task-notification"`, `ext:114647`) и не считает его законченным ходом (`ext:125674`–`ext:125677`); SDK помечает `modelLessTurnEnded` (`ext:110277`).

### `result/error_during_execution`, `result/error_max_turns`, `result/error_max_budget_usd`, `result/error_max_structured_output_retries`
- **Когда:** `error_max_budget_usd` — исчерпан `--max-budget-usd` (`cli:768946`–`cli:768951`); `error_max_turns` — `--max-turns` (`cli:768954`–`cli:768963`); `error_during_execution` — исключение в ходе, сбой подготовки (`terminal_reason:"turn_setup_failed"`, `cli:768540`–`cli:768552`), ход, прерванный до проверки промпта (`cli:1497541`–`cli:1497545`), нулевой результат при известной ошибке старта (`d312:5981`); `error_max_structured_output_retries` — `--json-schema` (`cli:1497873`, `cli:1498081`).
- **Поля** (`cli:107795`): как у `success`, но вместо `result` — `errors: string[]`, плюс `startup_failure_reason[?]`.
- **Граничные случаи:** прерванный ход (`terminal_reason` `aborted_streaming` или `aborted_tools`) как правило закрывается `error_during_execution` с описанием в `errors`; если последнее сообщение хода признано законченным (`mIs`), — `success` (`cli:768891`–`cli:768900`). Квитанция `interrupt` приходит раньше этого результата (`d312:4848`, абзац «Ordering»).
- **Расширение:** SDK запоминает текст ошибки (`ext:110261`–`ext:110276`).

### `system/init` (`SDKSystemMessage`)
- **Когда:** в начале **каждого** хода, включая ход без модели: `yield HA(...)` в обработчике хода (`cli:1497286`, `cli:1497497`), сборка — `npt` (`cli:747367`).
- **Поля** (`cli:107940`): `cwd`, `tools`, `mcp_servers[?]`, `model`, `permissionMode`, `slash_commands`, `output_style`, `skills`, `agents[?]`, `apiKeySource`, `betas[?]`, `claude_code_version`, `capabilities[?]`, `fast_mode_state[?]`, `effort[?n]`, `plugin_errors[?]` и внутренние `startup_timing`, `memory_paths`, `scratchpad_path`, `worker_epoch` и т. д.
- **Граничные случаи:** `capabilities` у 2.1.312 — `interrupt_receipt_v1`, `interrupt_cancel_queued_v1`, `interrupt_send_now_v1`, `interrupt_requeue_taken_v1`, `msg_lifecycle_v1`, `request_marker_lists_v1`, `ask_user_question_unanswered_v1` плюс условные (`cli:747283`–`cli:747310`, список `Fks` — `cli:747310`). Режим `manual` показывается как `"default"` (подтверждено вживую, `hqc:docs/SPEC.md:2301`–`hqc:docs/SPEC.md:2303`).
- **Расширение:** по `init` фиксирует `session_id` сессии (`ext:125626`–`ext:125629`).

### `stream_event` (`SDKPartialAssistantMessage`)
- **Когда:** только с `--include-partial-messages` (`cli:1489249`–`cli:1489250`): сырые события Messages API по мере генерации.
- **Поля** (`cli:108096`): `event`, `parent_tool_use_id[n]`, `ttft_ms[?]`, `user_message_uuid(s)[?]`, `resume_reason[?]`, внутренние `thinking_display`, `api_message_id`, `abandoned_blocks`, `text_runs_joined`.
- **Граничные случаи:** куски размышления (`thinking_delta`) идут только при показе `summarized` (5.11).
- **Расширение:** вкладка включает partial только вне Remote-окна (`ext:138050`).

### `system/compact_boundary`
- **Когда:** граница сжатия контекста, и при `/compact`, и при автокомпакции (`cli:768744`, `cli:1489317`–`cli:1489324`).
- **Поля** (`cli:108153`): `compact_metadata[i?]` (триггер, токены до), `logical_parent_uuid[i?n]`, `historical[i?]`.
- **Граничные случаи:** см. 5.7.
- **Расширение:** пересылает как есть.

### `system/status`
- **Когда:** `status:"compacting"` в начале сжатия, `null` в конце (`sdk_status`, `cli:768731`–`cli:768738`); `status:"requesting"` перед каждым запросом к API — только с partial (`cli:768740`, `cli:1489167`); `status:null, permissionMode` — при смене режима прав (`Rd`, `cli:1504860`–`cli:1504866`).
- **Поля** (`cli:108222`): `status` (`compacting` | `requesting` | `null`, `cli:106504`), `permissionMode[?]`, `compact_result[?]` (`success`/`failed`), `compact_error[?]`, `compact_output_chars[i?]`.
- **Граничные случаи:** `compact_output_chars` бывает только у автокомпакции, у набранного `/compact` его нет (`cli:108230`–`cli:108234`).
- **Расширение:** само подмешивает в webview `status` с `userPromptSubmitHooksRunning`, которого CLI не шлёт (`ext:125659`–`ext:125664`).

### `system/api_retry`
- **Когда:** запрос к API упал с ошибкой, которую CLI повторит (двойник `api_error`, `cli:768796`–`cli:768805`). Второй источник — «пульс» ожидания лимита для запросов не главного потока: `k$` собирает `api_retry` с `error_status:429, error:"rate_limit"` (`cli:1502937`–`cli:1502955`, вызов `cli:1502915`, гейт `tengu_rustling_sunset`, на gateway выключен).
- **Поля** (`cli:108426`): `attempt`, `max_retries`, `retry_delay_ms`, `error_status[n]` (`null` — нет HTTP-ответа), `error`, `no_response[?]` (`waited_ms`, `retry_wait_ms`).
- **Граничные случаи:** повторы внутри субагента сюда не попадают — они приходят как `tool_progress` с `subagent_retry` (`cli:723781`).
- **Расширение:** пересылает как есть.

### `system/control_request_progress`
- **Когда:** ход долгого control-запроса клиента; сейчас только `side_question` (`cli:1508441`, `d312:5251`).
- **Поля** (`cli:108446`): `request_id`, `status` (`started` | `api_retry`), счётчики повтора.

### `system/model_refusal_fallback`
- **Когда:** модель отказала, и CLI переключился на запасную модель (`cli:768750`–`cli:768768`).
- **Поля** (`cli:108462`): `trigger:"refusal"`, `direction`, `scope[?]`, `original_model`, `fallback_model`, `request_id[n]`, `api_refusal_category[?n]`, `api_refusal_explanation[?n]`, `retracted_message_uuids[?]` (какие уже показанные сообщения отозваны), `refused_user_message_uuid[?n]`, `content`.
- **Граничные случаи:** `RC` выкидывает внутреннее `provisional` (`cli:1489169`–`cli:1489171`).

### `system/model_refusal_no_fallback`
- **Когда:** отказ без запасной модели (`cli:768769`–`cli:768779`). **Поля** (`cli:108511`): `original_model`, `request_id[n]`, `api_refusal_*`, `refused_user_message_uuid[?n]`, `content`.

### `system/local_command_output`
- **Когда:** схема есть (`cli:108697`), отправителя нет ни в разобранном коде, ни как строки-литерала в бинаре (`"local_command_output"`: 2 вхождения в `cli-2.1.312.pretty.js` и 2 в сыром `~/.local/share/claude/versions/2.1.312`, оба — схема и разбор у приёмника `cli:1340689`). Вывод локальной команды в 2.1.312 приходит как `assistant` (`cli:768813`–`cli:768821`). Вживую (`p4-local-commands`): на `/cost`, `/context`, `/model`, `/usage` — 0 кадров `local_command_output`; каждая отвечает тройкой `assistant` (`stop_reason: end_turn`, текст вывода) → эхо `user` с `<command-name>…` → `result success` с `num_turns:0`, `duration_api_ms:0`. `/cost` в 2.1.312 — синоним `/usage`. Вывод `/compact` — не `assistant`, а эхо `user` с `<local-command-stdout>Compacted </local-command-stdout>` (`p10-autocompact`, строка 70).

### `system/hook_started`, `system/hook_progress`, `system/hook_response`
- **Когда:** запуск, вывод и конец хука (`cli:436634`–`cli:436710`). Без `--include-hook-events` — только для `SessionStart` и `Setup` (`T6s`, `cli:333615`); `UserPromptSubmit` — при `CLAUDE_CODE_EMIT_USER_PROMPT_SUBMIT_HOOK_EVENTS` (`cli:436631`–`cli:436633`, `cli:436708`).
- **Поля:** `hook_id`, `hook_name`, `hook_event` (`cli:108702`); у `progress` ещё `stdout`, `stderr`, `output` (`cli:108713`); у `response` — `output`, `stdout`, `stderr`, `exit_code[?]`, `outcome` (`cli:108727`).
- **Расширение:** в webview пропускает только события `SessionStart`/`Setup` (`DVe`, `ext:123961`; `b3`, `ext:50679`); незакрытые `UserPromptSubmit` считает сам (`LVe`, `ext:123968`).

### `system/plugin_install`
- **Когда:** только при `CLAUDE_CODE_SYNC_PLUGIN_INSTALL` и stream-json (`cli:1506589`–`cli:1506600`). **Поля** (`cli:108743`): `status`, `name[?]`, `error[?]`.

### `tool_progress`
- **Когда:** (а) «пульс» долгого вызова инструмента главного потока — каждые 30 с (`cVr = 30000`, `cli:550261`; таймер — `kNr`, `cli:550262`; у субагентов не заводится — `s.agentId ? () => {} : kNr(...)`, `cli:552244`–`cli:552246`), `heartbeat: true` (`cli:723768`–`cli:723778`); (б) повтор API внутри субагента — `subagent_retry` (`cli:723779`–`cli:723790`).
- **Поля** (`cli:108756`): `tool_use_id`, `tool_name`, `parent_tool_use_id[n]`, `elapsed_time_seconds`, `task_id[?]`, `heartbeat[?]`, `subagent_type[?]`, `subagent_retry[?n]`.
- **Граничные случаи:** у «пульса» `tool_use_id` на проводе — `<id вызова>-heartbeat-N` (`cli:550277`), а `parent_tool_use_id` — id самого вызова, хотя это главный поток, не субагент: признак «это субагент» по `parent_tool_use_id` здесь ошибётся (вживую, `p3p5-bash-heartbeat`, строки 33–34: `toolu_…-heartbeat-0` на 30 с, `-heartbeat-1` на 60 с). Прогресс Bash главного потока (`bash_progress`) движок отдаёт только «двойником» `tool_progress`, который `RC` отбрасывает (`cli:768822`–`cli:768831`, `cli:1489254`): вживую Bash, печатавший строку каждые 5 с 70 с подряд, дал только два «пульса», вывод — одним `tool_result` в конце. У Bash субагента «пульсов» нет (32 с, `t2-subagent-progress`, 0 кадров).

### `auth_status`
- **Когда:** только с `--enable-auth-status` (вкладка его передаёт): ход входа в аккаунт (`cli:1505025`, `cli:1513252`). **Поля** (`cli:108778`): `isAuthenticating`, `output`, `error[?]`.

### `system/task_notification`
- **Когда:** задача (фоновая команда, субагент, монитор, workflow) пришла в конечное состояние. Отправитель `ca()` выпускает уведомление один раз на «терминальную заявку» задачи (`s8r`, `cli:138697`–`cli:138700`; `ca`, `cli:138872`–`cli:138892`); переход в конечное состояние в реестре вызывает его сам (`cli:332785`–`cli:332814`).
- **Поля** (`cli:108807`): `task_id`, `run_id[?]`, `tool_use_id[?]`, `status` (`completed`/`failed`/`stopped`), `reason[?]`, `output_file`, `summary`, `usage[?]`, `resource_links[?]`, внутренние `handback` (`send`/`flagged`/`withheld`, `cli:104998`) и `handback_report`, `skip_transcript[?]`, `ambient[?]`.
- **Граничные случаи:** одна задача может прислать несколько уведомлений — после промежуточного отчёта заявка снимается, и следующий запуск получает новый `run_id` (5.6). `handback_report` надо показывать вместо `summary`; у промежуточного `withheld` поля `handback` нет (`cli:108828`, `cli:728549`). Субагент, остановленный прерыванием без `perTaskStopAffordance`, получает `stopped` с `summary` = описание (`cli:742344`). Отчёт через `SubagentHandback` включается в режиме прав `auto`, если его не выключает `CLAUDE_CODE_SENDMESSAGE_HANDBACK` (по умолчанию флаг `tengu_lively_waffle` = `true`, `cli:194998`–`cli:195005`); вживую (`x1-handback`, `auto`): `handback: send`, `summary` — «This agent's report was delivered to you as a message from "…" (its SubagentHandback call). Read it there; it is not repeated here.», `handback_report: {text}` — сам отчёт; следом CLI сам открывает ход, и агент пересказывает отчёт.
- **Расширение:** пересылает как есть.

### `system/task_started`
- **Когда:** задача зарегистрирована (`vo`, `cli:332849`–`cli:332914`) — и **повторно** для того же `task_id`, если после терминальной заявки задача снова ожила (`cli:332850`–`cli:332851`, `cli:332885`–`cli:332895`).
- **Поля** (`cli:108844`): `task_id`, `run_id[?]`, `tool_use_id[?]`, `description`, `subagent_type[?]`, `is_backgrounded[?]`, `awaited[i?]`, `spawn_depth[?]`, `parent_task_id[?]`, `task_type[?]`, `workflow_name[?]`, `prompt[?]`, `skip_transcript[?]`, `ambient[?]`. На проводе есть ещё `owned_by_subagent` (`true` у Bash, запущенной субагентом, `cli:332830`–`cli:332833`, `cli:332899`) — его нет ни в схеме, ни в d.ts.
- **Граничные случаи:** задачи с `skip_transcript`/`ambient` не показывать в ленте (`d312:6410`–`d312:6418`).

### `system/task_updated`
- **Когда:** изменилось одно из полей задачи (`cli:332809`–`cli:332812`). **Поля** (`cli:108881`): `task_id`, `run_id[?]`, `patch` — только изменившиеся `status`, `description`, `end_time`, `total_paused_ms`, `error`, `is_backgrounded` (`Ao`, `cli:332815`–`cli:332828`).
- **Граничные случаи:** перевод вызова в фон приходит как `patch.is_backgrounded:true` (`d312:6390`).

### `system/task_progress`
- **Когда:** ход субагента (`aye`, `cli:581928`–`cli:581945`). **Поля** (`cli:109274`): `task_id`, `run_id[?]`, `tool_use_id[?]`, `description`, `subagent_type[?]`, `usage`, `last_tool_name[?]`, `summary[?]`.
- **Граничные случаи:** «Summaries are not a heartbeat» — пауза в сводках не значит, что субагент встал (`d319:2162`).

### `system/background_tasks_changed`
- **Когда:** изменился список фоновых задач (`cli:770640`–`cli:770646`, `cli:770853`–`cli:770854`). **Поля** (`cli:108901`): `tasks[?]` — снимок списка.
- **Расширение:** число не-ambient задач (`AVe`, `ext:123912`) решает, считать ли сессию «в работе» (`FQ`, `ext:123862`–`ext:123881`).

### `system/thinking_tokens`
- **Когда:** оценка числа токенов размышления по ходу потока (`cli:768702`–`cli:768713`). **Поля** (`cli:109294`): `estimated_tokens`, `estimated_tokens_delta`, `user_message_uuid[?]`.

### `system/session_state_changed`
- **Когда:** смена состояния `idle` / `running` / `requires_action`, но только если в окружении `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` (кадр для клиента) или `CLAUDE_CODE_SDK_READS_SESSION_STATE` (тот же кадр с `sdk_host_only:true`) (`cli:390593`–`cli:390595`).
- **Поля** (`cli:108945`): `state`; на проводе ещё `sdk_host_only` (в схеме его нет).
- **Расширение:** SDK использует для конца прогона и проглатывает кадр с `sdk_host_only` (`ext:110287`–`ext:110294`).

### `system/worker_shutting_down`
- **Когда:** воркер моста Remote Control v2 завершается (конструктор `$5o`, `cli:320733`–`cli:320735`): оба вызова пишут в транспорт моста (`cli:1064984`, `cli:1066437`), не в stdout. **Поля** (`cli:108969`): `reason`. В локальном `-p` не приходит.

### `system/commands_changed`
- **Когда:** изменился список слэш-команд (`cli:1507278`–`cli:1507283`). **Поля:** `commands`.
- **Расширение:** обновляет кэш команд (`ext:125682`–`ext:125690`).

### `system/notification`
- **Когда:** уведомление интерфейса из цикла (`cli:768716`–`cli:768726`) и других мест (`cli:1486975`, `cli:1487048`). **Поля** (`cli:109259`): `key`, `text`, `priority`, `color[?]`, `timeout_ms[?]`.

### `system/files_persisted`
- **Когда:** отправитель не найден: в разобранном коде только схема (`cli:108788`) и списки подтипов (`cli:1250325`, `cli:1439090`, `cli:1503394`); строка `"files_persisted"` в сыром бинаре — 4 раза, как и в разобранном; в 33 живых прогонах на стенде — 0 кадров. **Поля:** `files`, `failed`, `processed_at`.

### `tool_use_summary`
- **Когда:** краткая сводка группы вызовов инструментов (`mzs`, `cli:593896`; двойник — `cli:768791`). **Поля** (`cli:109311`): `summary`, `preceding_tool_use_ids`.

### `system/memory_recall`
- **Когда:** вложение `relevant_memories` в цикле (`cli:766795`–`cli:766802`, двойник `cli:768807`–`cli:768812`). **Поля** (`cli:109325`): `mode`, `memories[?]`. Условие — `ef() && mie()`, где `mie()` — флаг `tengu_moth_copse` или непустая `CLAUDE_MEMORY_STORES` (`B2s`, `cli:527312`–`cli:527345`; `cli:126961`–`cli:126966`). Вживую: с одними файлами памяти проекта кадра нет (флаг на стенде `false`, `p6-memory-default`); с `CLAUDE_MEMORY_STORES` — `{mode:"select", memories:[{path, scope:"personal"}]}` во второй итерации хода, между `tool_result` и следующим `status requesting`, а в `init.memory_paths` появляется ключ `team` (`p6-memory-stores`, строка 65).

### `rate_limit_event`
- **Когда:** слушатель `NR` (`cli:1486662`–`cli:1486710`), подписанный в каждой сессии `-p` (`cli:1505033`), не цикл запроса (`RC` сырой `rate_limit_event` отбрасывает, `cli:1489258`); каждое срабатывание даёт кадр (`ZHr`, `cli:462886`–`cli:462890`). Срабатывает: (1) после заголовков каждого ответа API, если объект лимитов изменился при глубоком сравнении (`cli:324136`–`cli:324162`, `cli:324223`) или у любого окна (`five_hour`, `seven_day`, `seven_day_overage_included`) сменился ключ `округлённый процент @ resets_at : status : surpassedThreshold` (`cli:323738`–`cli:323740`, `cli:324224`–`cli:324238`) — то есть рост использования на 1 п.п. даёт кадр без смены статуса; ещё на ответ 429, сброс «не подписчик → allowed», забывание и восстановление окон (`cli:324110`–`cli:324195`, `cli:324336`); (2) повтор отказа — под флагом `tengu_gravel_chorus` (по умолчанию `true`), не на gateway, с тем же `resetsAt` не чаще раза в 30 с (`cli:1352696`–`cli:1352712`); (3) смена предложения кредитов и запроса администратору (`cli:1306109`, `cli:1486379`, `cli:1486694`–`cli:1486712`). Вживую на стенде — 28 кадров за 33 прогона, все `allowed`.
- **Поля:** `rate_limit_info` (`cli:106863`: `status`, `resetsAt`, `rateLimitType`, `utilization`, `overage*`, `unifiedWindows[i]` и др.).
- **Расширение:** окна лимитов из `unifiedWindows` показывает на панели (`ext:125724`–`ext:125729`).

### `system/elicitation_complete`
- **Когда:** MCP-сервер подтвердил конец запроса ввода (`cli:1494366`). **Поля:** `mcp_server_name`, `elicitation_id`.

### `system/permission_denied`
- **Когда:** вызов инструмента отклонён правилом, режимом или ответом хоста (`emitPermissionDenied`, `cli:1468181`–`cli:1468195`; `cli:768587`). **Поля** (`cli:109361`): `tool_name`, `tool_use_id`, `agent_id[?]`, `decision_reason_type[?]`, `decision_reason[?]`, `message`.

### `prompt_suggestion`
- **Когда:** после хода, если подсказки включены (`initialize.promptSuggestions`) (`cli:1486897`). **Поля:** `suggestion`. Вкладка подсказки не заказывает (`ext:138052`).

### `system/mirror_error`
- **Когда:** ошибка зеркала транскрипта (`--session-mirror`) (`cli:1087039`–`cli:1087041`). **Поля:** `error`, `key[?]`.

### `system/informational`
- **Когда:** служебные строки в ленту (17 мест, например `cli:1485426`); `RC` переводит внутреннюю форму с `timestamp` в SDK-форму (`cli:1489187`–`cli:1489199`). **Поля** (`cli:108274`): `content`, `level`, `tool_use_id[?]`, `prevent_continuation[?]`, `tag[?]`.

### `conversation_reset`
- **Когда:** `/clear` в неинтерактивном режиме (`cli:992320`–`cli:992333`). **Поля** (`cli:109433`): `new_conversation_id`, `trigger[?]`, `user_message_uuid[?]`. После него `total_cost_usd` считается заново (`d312:5958`).

### 2.2 Что CLI шлёт сверх union `SDKMessage`

Схема stdout внутри CLI (`Oo`, `cli:109811`–`cli:109877`, 61 член) содержит 22 типа, которых нет в union d.ts; ещё 9 типов уходят в stdout мимо `Oo`. Формат записей тот же, короче.

### `command_lifecycle`
- **Когда:** судьба каждого входящего сообщения с `uuid` (промпт, слэш-команда). Пересылка ставится один раз на транспорт, кроме удалённого (`aV`/`dV`, `cli:1503342`–`cli:1503358`); повторный `queued` с `ran_ahead:true` — для мгновенных команд во время хода (`cli:1512829`–`cli:1512837`).
- **Поля** (`cli:109493`): `command_uuid`, `state` (`queued`, `started`, `completed`, `cancelled`, `discarded`, `refused`), `wakeup_source[?]` (`loop_wakeup`/`schedule_wakeup`), `scheduled_for[?]`, `ran_ahead[i?]`.
- **Граничные случаи** (описание схемы, `cli:109518`): сообщение без `uuid` событий не получает; `cancelled` бывает и от `cancel_async_message`, и от `interrupt` с `cancel_queued`, и когда ход, в который сообщение влито, прерван или упал — переотправлять вслепую нельзя, сверять со своими ответами на cancel и с `cancelled` квитанции `interrupt`; `discarded` — сессия закрыта `end_session`, пока сообщение ждало; `refused` — отказ политики приёма, без `queued`; влитые в ходы, кончившиеся `max_turns`/`hook_stopped`/`tool_deferred`/`background_requested`, получают `completed`, хотя их содержимое могло не отработать. Объявлено возможностью `msg_lifecycle_v1` (`cli:747284`). На каждый входящий `control_response` с `uuid` CLI сам отвечает `completed` (`cli:1467782`–`cli:1467804`).
- **Расширение:** ведёт множество ожидающих `uuid` (`xVe`, `ext:123896`–`ext:123911`) и по нему решает, кончился ли ход (`FQ`, `ext:123862`).

### `system/session_title_changed`
- **Когда:** у сессии появилось или сменилось название — автоназвание или `rename_session` (подписка `Vhs`, `cli:1476948`–`cli:1476953`). **Поля:** `title`.
- **Расширение:** применяет название и в webview кадр не пересылает (`ext:125708`–`ext:125712`).

### `system/turn_preempted`
- **Когда:** только если клиент объявил `rapidFollowupPreempt` и включён флаг: ход, ещё ничего не показавший, остановлен ради нового сообщения (`cli:1482713`). **Поля** (`cli:109606`): `reason` (`rapid_followup`), `preempted_by_uuid`, `preempted_message_uuids`.

### `system/inherited_asks_settled`
- **Когда:** новый процесс той же сессии разобрал вопросы, унаследованные от прошлого воркера (`cli:1492393`–`cli:1492397`). **Поля:** `kept_request_ids`.

### `system/instruction_size_warning`
- **Когда:** файлы инструкций (`CLAUDE.md` и правила) превысили лимит (`cli:1495543`). **Поля** (`cli:108370`): `total_chars`, `total_limit_chars`, `file_count`, `largest_chars[?]`; на проводе ещё `limit_chars`, `files_over_limit` (`cli:1495538`–`cli:1495541`).

### `system/permission_check_status`
- **Когда:** долгая проверка прав вызова (классификатор) — начало и конец (`cli:452704`–`cli:452712`). **Поля** (`cli:109388`): `tool_use_id`, `agent_id[?]`, `status`.

### `system/per_turn_effort_changed`
- **Когда:** выключилось разовое усилие хода (`OZ`, `cli:494933`–`cli:494935`). **Поля:** `per_turn_effort_active`.

### `system/peer_message_hold`
- **Когда:** сообщение другой сессии задержано или отпущено политикой приёма (`cli:1500615`–`cli:1500625`). **Поля** (`cli:108595`): `state`, `message_uuid[?]`, `lane`, `from`, `from_name[?]`, `cause[?]`, `outcome[?]`.

### `system/feedback_draft_queued`
- **Когда:** CLI подготовил черновик отзыва (`cli:744568`–`cli:744575`). **Поля:** `draft_id`, `draft_type`, `title`, `details_preview`.

### `system/code_change_published`, `system/vcs_state_changed`
- **Когда:** опубликован PR или ветка (`oc`, `cli:358828`–`cli:358840`); изменилось состояние git в рабочей папке (`cli:358862`). **Поля:** `provider`, `url`, `repo`, `identifier`, `action[?]`, `branch[?]` (`cli:109521`); `kind`, `cwd`, `branch[?]` (`cli:109567`).

### `system/dev_intent`
- **Когда:** детектор намерения в ходе сработал (`cli:990925`–`cli:990927`, `cli:991001`). **Поля:** `kind`, `trigger[?]`.

### `system/file_attachments_missing`
- **Когда:** у сообщения, пришедшего через мост, не нашлись вложения (`cli:1352361`–`cli:1352370`). **Поля** (`cli:109636`): `message_uuid`, `sent_count`, `missing`.

### `system/ui_invalidate`, `system/ui_log`, `system/ui_toast`, `system/ui_status`, `system/ui_panes`, `system/ui_scroll`, `system/ui_focus`
- **Когда:** поверхности плагинов (`$.ui.*`) в безголовой сессии: `cli:1506313`–`cli:1506317`, `cli:1507265`, `cli:1507409`–`cli:1507426`, `cli:1507503`–`cli:1507523`. Все поля внутренние (`cli:109053`–`cli:109242`). Приходят и в локальном `-p` со stream-json, если модуль хуков плагина зовёт `$.ui.*`: гейт `El()` = `outputFormat === "stream-json" && Vle(канал)` (`cli:1507233`–`cli:1507241`), модули хуков плагинов включены флагом `tengu_plugin_hooks_modules` (по умолчанию `true`, `cli:209956`), для `claude-vscode` — пока выключен `tengu_melodic_ember` (`svr()`, `cli:1466673`–`cli:1466675`). Вживую `--safe-mode` дал `system/ui_invalidate {event:"ui.render"}` до первого хода (1.1). Запросы хосту 4.8 — по тому же гейту (`cli:1506366`–`cli:1506427`).

### `system/turn_handoff_available`, `system/cloud_session_delta`
- **Когда:** облачные воркеры и клиент облачных сессий (`cli:1471282`–`cli:1471290`, `cli:1517185` — вне модуля `print.ts`). В локальном `-p` не приходят: `turn_handoff_available` пишет только удалённый транспорт `lU` (`cli:1473597`–`cli:1473598`), а он создаётся только при `--sdk-url` (`cli:1514055`–`cli:1514058`); `cloud_session_delta` — только `--cloud` с `-p` (`writeDelta`, `cli:1517173`–`cli:1517193`; `cli:795266`–`cli:795275`).

### `system/post_turn_summary`, `system/task_summary`, `system/session_metadata`
- **Когда:** сводка после хода (`cli:756361`–`cli:756370`); итог задачи сессии и артефакты — при записи метаданных сессии (`notifyMetadataChanged`, `cli:390640`–`cli:390650`). **Поля:** `summarizes_uuid`, `status_category`, `status_detail`, `needs_action` (`cli:108240`); `detail` (на проводе; в схеме полей нет, `cli:108254`); `metadata` (`cli:108259`).
- **Расширение:** SDK передаёт их потребителю отдельной веткой (`ext:110246`–`ext:110252`).

### `system/model_fallback`, `system/model_consent_fallback`
- **Когда:** основная модель недоступна, CLI перешёл на запасную (`cli:768781`–`cli:768790`, в цикле — `cli:761495`); то же после согласия пользователя (`Lbo`, `cli:768790`). `RC` пропускает оба (`cli:1489183`–`cli:1489186`). **Поля** (`cli:108527`, `cli:108548`): `trigger`, `original_model`, `fallback_model`, `content`; у второго `choice`, `original_model_name[?]`, `persisted_as_default`.

### `transcript_mirror`
- **Когда:** только с `--session-mirror`: строки, дописанные в транскрипт (`cli:1504868`–`cli:1504871`). **Поля:** `filePath`, `entries`. SDK отдаёт их своему пакетировщику и потребителю не показывает (`ext:110240`–`ext:110242`). Вживую (`x4-session-mirror`): кадр на каждую порцию дописанных записей (`entries` — сами записи транскрипта), после `/clear` следующий же кадр несёт путь нового файла — раньше нового `system/init`.

### `active_goal`, `autocompact_state`
- **Когда:** только при `CLAUDE_CODE_REMOTE` (`cli:1504978`–`cli:1504990`): цель сессии и состояние автокомпакции (`Lb`, `cli:1482999`–`cli:1483013`). **Поля:** `value` (`cli:109688`; `cli:109704`: `enabled`, `effective_window`, `threshold`, `enforced`, `source`). В локальном `-p` их нет — порог автокомпакции оттуда не узнать.

### `system/bridge_state`
- **Когда:** состояние моста Remote Control (`kk`, `cli:1505803`–`cli:1505812`). **Поля:** `state`, `detail`, `failure_kind` (при `failed`), `bridge_epoch`. В схеме нет.
- **Расширение:** обрабатывает сам и в webview не пересылает (`ext:125642`–`ext:125652`).

### 2.3 Что есть в схеме, но в stdout `-p` не попадает

- Внутренние `system`-сообщения транскрипта, рождённые в цикле (`cli:593241`–`cli:593383`): `permission_retry`, `stop_hook_summary`, `memory_saved`, `agents_killed`, `away_summary`, `scheduled_task_fire`, `turn_duration`, `api_error` (вместо него — `api_retry`), а также `thinking` и `file_snapshot` (`cli:460609`). `RC` их отбрасывает (`cli:1489201`–`cli:1489202`); прямых отправителей в stdout у них нет (у всех мест создания нет `ea()`/`enqueue()` рядом).
- Сырые типы движка: `tombstone`, `api_metrics`, `os_notification`, `apply_flag_settings`, `set_expanded_view`, `set_in_progress_tool_use_ids`, `hint_clears`, `open_message_selector`, `compact_progress`, `stream_mode`, `response_length`, `refusal_continuation` (`cli:1489252`–`cli:1489275`).
- `system` / `session_writes_lost` — появился в типах 0.3.319 (`d319:6274`), в 2.1.312 его нет (`"session_writes_lost"`: 0 вхождений и в разобранном коде, и в сыром бинаре).

### 2.4 Конверты управления в stdout

- `control_request` — запрос CLI к клиенту, раздел 4 (`cli:114137`).
- `control_response` — ответ CLI на запрос клиента: `{subtype:"success", request_id, response?}` или `{subtype:"error", request_id, error, error_code[i?], model[i?]}` (`cli:114162`–`cli:114196`); у ответа на `initialize` ещё `pending_permission_requests`, `pending_user_dialog_requests`. С `--replay-user-messages` CLI ещё и **эхом** отдаёт входящие `control_response` клиента (`cli:1512337`–`cli:1512339`).
- `control_cancel_request` — CLI отзывает свой запрос: `{request_id}` (`cli:114202`). Шлётся, когда ожидание прервано (ход остановлен, вопрос отменён) — `enqueueCancelRequest` в `sendRequest` (`cli:1468148`).
- `keep_alive` — пустой кадр: раз в 30 с во время пяти долгих control-запросов — `register_repo_root`, `fork_conversation`, `stage_file`, `add_directory`, `mcp_call` (`fc`, `cli:1508426`–`cli:1508435`; вызовы — `cli:1508473`, `cli:1509854`, `cli:1510448`, `cli:1510477`, `cli:1510731`); до ответа на `initialize` — `{"type":"keep_alive","startup_mark":…}` при `CLAUDE_CODE_SDK_READS_STARTUP_MARKS` (`cli:68222`–`cli:68228`, `cli:806996`–`cli:807003`); между ходами, пока идут фоновые задачи, — таймер `py`: период `JN = 30000` мс, флаг `tengu_ccr_keepalive_after_stop_enabled` (`cli:1491110`–`cli:1491145`), взводится на `interrupt` и `remote-cancel`. Но флаг читается через `sv()`, которая без `CLAUDE_CODE_REMOTE` возвращает `"off"` (`cli:509427`–`cli:509431`), и `arm()` выходит сразу (`cli:1491128`): в локальном `-p` между ходами `keep_alive` нет — вживую 0 кадров за ~95 с после `interrupt`, пока жила фоновая команда (`t5-p12-bg`; флаг на стенде `true`).

---

## 3. Клиент → CLI

### 3.1 Что CLI принимает на stdin

Разбор строки — `processLine` (`cli:1467685`–`cli:1467956`).

| Тип | Что делает |
| --- | --- |
| `user` | сообщение пользователя (3.2) |
| `control_request` | запрос к CLI (3.3); без поля `request` — **процесс завершается** («Error: Missing request on control_request», `cli:1467892`) |
| `control_response` | ответ на запрос CLI (раздел 4); ответ без ожидающего запроса — `settleUnknownResponse` (`cli:1467793`) |
| `control_cancel_request` | отменить запрос, который CLI ещё обрабатывает: прерывает его (`remote-cancel`, `cli:1512340`–`cli:1512343`) |
| `keep_alive` | игнорируется (`cli:1467690`) |
| `update_environment_variables` | меняет `process.env` только для разрешённых ключей и только на транспортах, которые принимают это (`cli:1467691`–`cli:1467767`); иначе отказ в лог |
| `assistant`, `system` | **история**: дописываются в разговор без хода (`Eft`, `cli:1512401`–`cli:1512413`); `assistant` с `--replay-user-messages` отдаётся эхом |
| `bash_command` | выполнить команду оболочки как `!` в терминале: `command`, `cwd[?]` (`cli:106846`, `cli:1512430`) |
| `session_notice` | служебное уведомление от другой сессии (`cli:1512359`–`cli:1512374`, схема `cli:114220`) |
| `queued_notification` | только на удалённом транспорте; на локальном отбрасывается (`cli:1512375`–`cli:1512383`) |
| `workflow_launch` | запуск workflow (`cli:1512345`–`cli:1512357`) |
| прочее | «Ignoring unknown message type» в лог, кадр выброшен (`cli:1467869`) |

Невалидный JSON на локальном stdio — **фатально**: `exitOnFatalInput` (`cli:1467939`); `user` с `message.role` не `user` — тоже фатально на локальном транспорте (`cli:1467900`–`cli:1467902`), на удалённом — попытка починки или выброс.

### 3.2 Сообщение пользователя

```json
{"type":"user","uuid":"<uuid>","parent_tool_use_id":null,
 "message":{"role":"user","content":[{"type":"text","text":"…"},
   {"type":"image","source":{"type":"base64","media_type":"image/png","data":"…"}}]},
 "priority":"next"}
```

- `message` — `MessageParam` Messages API: строка или блоки `text`, `image`, `document`, `tool_result` (`d312:6564`–`d312:6567`). Картинка: `source` — `base64` (`image/jpeg|png|gif|webp`), `url` или `file` (Files API) (`cli:105052`–`cli:105060`); документ — `application/pdf`, `text/plain`, `url`, `file` (`cli:105061`–`cli:105065`).
- `uuid` — ключ всего: по нему приходят эхо, `command_lifecycle`, `result.user_message_uuid(s)`, отмена (`cancel_async_message`). Без `uuid` сообщение исполняется, но не отслеживается и в квитанциях не перечисляется (`cli:109496`, `d312:4848`). Повтор с тем же `uuid` не исполняется, а подтверждается эхом (`cli:1512484`–`cli:1512498`).
- `parent_tool_use_id` — `null` для главного потока.
- `priority` — `now` | `next` | `later` (`cli:106676`). `next` — по умолчанию. `later` — после остального. `now` во время хода: CLI переводит его в `next` с пометкой `sentNow` и пробует доставить немедленно — сдвигает мешающие команды в фон или прерывает ход ради нового сообщения (`takeBeforeEnqueue`, `cli:1491720`–`cli:1491737`; `interruptForSubmit` — `cli:1491709`). Правило (по коду и вживую, `p9-*`): быстрая доставка перехватывает кадр, только если есть `uuid`, идёт ход, `origin.kind === "human"` (`Id`, `cli:149145`) и в разговоре есть незакрытый `tool_use`; включена флагами `tengu_velvet_panda` и `tengu_bubbly_meteor` (`wIo`, `cli:549359`; на стенде оба `true`). Тогда переносимый вызов (Bash) уходит в фон — `task_updated {is_backgrounded:true}` через 3 мс после `queued`, `tool_result` «Command was moved to the background … so that a message that arrived while it was ru…» с `tool_use_result.backgroundedToDeliverMessage: true`, эхо нового сообщения, и ход продолжается с обоими `uuid` в `user_message_uuids`; так же переносятся вызов MCP и субагент в переднем плане. Вживую (`p9-now-mcp-human`): MCP-вызов на 30 с в момент переноса впервые становится задачей — `task_started {task_type:"mcp_task"}` и `background_tasks_changed`, `tool_result` «MCP tool "slowtool/wait" was moved to the background as task … so that a message t…»; по его концу — `task_notification completed` и ход, который CLI начинает сам. `p9-now-agent-human` (без `claude-vscode`, `run_in_background:false`): `task_updated {is_backgrounded:true}` у идущего субагента, `tool_result` «Async agent launched successfully…». В обоих случаях сообщение влито в тот же ход, `user_message_uuids` — оба. Прерывание или отмена (`wi()`, `cli:1353055`–`cli:1353069`) остаются для вызова без переносимого держателя, который идёт дольше двух проверок по 200 мс (`qn = 2`, `yi = 200`); Bash, MCP и субагент такого случая не дали. Во всех остальных случаях работает общий путь `PP` (`cli:1491608`–`cli:1491613`) — прерывание хода: без `origin` во время Bash команда доходит до конца, а ход закрывается `result success`, `terminal_reason:"aborted_tools"`, `result:""`, первое сообщение получает `cancelled`; во время текста модели — с `origin` и без — `result success`, `is_error:false`, `terminal_reason:"aborted_streaming"`, `stop_reason:null`, в `result` — недописанный текст. Новое сообщение идёт своим ходом. Вкладка `priority` не ставит, а `origin {kind:"human"}` ставит (`web:259692`).
- `shouldQuery: false` — дописать без хода; текст сольётся со следующим сообщением, которое ход откроет (`cli:106703`–`cli:106707`).
- `client_composed: true` — текст не печатал человек: без разворота `@path`, без слэш-команд и без вложений начала хода (`d312:6614`–`d312:6617`).
- `pasted_content`, `inline_pastes` — вставленный текст (`d312:6635`–`d312:6642`).
- `origin` — источник; хост, передающий ввод с клавиатуры, должен ставить `{kind:"human"}`, иначе строгие проверки «это человек» не пройдут (`d312:5596`–`d312:5599`).
- Внутренние: `requeue_if_interrupted_within_ms` — вернуть сообщение в очередь, если `interrupt` пересёкся с ходом, который его уже взял (до 10000 мс, `cli:106677`–`cli:106683`); `file_attachments` (`cli:106813`).

### 3.3 Запросы клиента к CLI (`control_request`)

Конверт: `{"type":"control_request","request_id":"<свой id>","request":{"subtype":"…",…}}`. Ответ — один `control_response` с тем же `request_id` (2.4). Диспетчер — один цикл `for await` в `print.ts` (`cli:1508764`–`cli:1512336`) плюс таблица простых обработчиков `Qe` (`cli:1477045`–`cli:1477066`). Всего он знает **101** подтип (список — `vendor-src/scripts/dispatcher.txt`).

Общие правила ответа:
- Неизвестный подтип — `error` с текстом `Unsupported control request subtype: <subtype>` (`Z7`, `cli:1339784`; ветка `else`, `cli:1512323`). Так отвечают и на `set_color`, хотя он есть в union d.ts (`d312:5318`), и на `send_task_message` (обработчик всегда бросает эту ошибку, `cli:1477042`–`cli:1477044`).
- Процесс, запущенный с `--await-claim` и ещё не занятый, на всё, кроме `claim_session` и разрешённых, отвечает ошибкой `not_claimed: …` (`cli:1508801`–`cli:1508803`).
- После обработки запроса с `uuid` CLI шлёт `command_lifecycle completed` (`cli:1512325`).
- Медленный обработчик пишется в лог `cli_control_request_handled` (`cli:1512329`–`cli:1512334`). Общего срока на ответ нет; внутренний срок найден только у `reload_plugins` и `reload_skills`, и то не всегда (`cli:1510614`–`cli:1510615`, `cli:1510699`–`cli:1510702`).

#### `initialize`
См. 1.3. Ветка — `cli:1508842`–`cli:1509044`. Ошибки формы возвращаются `error`, а не роняют процесс.

#### `interrupt`
- **Запрос** (`cli:110245`): `cancel_queued[?]`; внутренние `send_now`, `message_uuid`, `for_user_message_uuids`, `worker_epoch`, `reason`.
- **Что делает** (`vr`, `cli:1508637`–`cli:1508707`): прерывает идущий ход (`abort`); останавливает отложенный вопрос о правах (`np`, `cli:1508547`–`cli:1508552`); фоновых субагентов и workflow щадит только при `perTaskStopAffordance` (`La`, `cli:1504650`–`cli:1504655`), иначе останавливает (`Kle`); с `cancel_queued:true` ещё и снимает с очереди все сообщения с `uuid` — каждое получает `command_lifecycle cancelled` (`d312:4836`).
- **Ответ** (`cli:110282`–`cli:110305`): `still_queued` — `uuid` сообщений, которые переживут прерывание и **будут исполнены**; `cancelled[?]` — снятые (только с `cancel_queued`); внутренние `put_back` (возвращены в очередь из взятого хода), `send_now` (`stopped`/`delivering`/`nothing_waiting`), `turn` (`stopped`/`idle`/`spared`).
- **Граничные случаи** (`d312:4845`–`d312:4848`): квитанция пишется до `result` прерванного хода; список `still_queued` неполон — сообщения без `uuid` и уведомления о задачах в нём не видны, а пустой список не значит «ничего не будет»; прерывание в окне до первого хода «защёлкивается»: первый ход, несущий обречённую работу, стартует уже прерванным. Поле `turn` приходит, только если в запросе были `for_user_message_uuids`; `idle` — хода не было (`cli:110297`).

#### `end_session`
- **Что делает** (`cli:1508826`–`cli:1508841`): ставит `endSessionRequested`, прерывает ход с причиной `shutdown`, сбрасывает очередь (ожидавшие сообщения получают `discarded`), отвечает `success` и выходит из цикла — процесс завершается. `end_session` с `reason` «archived» от прошлого жизненного цикла игнорируется (`cli:1508827`–`cli:1508829`).

#### `set_permission_mode`
- **Запрос** (`cli:110416`): `mode`; внутренние `ultraplan`, `from_settings`.
- **Ответ** (`cli:1509045`–`cli:1509072`): `{mode}` — режим, который реально встал; неизвестный режим — `error` с кодом `invalid_mode`; переход в `auto` может быть отвергнут проверкой (`cli:1509057`–`cli:1509062`). После смены CLI шлёт `system/status` с `permissionMode` (`cli:1504860`–`cli:1504866`).

#### `set_model`
- **Запрос** (`cli:110435`): `model[?n]` (`null` — модель по умолчанию); внутренние `system_prompt`, `only_if_model_is_current`.
- **Ответ** (`cli:1509084`–`cli:1509106`): `{model}` — модель следующего хода; ошибка — `error` с `error_code` (`invalid_request`, `catalog_unknown`, `not_found`, … — `cli:114180`) и полем `model` — какой модель осталась; внутренний сбой — `set_model failed`, код `internal_error`. Действует со следующего хода.

#### `set_max_thinking_tokens`
- **Запрос** (`cli:110464`): `max_thinking_tokens[?n]`, `thinking_display[?n]` (`summarized`/`omitted`/`highlights`). `null` у бюджета — вернуть бюджет запуска; `null` у показа — снять переопределение.
- **Ответ:** `success` или `error` «set_max_thinking_tokens: part of the request refused» (`NOn`, `cli:1357090`–`cli:1357094`); `highlights` принимается не везде, причина — в тексте ошибки, бюджет при этом применяется (`cli:1357104`, `cli:110470`).

#### `stop_task`, `background_tasks`, `get_task_output`
- `stop_task {task_id}` — остановить одну задачу, ответ `{}`; ошибка — «stop_task: task_id must be a string» или причина остановки (`Nt`, `cli:1476997`–`cli:1477013`). Остановленный субагент приходит `task_notification stopped`.
- `background_tasks {tool_use_id?}` — перевести в фон идущие Bash и субагентов: с `tool_use_id` — один, ответ `{backgrounded}`; без — все, ответ `{}` (`Bt`, `cli:1477015`–`cli:1477022`).
- `get_task_output {task_id}` — хвост вывода фоновой команды или Monitor (последние 8 КиБ): `{output, total_bytes, truncated}`; для субагента — ошибка «no shell or Monitor task with that task_id» (`qt`, `cli:1477023`–`cli:1477040`, описание `cli:112294`).

#### `cancel_async_message`
- **Запрос:** `message_uuid`. **Ответ:** `{cancelled: boolean}` (`cli:1509626`–`cli:1509630`). `false`, если сообщение уже снято с очереди, уже вливается в ход (`isFoldInFlight`) или его нет. Снятое получает `command_lifecycle cancelled`.

#### `rename_session`
- **Запрос:** `title`, `source[?]` (`remote`/`host`), `session_id[?]` (должен совпасть с текущей). Пустое название — ошибка «title must be non-empty» (`jt`, `cli:1476961`–`cli:1476975`). Следом приходит `session_title_changed`.

#### `get_context_usage`
- **Запрос:** `detail[?]` (`summary`/`full`). **Ответ:** разбивка окна по категориям (`Ct`, `cli:1476574`–`cli:1476579`; схема ответа — `cli:107082`).

#### `rewind_files`
- **Запрос:** `user_message_id`, `dry_run[?]`. **Ответ:** `{canRewind, filesChanged, …}` или `error` «Failed to rewind…» (`cli:1509612`–`cli:1509625`). Нужен `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING`.

#### `mcp_message`, `mcp_set_servers`, `mcp_status`
- `mcp_message {server_name, message}` — в обе стороны: от CLI к клиенту для серверов, которые живут в SDK, и обратно; ответ `{mcp_response}` (`cli:111996`–`cli:112006`, ветка `cli:1509599`).
- `mcp_set_servers` — заменить набор динамических MCP (`cli:1510553`); `mcp_status` — состояние подключений (`cli:1508807`).

#### Остальные подтипы диспетчера

Строка — первая ветка подтипа в диспетчере; «Qe» — таблица `cli:1477045`–`cli:1477066`. Описания сжаты из описаний схемы CLI (`cli:109920`–`cli:113786`); у подтипов без схемы — по имени и ветке.

| Подтип | Что делает | Строка |
| --- | --- | --- |
| `side_question` | вопрос «в сторону» без хода; ход сообщает `control_request_progress` | `cli:1511702` |
| `fork_conversation` | развилка разговора (схемы нет) | `cli:1509851` |
| `rewind_conversation` | откат разговора к `target_message_uuid` (схемы нет) | `cli:1509642` |
| `export_conversation` | текст разговора, как пишет `/export` | `cli:1509423` |
| `generate_session_title` | сгенерировать название (схемы нет) | Qe |
| `add_directory`, `register_repo_root`, `set_cwd` | добавить рабочую папку; корень репозитория с перечиткой `CLAUDE.md`; переезд в другую папку (аналог `/cd`) | `cli:1510474`, `cli:1510471`, `cli:1509107` |
| `claim_session` | занять «запасной» процесс `--await-claim` | `cli:1509186` |
| `poll_event`, `turn_handoff`, `prefetch_attachments`, `stage_file` | облачные воркеры: события, передача хода, подкачка вложений, выкладка файла | `cli:1509631`, `cli:1512320`, Qe, `cli:1510426` |
| `seed_read_state`, `read_file`, `list_directory`, `get_workspace_diff`, `get_plan` | отметить файл прочитанным для `Edit`; прочитать файл по правам `Read`; список папки; git diff; план plan-режима | `cli:1510495`, `cli:1509868`, `cli:1509909`, `cli:1510401`, `cli:1510411` |
| `start_shell_task` | фоновая команда облачной сессии | `cli:1509936` |
| `set_proactivity_level`, `set_prompt_suggestions_paused` | уровень инициативы агента; пауза подсказок | `cli:1509073`, `cli:1510938` |
| `apply_flag_settings`, `get_settings`, `update_settings` | слой настроек из флагов; итоговые настройки; запись настроек | `cli:1508771`, `cli:1511526`, `cli:1511569` |
| `get_hooks_listing`, `list_permission_rules`, `set_mcp_permission_mode_override` | список хуков (`/hooks`); правила прав (`/permissions`); режим прав MCP-сервера, только ужесточение | `cli:1511559`, `cli:1511595`, Qe |
| `reload_plugins`, `reload_skills`, `reload_output_styles` | перечитать с диска | `cli:1510601`, `cli:1510697`, Qe |
| `get_session_cost`, `get_usage`, `list_models`, `get_binary_version`, `get_status` | стоимость; `/usage`; каталог моделей; версия; строки `/status` | Qe, Qe, Qe, Qe, `cli:1509397` |
| `file_suggestions` | автодополнение `@`-путей | `cli:1510484` |
| `get_memory_dialog`, `get_skills_dialog`, `get_sandbox_dialog`, `get_chrome_dialog` | данные диалогов `/memory`, `/skills`, `/sandbox`, `/chrome` | `cli:1509486`, `cli:1509506`, `cli:1509588`, `cli:1509517` |
| `get_chrome_browsers`, `select_chrome_browser`, `set_chrome_browser_hints` | Claude in Chrome | `cli:1509531`, `cli:1510917` |
| `mcp_call` | вызвать инструмент MCP без хода и без проверки прав | `cli:1510708` |
| `mcp_read_resource`, `mcp_reconnect`, `mcp_toggle`, `mcp_tasks_changed` | ресурс `ui://`; переподключить; вкл/выкл; уведомление о задачах MCP | Qe |
| `mcp_authenticate`, `mcp_clear_auth`, `mcp_oauth_callback_url`, `channel_enable` | вход в MCP-сервер, сброс входа, адрес OAuth, включить канал (схем нет) | `cli:1510948`, `cli:1511274`, `cli:1511083`, `cli:1510945` |
| `claude_authenticate`, `claude_oauth_callback`, `claude_oauth_wait_for_completion` | вход в аккаунт Claude (схем нет) | `cli:1511123`, `cli:1511240`, `cli:1511241` |
| `list_cloud_sessions`, `get_cloud_session`, `get_cloud_session_transcript` | облачные сессии аккаунта | `cli:1509447`–`cli:1509449` |
| `remote_control`, `remote_tools_announce`, `register_device_hooks`, `upload_device_hook_template` | Remote Control и машины, обслуживающие инструменты облачной сессии | `cli:1511823`, `cli:1512286`, `cli:1512254`, — |
| `message_rated`, `submit_feedback`, `submit_survey_feedback`, `ultrareview_launch` | оценка сообщения; `/feedback`; опрос; запуск ultrareview | `cli:1511809`, `cli:1511609`, `cli:1511683`, `cli:1511774` |
| `ui_attach`, `ui_detach`, `ui_render`, `ui_press`, `ui_client_module`, `ui_client_press`, `ui_message`, `ui_client_fault`, `ui_input`, `ui_select`, `ui_panes`, `ui_pane_show`, `ui_pane_focus`, `ui_close`, `ui_scroll`, `ui_focus`, `ui_prompt_edit`, `ui_prompt_autocomplete` | удалённые поверхности плагинов (все `@internal`) | `cli:1509971`–`cli:1510311` |

---

## 4. Запросы CLI к клиенту

Все идут через `sendRequest` (`cli:1468130`–`cli:1468180`): CLI пишет `control_request`, ждёт `control_response` с тем же `request_id`. **Своего таймаута у `sendRequest` нет**; ожидание кончается ответом, отменой (`AbortSignal` хода → CLI шлёт `control_cancel_request`, `cli:1468141`–`cli:1468157`) или закрытием stdin («Stream closed», `cli:1468139`). Пока ждёт ответа человека, сессия в `requires_action` (`beginUserDecision`, `cli:1468143`). В union d.ts из них — 4 (`can_use_tool`, `hook_callback`, `elicitation`, `request_user_dialog`); в схеме CLI — 17 плюс двусторонний `mcp_message` (`upgrade_relay_marker`, перечисленный в 4.7, — не запрос, а вид строки в поле `relay_marker` запроса клиента `turn_handoff`, `cli:111965`–`cli:111968`).

### 4.1 `can_use_tool`
- **Когда:** вызов инструмента требует решения, а CLI запущен с `--permission-prompt-tool stdio` (иначе отказ без вопроса). С `--permission-prompts none` — локальный отказ, хост не спрашивают (`cli:1503611`–`cli:1503617`).
- **Запрос** (`cli:110311`): `tool_name`, `input`, `tool_use_id`, `permission_suggestions[?]`, `blocked_path[?]`, `decision_reason[?]`, `decision_reason_type[?]`, `title[?]`, `display_name[?]`, `description[?]`, `agent_id[?]` (вызов субагента), `mcp_server[?]`, `suppress_always_allow_rule[?]`, `default_to_no[?]`, `matched_ask_rule[?]`, `requires_user_interaction[?]`, `classifier_approvable[?]` и внутренние. Вживую `requires_user_interaction: true` пришёл на каждом из 12 запросов `AskUserQuestion`; на Bash с правилом `permissions.ask` — `decision_reason_type: rule` и `decision_reason_rule_source`, без `matched_ask_rule`; `suppress_always_allow_rule` не встретился ни разу (`x2-perm-*`, `x2b-perm-*`, `x5-*`).
- **Разрешить:** `{"behavior":"allow","updatedInput":{…},"updatedPermissions":[…]}` — `updatedInput` необязателен (пустой → исходный ввод), `toolUseID`, `decisionClassification` (`user_temporary`/`user_permanent`/`user_reject`) — по желанию (`cli:1458610`–`cli:1458616`). `updatedPermissions` с `setMode` меняет режим (`cli:1458682`). Для `AskUserQuestion` ответы кладутся в `updatedInput.answers` (подтверждено вживую, `hqc:docs/SPEC.md:2376`, `hqc:docs/SPEC.md:5094`).
- **Отказать:** `{"behavior":"deny","message":"…"}` — модель получает отказ с этим текстом и продолжает ход.
- **Отказать и прервать:** `{"behavior":"deny","message":"…","interrupt":true}` — CLI прерывает ход (`JM(...).abort()`, `cli:1458710`–`cli:1458711`).
- **Ошибка вместо ответа:** `control_response` `subtype:"error"` или исключение → отказ с текстом `Tool permission request failed: <error>` (`VPo`, `cli:434061`–`cli:434067`; `cli:1459241`); ответ не той формы → отказ «The canUseTool callback returned an invalid permission result. Expected {behavior: 'allow', …} or {behavior: 'deny', message: string}» (`cli:1459243`–`cli:1459247`, `cli:1458632`); ход прерван, пока ждали, → «Tool permission request aborted» (`cli:1459248`–`cli:1459251`).
- **Нет ответа:** CLI ждёт сколько угодно. Незакрытые вопросы перечисляются в ответе на `initialize` (`pending_permission_requests`, `d312:418`). Ответ на уже отозванный вопрос CLI отбрасывает (`settleUnknownResponse`, `cli:1467793`).
- **Расширение:** SDK зовёт свой `canUseTool`; если колбэк вернул `null`, оставляет запрос без ответа (`ext:110441`); при `control_cancel_request` прерывает колбэк (`ext:110378`–`ext:110381`).

### 4.2 `hook_callback`
- **Запрос** (`cli:111481`): `callback_id` (из `initialize.hooks`), `input` (вход хука), `tool_use_id[?]`; внутренние `issued_at`, `deadline_ms`.
- **Ответ:** JSON вывода хука (как stdout хука-команды: `decision`, `hookSpecificOutput` и т. п.); `{}` — ничего не менять. Ошибка или исключение → в лог «Error in hook callback», CLI считает ответ пустым `{}` (`cli:1468265`–`cli:1468268`). Таймаут — `timeout` сопоставителя из `initialize` (`createHookCallback`, `cli:1468251`–`cli:1468255`).

### 4.3 `mcp_message`
- **Запрос:** `server_name`, `message` (JSON-RPC). **Ответ:** `{mcp_response}`; для неизвестного сервера SDK отвечает ошибкой (`ext:110446`–`ext:110451`).

### 4.4 `elicitation`
- **Когда:** MCP-сервер просит у пользователя ввод (`handleElicitation`, `cli:1468460`–`cli:1468501`). Не шлётся, если stdin закрыт, ход прерван или `--permission-prompts none` — тогда сразу `cancel`.
- **Запрос** (`cli:112560`): `mcp_server_name`, `message`, `mode[?]`, `url[?]`, `elicitation_id[?]`, `requested_schema[?]`, `property_order[?]`, `title[?]`, `display_name[?]`, `description[?]`.
- **Ответ** (`UQo`, `cli:112591`): `{action: "accept"|"decline"|"cancel", content?}`. Ошибка или исключение → `cancel` (`cli:1468497`–`cli:1468498`). Сессия на время вопроса в `requires_action`.

### 4.5 `request_user_dialog`
- **Когда:** инструмент просит хост нарисовать блокирующий диалог (`requestUserDialog`, `cli:1468503`–`cli:1468545`); только для видов, объявленных в `initialize.supportedDialogKinds`.
- **Запрос** (`cli:112596`): `dialog_kind`, `payload`, `tool_use_id[?]`.
- **Ответ** (`wkn`, `cli:112610`–`cli:112619`): `{behavior: "completed"|"cancelled", result?}`. Ответ `error` CLI **игнорирует** — диалог остаётся висеть (`cli:1467805`, `cli:1467561`). На вид, который хост не объявлял, отвечать нельзя, и `cancelled` — это настоящий отказ пользователя (`d312:5276`).
- **Нет ответа:** у диалога может быть срок (`parkTimeoutMs`), по нему CLI отменяет диалог сам и шлёт `control_cancel_request` (`cli:1468518`–`cli:1468536`); без срока висит до ответа или отзыва.

### 4.6 `oauth_token_refresh`, `host_auth_token_refresh`, `remote_control_work_secret`
- Хост держит учётные данные: свежий OAuth-токен после 401, ключ провайдера, секрет работы моста (`cli:112709`, `cli:112734`, `cli:112696`). Без соответствующего колбэка SDK отвечает ошибкой «… callback is not provided» (`ext:110491`–`ext:110510`). Приходят и в локальном `-p`, если хост выставил переменные: `oauth_token_refresh` — при `CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH` и точке входа `claude-desktop`/`local-agent`/`claude-vscode` (`cli:1503603`, `cli:145483`, `cli:145567`–`cli:145569`); `host_auth_token_refresh` — при `CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH` и `claude-desktop`/`claude-desktop-3p`/`local-agent` (`cli:1503605`–`cli:1503607`); `remote_control_work_secret` — только мостом, который клиент включил запросом `remote_control` (`cli:1468705`–`cli:1468724`).

### 4.7 `remote_tool_call`, `remote_plumbing_call`, `remote_tools_probe`, `remote_tools_reannounce`, `upgrade_relay_marker`
- Облачная сессия обращается к машине, которая обслуживает её инструменты (`cli:111845`–`cli:111965`). SDK вкладки первые четыре нарочно оставляет без ответа (`uVn`, `ext:109971`; `ext:110383`–`ext:110388`). В локальном `-p` не приходят: их шлёт рабочий объявленных удалённых инструментов (`cli:968368`–`cli:969775`), а допуск требует `--sdk-url` и `CLAUDE_CODE_REMOTE_SESSION_ID` (`cli:1503124`–`cli:1503131`). `upgrade_relay_marker` — не запрос (раздел 4, вступление).

### 4.8 `ui_copy`, `ui_prompt_read`, `ui_prompt_fill`, `ui_prompt_paint`, `ui_prompt_suggest`, `ui_read_selection`
- Плагин в безголовой сессии просит хост скопировать текст, прочитать или заполнить поле ввода, показать подсказку, прочитать выделение (`cli:113581`–`cli:113740`). У этих запросов, в отличие от остальных, срок есть: `AbortSignal.timeout(n)` (`sendUiHostRequest`, `cli:1468272`–`cli:1468286`). Без `uiHost` SDK отвечает «Unsupported control request subtype» (`ext:110514`–`ext:110538`).

---

## 5. Жизненный цикл

### 5.1 Ход: открытие

1. Клиент пишет `user` с `uuid`. CLI кладёт его в очередь команд: `command_lifecycle queued` (`cli:109518`).
2. Очередь отдаёт пачку в ход: `command_lifecycle started`. Несколько сообщений, пришедших подряд, могут слиться в один ход — тогда `result.user_message_uuid` — последний член пачки, а `user_message_uuids` — все (`d312:5985`–`d312:5989`).
3. `system/init` — в начале **каждого** хода (`cli:1497286`, `cli:1497497`).
4. С `--include-partial-messages`: `system/status requesting` перед каждым запросом к API (`cli:768740`), затем `stream_event`. Без partial — только готовые `assistant`.
5. `assistant` (по блоку на сообщение), `user` с `tool_result`, повторение — пока модель вызывает инструменты.

### 5.2 Ход: закрытие

- Ровно один `result` на ход — сигнал конца хода (`d312:6010`–`d312:6013`); `result_index` нумерует результаты процесса с 0, пропуск номера — потерянный результат (`d312:6002`).
- После `result` ещё могут прийти `task_*`, `session_state_changed`, `prompt_suggestion`, `rate_limit_event` и т. п.
- Подтипы `result` и `terminal_reason` — 2.1. Ход прерванный — как правило `error_during_execution` с `aborted_streaming`/`aborted_tools` (2.1); ход, где отказал хук остановки, — `stop_hook_prevented`/`hook_stopped`; модель попросила уйти в фон — `background_requested` (`d312:10128`).
- `queued_turn_count > 0` — после этого `result` CLI сам начнёт следующий ход из очереди (`d312:5975`).
- Конец «работы» сессии для клиента — не `result`: у вкладки ход считается законченным, когда `result` не ошибочный, нет неотвеченных отправок и нет не-ambient фоновых задач (`FQ`, `ext:123862`–`ext:123881`).

### 5.3 Ходы, которые CLI начинает сам

- **Уведомление о задаче.** Конец фоновой команды или субагента ставит в очередь служебную команду `mode: "task-notification"` с `priority: "next"` (`oUt`, `cli:582323`–`cli:582339`; `Qbt = "next"`, `cli:368747`). Она открывает ход как обычное сообщение; у его `result` — `origin.kind: "task-notification"` (на это опирается вкладка, `ext:114647`). Модель получает текст `<task-notification>…`; для субагента, у которого ещё работает своя фоновая работа, туда добавляется: «This agent stopped with background work of its own still running. It may resume on its own when that work completes or reports, and the same task-id notifies again if it does; the result below may be interim.» (`cli:582289`). Самого уведомления клиент в таком ходе не видит: кадра `user` в stdout нет даже с `--replay-user-messages`, `command_lifecycle` нет, `status requesting` идёт без `user_message_uuids`; приходят `init`, `status`, ответ модели и `result` с `origin {kind:"task-notification", producer:"session-task", runId}` (вживую, `t5-p12-bg` строка 114, так же в `p9-now-bash-human` и `t2-subagent-progress`); текст `<task-notification>…` есть только в транскрипте — строкой `user` с этим `origin`. Уведомление, влитое посреди другого хода, приходит эхом `user` с `isReplay:true`, `origin.kind:"task-notification"` и **без** `isSynthetic`; его `uuid` — `commandUuid` из `queue-operation remove absorbed_mid_turn` (`t5-midturn-notification`, строка 62).
- **Ход без модели.** Если отчёт субагента уже доставлен главному агенту вызовом `SubagentHandback` (handback `send`, получатель — главный поток, флаг `tengu_hushed_kestrel`), уведомление идёт с `shouldQuery:false` (`cli:582337`, `cMt` — `cli:422524`–`cli:422531`). Такой ход не зовёт API и закрывается `result success` с `num_turns:0`, `duration_api_ms:0`, `stop_reason:null`, `result:""` или текстом локальной команды, `total_cost_usd` — накопленным итогом, `usage` — нулевым (`hr = Pm`, `cli:1497127`; нули — `cli:211266`–`cli:211279`, `Pm` разрешается именно сюда) (`cli:1497509`–`cli:1497553`). Вкладка узнаёт такой ход по `num_turns === 0 && origin.kind === "task-notification"` (`ext:114647`). Это **случай 3**: строка расхода «0 → 0 tokens» — это ход без модели, а не стоимость.
- **Сработавший таймер** (`ScheduleWakeup`, `CronCreate`): `command_lifecycle started` с `wakeup_source` и `scheduled_for` (`cli:109498`–`cli:109505`). Таймер `ScheduleWakeup` живёт только в процессе: в живом процессе срабатывает (`x6-wake-control`, `wakeup_source: loop_wakeup`), после SIGTERM и `--resume` — ни в срок, ни после (`x6-wake-kill`, `x6-wake-resume`); на диске его нет. В hqc то же после выгрузки по простою (пробная сессия `agent-trader/session-7`, 2026-11-11).
- **Перезапуск прерванного хода** после падения процесса — только при `CLAUDE_CODE_RESUME_INTERRUPTED_TURN`; у `result` — `resume_reason` (`d312:5993`).
- Сообщения из очереди, которые CLI добавил сам (таймеры, автопродолжения), могут появляться в `still_queued` квитанции `interrupt` — незнакомые `uuid` надо пропускать (`d312:4848`).

### 5.4 Прерывание

- `interrupt` (3.3) прерывает ход; его `result` — как правило `error_during_execution` (2.1), квитанция приходит раньше результата.
- Сообщения в очереди **не** пропадают: они в `still_queued` и исполнятся следующими ходами, если не снять их `cancel_async_message` или `cancel_queued:true`.
- Фоновые задачи: без `perTaskStopAffordance` прерывание убивает фоновых субагентов и workflow, каждый присылает `task_notification stopped` (`Kle`, `cli:742336`–`cli:742345`); с ним — щадит, останавливает только агентов-наблюдателей (`isObserver`: `qp`, `cli:330873`–`cli:330875`; `JQt`, `cli:742347`–`cli:742350`). Прерывание взводит таймер `keep_alive` (`ai.arm()`, `cli:1504654`), но в локальном `-p` кадров нет: без `CLAUDE_CODE_REMOTE` таймер выключен (2.4).
- Вопрос о правах, висевший в момент прерывания: ожидание обрывается сигналом хода, и CLI шлёт `control_cancel_request` (`cli:1468145`–`cli:1468148`); отложенный вопрос снимает `np` → `rt.fire("interrupt")` (`cli:1508547`–`cli:1508552`).
- `deny` с `interrupt:true` в ответ на `can_use_tool` прерывает ход так же (`cli:1458710`–`cli:1458711`).
- Строка `[Request interrupted by user for tool use]` приходит в stdout кадром `user` **без** `isSynthetic` (вживую, `t5-p12-bg` строка 77): клиент, который прячет служебное по `isSynthetic`, её покажет.
- Ход, взявший сообщение в момент прерывания, может вернуть его в очередь, если у сообщения был `requeue_if_interrupted_within_ms` (`cli:106677`–`cli:106683`; в квитанции — `put_back`).

### 5.5 Очередь ввода и сообщения посреди хода (случай 5)

- Сообщение, пришедшее во время хода, ждёт в очереди (`command_lifecycle queued`).
- **Вливание** происходит в цикле запроса после того, как отработала пачка вызовов инструментов (`aborted_tools` и хук `PostToolBatch` — `cli:762387`–`cli:762397`), и перед следующим запросом к модели: команды из очереди превращаются во вложения `queued_command` (`AGe`, `cli:525584`), снимаются с очереди с причиной `absorbed_mid_turn` и отмечаются `started` (`cli:762448`–`cli:762527`). В разговоре сообщение встаёт после `tool_result` — поэтому «вставлено после результата инструмента». На последней итерации, разрешённой `--max-turns`, вливания нет (`cli:762457`–`cli:762459`).
- **Эхо** влитого сообщения (`user`, `isReplay:true`, его `uuid`, `isSynthetic` для служебных, `origin`) рождается из этого вложения в момент вливания (`cli:1489207`–`cli:1489210`, `vAr` — `cli:723351`–`cli:723364`) — то есть позже, чем сообщение было отправлено, и после `tool_result`.
- **Во время финального ответа** (модель пишет текст без вызовов инструментов) следующего запроса в этом ходе нет, проход вложений не случается, сообщение остаётся в очереди и открывает **свой ход** после `result`. Это следует из места вливания: блок вливания стоит в ветке, куда цикл попадает только после вызовов инструментов; отдельного правила «не вливать во время ответа» в коде нет. Вживую подтверждено (2.1.312, haiku, `--replay-user-messages`): сообщение, отправленное во время финального ответа, в текущий ход не входит — `result` первого хода, затем CLI сам начинает второй ход с ним: эхо, ответ, свой `result` (`docs/SPEC.md` §2.4, замеры 2026-11-05).
- `result.user_message_uuids` перечисляет и влитые сообщения (`d312:5987`–`d312:5989`).
- В начале хода CLI тоже вливает подходящие команды из очереди (`absorbed_mid_turn`, `cli:759596`–`cli:759606`).
- `priority:"now"` пытается доставить сообщение сразу, не дожидаясь вызова инструмента (3.2).
- `command_lifecycle` по сообщению: `queued` → `started` → `completed` (ход, куда оно вошло, кончился чисто) или `cancelled` (ход прерван или упал — даже если сообщение уже было влито); точное правило — описание схемы (`cli:109518`).

### 5.6 Фоновые задачи и субагенты (случаи 1–3)

- **События задачи:** `task_started` (регистрация) → `task_updated` (патчи) → `task_progress` (для субагента) → `task_notification` (конец). Плюс снимок `background_tasks_changed`.
- **`run_id`:** у каждого запуска свой; формат — время в base36 и 4 случайных байта (`qL`, `cli:138693`–`cli:138696`), строки одной задачи сортируются по времени; кадр со старым `run_id` — опоздавший (`d312:6378`).
- **Одна заявка на уведомление.** `ca()` выпускает `task_notification` только если заявка свободна (`s8r`, `cli:138697`–`cli:138700`). Когда задача снова оживает (субагента разбудила его фоновая команда, сообщение пользователя, продолжение), `vo()` снимает заявку, выдаёт новый `run_id` и **снова шлёт `task_started`** с тем же `task_id` (`cli:332850`–`cli:332851`, `cli:332890`–`cli:332895`).
- **Bash субагента в фоне (случай 1).** Bash-инструмент передаёт потолок `subagentCapMs: _Lr(...)` (`cli:587148`). `_Lr` возвращает значение флага `tengu_maple_ferry` (по умолчанию 0) только для асинхронного субагента, не для главного потока и не для вложенного вызова, и только если `WP()` и `KNe()` ложны (`cli:518285`–`cli:518299`). `WP()` = `Se() && !(Ap() && !claudecode) && !$m()` (`cli:36943`–`cli:36946`), `Se()` здесь — импорт из модуля `cli:4393`–`11551` (`cli:36567`): `!launchOptions.isInteractive()` (`cli:9442`–`cli:9444`), в `-p` истинна. `Ap()` — точка входа из набора `claude-desktop`, `claude-desktop-3p`, `local-agent` (`cli:36654`, `cli:36685`–`cli:36688`, `cli:36906`–`cli:36908`), `$m()` — `claude-vscode` (оба — без `childSession`/`claudecode`). Значит, в `-p` `WP()` ложна, и потолок действует при `claude-vscode`, `claude-desktop`, `claude-desktop-3p` и `local-agent`; при `sdk-cli`, `cli` и прочих потолка нет. Воркер hqc ставит `claude-vscode` (`hqc:crates/hqc-worker/src/adapter/claude_code.rs:105`–`106`) — у него потолок тот же, что во вкладке. `p7n` берёт `min(запрошенный таймаут, max(потолок, 2000 мс))` (`cli:377239`–`cli:377250`); по истечении команда не убивается, а уходит в фон с текстом «Command was moved to the background (ID: …) after running for Ns» (`cli:518320`). Сколько это секунд, определяет сервер: закэшированное значение флага на agent-node-227 и на ноутбуке — `180000` мс (`cachedGrowthBookFeatures` в `~/.claude.json`, кэш от 2026-11-11), что и даёт наблюдённые ~180 с. Попутно вживую: `Agent` без `run_in_background` при `claude-vscode` тоже ушёл в фон (`tool_use_result {isAsync:true, status:"async_launched"}`, `task_started is_backgrounded:true`, `t2-subagent-progress` строка 47; без этой точки входа не проверялось). Bash в переднем плане отказывает в команде, если первая подкоманда — `sleep N` с N ≥ 25: `tool_result is_error:true` «Blocked: sleep 45 followed by: … To wait for a condition, use Monitor…» (`mLr = 25`, `cli:518186`; проверка `cli:587021`–`cli:587034`; на `run_in_background` не действует).
- **Промежуточный отчёт (случай 2).** Когда у субагента есть живая своя фоновая работа — вложенный агент, workflow или (при флаге `tengu_concurrent_shore`, по умолчанию true) фоновая Bash (`vyt`, `cli:582087`–`cli:582097`; `LK`, `cli:464962`–`cli:464964`), его конец хода — промежуточный: `handback = "withheld"`, в текст отчёта идёт «This agent has not reported yet: it is waiting on its own background work and will deliver its report through SubagentHandback when that finishes.» (`Ynr`, `cli:195067`; имя инструмента `Yh = "SubagentHandback"`, `cli:194867`; выбор — `cli:727680`), а в `task_notification` поле `handback` для такого случая выбрасывается (`cli:728549`). Статус уведомления — `completed`.
- **Окончательный отчёт.** Когда фоновая работа субагента кончилась, он просыпается (в типах 0.3.319 это поле `backgroundWakesOwner` результата Bash: «the subagent that owns this backgrounded command parks for its end and is woken by it», `/Users/vvzvlad/Data/Projects/hqc_dev/vendor-src/sdk-0.3.319/sdk-tools.d.ts:3318`–`3321`; новый `run_id`, `task_started`), дописывает ответ и доставляет его главному агенту вызовом `SubagentHandback`: `handback: "send"` (или `"flagged"` с предупреждением), `handback_report` — сам отчёт, текст для модели — «This agent's report was delivered to you as a message from "…" (its SubagentHandback call). Read it there; it is not repeated here.» (`woi`, `cli:195059`–`cli:195065`; `cli:727677`–`cli:727679`). Уведомление о таком завершении идёт с `shouldQuery:false` — **ход без модели (случай 3)**.
- **Если после отчёта у субагента ещё работает команда** — он снова «остановился с фоновой работой» и проснётся, когда она кончится: тот же цикл `task_started` → `task_notification` с новым `run_id` (`cli:582289`).
- **Остановка:** `stop_task`, прерывание без `perTaskStopAffordance`, закрытие stdin с превышением срока (5.9) — `task_notification stopped`.

### 5.7 Сжатие контекста (случай 6)

- **`/compact`** (текстом `user`) — локальная команда, ход без запроса к модели основного цикла: `status compacting` (статусы сжатия есть и у набранного `/compact`, `cli:108230`–`cli:108234`) → `compact_boundary` → сводка как `user` со строковым `content`, `isReplay:false` и `isSynthetic:true` (`AC`/`CC`, `cli:1489308`–`cli:1489327`, `cli:1489282`–`cli:1489293`; `isSynthetic` = `isMeta || isVisibleInTranscriptOnly || isCompactSummary`, `uXe`, `cli:723153`) — по этим трём признакам её ловит вкладка (раздел 8, «выжимка сжатия») → `status` с `compact_result` → `result`. `compact_output_chars` здесь нет (`cli:108230`–`cli:108234`).
- **Автокомпакция** в ходе: `status compacting` (с `compact_output_chars` по мере написания сводки) → `compact_boundary` (`cli:768744`) → `status null` с `compact_result: success|failed`, `compact_error` (`cli:768731`–`cli:768738`). Вживую (`p10-autocompact`, `CLAUDE_CODE_AUTO_COMPACT_WINDOW=100000`, порог 67000 — сходится с формулой `docs/SPEC.md` §2.4): эхо промпта, открывшего ход, приходит **до** границы, и `logical_parent_uuid` границы — `uuid` этого промпта; затем `compact_boundary {trigger:"auto", pre_tokens, post_tokens}` и сводка — `user` с `isSynthetic:true`, **без ключа `isReplay`**, `content` — **массив** из одного `text` (у `/compact` — строка и `isReplay:false`); дальше ход продолжается (`assistant`, `result success`, `num_turns:2`). `compact_output_chars` по ходу сжатия пришёл только `0`. Сжатие может не выйти: первая попытка дала `compact_result:"failed"`, `compact_error:"too_few_groups"`, и ход пошёл дальше без сжатия при заполнении 165 % окна; следующий ход сжался. В транскрипте обе сводки — `user` с `isCompactSummary` и `isVisibleInTranscriptOnly` и строковым `content`.
- `compact_boundary.compact_metadata` (внутреннее) несёт триггер и число токенов; `logical_parent_uuid` связывает с цепочкой до сжатия.
- Порог автокомпакции клиенту в локальном `-p` не сообщается: `autocompact_state` есть только при `CLAUDE_CODE_REMOTE` (`cli:1504978`–`cli:1504990`).

### 5.8 `--resume` (случай 6)

- Каждый ход снова приходит `system/init`; `total_cost_usd` и `modelUsage` продолжаются с итога, сохранённого в транскрипте (`d312:5958`–`d312:5962`).
- **Сироты прошлого процесса** (`KYt`, `cli:1353895`): по фоновым командам, у которых в транскрипте нет записи о завершении, CLI сам ставит в очередь уведомления `status:"stopped"` с текстом «Background shell command didn't finish before the previous session ended» и телом «No completion record was found for it in the previous session. It may have been stopped (via the UI, Monitor timeout, or agent teardown — these leave no transcript marker), or it may have been running when the previous Claude Code process exited…» (`cli:1354470`–`cli:1354486`, `shouldQuery:false`). Если таких много, одно сводное, каждой — `task_notification stopped` с `summary` «Orphaned by a previous Claude Code process exit and reported in an aggregate summary.» (`cli:1354466`, текст — `cli:1353889`). По фоновым субагентам — `stopped` или `failed` (`cli:1354304`).
- Значит, после `--resume` клиент получит `task_notification` о задачах, которых в этом процессе не было (`task_started` им не предшествует).

### 5.9 Завершение процесса

- **stdin закрыт:** CLI доделывает очередь, затем ждёт фоновые задачи не дольше `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS` (по умолчанию 600000 мс — `BBt`, `cli:333401`; `vFs`, `cli:403484`–`cli:403486`) и останавливает их с сообщением в stderr «Background tasks still running … after the last turn (…); stopping them. Set CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0 to wait indefinitely.» (`cli:1483505`–`cli:1483540`).
- **`end_session`** — 3.3.
- **Фатальный ввод** — невалидный JSON, `user` с чужой ролью, `control_request` без `request` (3.1).
- Пока stdin закрыт, CLI не шлёт запросов к клиенту: `sendRequest` бросает «Stream closed» (`cli:1468139`), вопрос о правах становится отказом (`ze`, `cli:333613`).
- **Коды выхода** (вживую, `p14-*`): закрытие stdin — 0 и до хода, и после, и посреди (CLI доделывает ход до `result`); `end_session` в простое — 0, посреди хода — **1** (`control_response success`, затем `result error_during_execution` с `aborted_tools`, сообщение — `cancelled`); невалидный JSON — 1 («Error parsing streaming input line … SyntaxError» в stderr), `control_request` без `request` — 1 («Error: Missing request on control_request»), `user` с ролью `assistant` — 1 («Error: Expected message role 'user', got 'assistant'»); во всех трёх фатальных случаях stdout пуст. По коду ещё: 1 — последний `result` с `is_error: true` (`cli:1504381`), ошибки флагов и старта со строкой в stderr (`--resume-session-at` без `--resume`, `stream-json` без `--verbose`, сессия занята или не найдена, `--permission-prompt-tool` не найден — `cli:1503460`–`cli:1503889`, `cli:1513031`–`cli:1513908`), строка stdin длиннее `maxLineChars` (`cli:1467401`), сбой записи в stdout (`cli:1513495`–`cli:1513523`), петля необработанных исключений (`cli:408799`–`cli:408826`); 0 — SIGINT (`cli:1504872`–`cli:1504881`) и простой дольше `CLAUDE_CODE_EXIT_AFTER_STOP_DELAY` (`cli:1498489`–`cli:1498495`); 143 — SIGTERM (`cli:1504882`–`cli:1504887`); 129 — SIGHUP (`cli:408750`–`cli:408763`); 70 — необработанное исключение при `CLAUDE_CODE_SUPERVISED` (`cli:408787`–`cli:408797`, `iRt = 70` — `cli:68726`). Код ставится один раз: поздний выход его не меняет (`cli:408880`–`cli:408881`). 75 и 78 бывают только у Remote Control и демона (`cli:68724`–`cli:68725`).

### 5.10 Повторы API, лимиты, запасная модель

- Повтор запроса: `system/api_retry` с `attempt`, `max_retries`, `retry_delay_ms`, `error_status` (`null` — без HTTP-ответа), `no_response` (`cli:768796`–`cli:768805`). Повторы исчерпаны — `assistant` с `error` и `result success` с `is_error:true`, `api_error_status`, `terminal_reason` `api_error`/`model_error`/`prompt_too_long` (`cli:768983`–`cli:768990`).
- Повтор внутри субагента — `tool_progress` с `subagent_retry` (`cli:723779`–`cli:723790`).
- Ожидание лимита подписки фоновыми запросами — «пульс» `api_retry` с `error_status:429, error:"rate_limit"` (`cli:1502915`, `cli:1502937`–`cli:1502955`).
- Статус лимитов — `rate_limit_event` при каждом изменении (`cli:1486662`–`cli:1486685`).
- Смена модели: `model_fallback` (недоступна), `model_consent_fallback` (с согласия), `model_refusal_fallback` (отказ модели, с `retracted_message_uuids` — какие показанные сообщения отозваны), `model_refusal_no_fallback`.

### 5.11 Показ размышления (случай 4)

- Флаг `--thinking-display <summarized|omitted|highlights>` скрыт из справки (`cli:807312`, значения — `Ust`, `cli:104883`).
- Без флага: в интерактивном режиме — `summarized` при настройке `showThinkingSummaries`; в неинтерактивном с `text`/`json` (без `--verbose`) — `omitted`; в stream-json — **не задано**, решает API (`KXo`, `cli:144218`–`cli:144223`). Для opus 5.5 на 2.1.312 это пустой текст размышления (подтверждено вживую, `hqc:docs/SPEC.md:2248`–`hqc:docs/SPEC.md:2250`).
- С `summarized`: блоки `thinking` в `assistant` несут текст сводки; с `--include-partial-messages` он идёт кусками `stream_event` (`content_block_delta` / `thinking_delta`) раньше готового `assistant`. `assistant` и `stream_event` несут внутреннее `thinking_display`, `assistant` — `thinking_duration_ms` (`cli:723345`–`cli:723349`).
- Сменить показ на ходу — `set_max_thinking_tokens {thinking_display}` (3.3).
- Независимо от показа идёт оценка объёма — `system/thinking_tokens` (`cli:768702`–`cli:768713`).

---

## 6. Что изменилось в типах 0.3.319 против 0.3.312

Сверка `diff` двух `sdk.d.ts` (247 изменённых строк) и `sdk-tools.d.ts` (38 строк).

| Что | Где в 0.3.319 | Смысл |
| --- | --- | --- |
| новый член union `SDKSessionWritesLostMessage` (`system/session_writes_lost`) | `d319:6274`, union `d319:5666` | CLI не смог записать транскрипт или `~/.claude.json` (диск полон, квота); шлётся раз, при выходе с кодом 0. В 2.1.312 его нет |
| новый запрос `set_has_subscribers {has_subscribers: boolean|null}` | `d319:5382` | хост сообщает, смотрит ли кто-то сессию |
| `pending_elicitation_requests` в ответах `control_response` | `d319:429`, `d319:456` | незакрытые `elicitation` при подключении, как у вопросов о правах |
| `hook_on_failure_supported` в ответе `initialize`; `onFailure` у сопоставителя хука | `d319:4838`; `d319:1025`, `d319:5514`, `d319:7523`, `d319:7625` | что делать, если колбэк хука упал |
| `check_permissions` у `mcp_call` | `d319:4960` | вызов MCP с обычной проверкой прав |
| `lossy` в ответе `read_file` | `d319:5185` | чтение было с потерями (utf-8 по двоичному) |
| `credential_expires_at` в ответах на обновление токенов | `d319:6081`, `d319:6206` | срок учётных данных |
| `task_progress`: «Summaries are not a heartbeat» | `d319:2162` | пауза в сводках не значит остановку |
| расширено описание `still_queued` | `d319:4896` | — |
| `can_use_tool`: `suppressAlwaysAllowRule` у плагинов, имена других серверов в `matched_ask_rule` | `d319:288`, `d319:308`, `d319:5112` | — |
| инструменты (`sdk-tools.d.ts`): удалено `file_unchanged`; добавлены `backgroundWakesOwner` (фоновая команда разбудит владеющего ею субагента), `outsidersMayHaveWritten`, `page_token`/`next_page_token`, ещё 2 вхождения `timedOut`; `status` стало необязательным | `sdk-tools.d.ts` 0.3.319: 3321, 457, 463 | `backgroundWakesOwner` — тот самый механизм случая 2, теперь виден в типах |

---

---

# Часть II. Транскрипт JSONL

## 7. Транскрипт

### 7.1 Где лежат файлы

| Что | Путь | Код |
|---|---|---|
| Каталог проекта | `~/.claude/projects/<slug>/`, `slug` — cwd, где каждый символ кроме `[a-zA-Z0-9]` заменён на `-`; длиннее 200 символов — обрезка и `-<хэш>` | `ext:26826`, `ext:27395` (`sp = 200`), `cli:231947` |
| Транскрипт сессии | `<slug>/<sessionId>.jsonl` | `cli:411636` (`cm`), `cli:231950` (`up`) |
| Субагент | `<slug>/<sessionId>/subagents/agent-<agentId>.jsonl`; у субагентов воркфлоу — `subagents/workflows/<runId>/agent-<id>.jsonl` (подкаталог из `agentTranscriptSubdirs`) | `cli:231963`, `cli:231968` (`pm`), `cli:721103` |
| Метаданные субагента | рядом, `agent-<agentId>.meta.json` | `cli:411650` |
| Большие выводы инструментов | `<slug>/<sessionId>/tool-results/` (в транскрипте вместо вывода — `<persisted-output>` с путём) | `cli:64897`, `cli:64910` |
| Удалённые агенты | `<slug>/<sessionId>/remote-agents/remote-agent-<id>.meta.json` | `cli:411815`–`411820` |
| Воркфлоу | `<slug>/<sessionId>/workflows/` (`scripts/`, `journal.jsonl` — по строке `{"type":"result",…}` на агента) | `cli:247843`, `cli:802789`, `cli:950958` |
| Прочие файлы в каталоге сессии | `custom-title.json`, `precompact.json`, `sent-prefix.json`, `ccr-tip.json` | `cli:836829`, `cli:333575` |

По файлам: 12 533 `.meta.json` субагентов, ключи `agentType` (12 533), `spawnDepth` (12 502),
`description` (11 376), `toolUseId` (10 921), `model` (4 995), `requestShape` /
`requestNonInteractive` (4 608), `finished` (1 164), `isBuiltIn` (786), `parentAgentId` (783),
`workflowPhase` (455), `pickedEffortModel` (351), `stoppedByUser` (77), `effort` (14), `isFork`
(8), `launchedWithDefaults` (2). Файлов субагентов прямо в `subagents/` — 10 921, в
`subagents/workflows/<runId>/` — 1 688. В `tool-results/` — 7 840 `.txt`, 904 `.jpg`, 287 `.png`,
57 `.pdf`, 53 `.json`. В каталогах сессий найдено 6 `custom-title.json`. Файлы
`<sessionId>.jsonl.latest` (1 637 штук) пишет не Claude Code, а logovo
(`/Users/vvzvlad/Data/Projects/logovo/scan.go`; в бинаре 2.1.312 строки `jsonl.latest` нет:
`grep -a -c` = 0).

### 7.2 Как пишется строка беседы

- Все строки `user`/`assistant`/`system`/`attachment` основной сессии идут через
  `insertMessageChain` (`cli:413793`). Для каждой строки собирается объект (`cli:413853`–`413866`):
  `parentUuid` (у границы сжатия — `null`), `logicalParentUuid` (только у границы сжатия),
  `isSidechain`, `teamName`, `agentName`, `promptId` (только у `user`), `agentId`, затем все поля
  самого сообщения (`MAi`, `cli:415947`), затем `sessionKind` (`bg`/`daemon`/`daemon-worker` из
  `CLAUDE_CODE_SESSION_KIND`), `userType: "external"`, `entrypoint` (= `CLAUDE_CODE_ENTRYPOINT`),
  `cwd`, `sessionId`, `version` (`L$r`, `cli:411965`; `g0e`, `cli:411980`), `gitBranch`, `slug`.
- `parentUuid` — `uuid` предыдущей строки той же пачки (или подсказки родителя, переданной
  записывающему), но: строка `user` с `sourceToolAssistantUUID` получает родителем эту строку
  `assistant`, если она уже записана (`cli:413847`–`413851`); строки `progress` родителями не
  становятся (`kQ`, `cli:411586`, `cli:413876`).
- Перед записью `ZC` (`cli:414390`) отбрасывает сообщения, чьи `uuid` уже есть в сессии, и
  сдвигает подсказку родителя на последнее уже записанное (`cli:414407`–`414417`); `appendEntry`
  ещё раз проверяет `uuid` по множеству сессии (`cli:414093`, `cli:414107`). Поэтому повтор
  того же сообщения (эхо, повторная отправка с тем же `uuid`) второй строки не даёт.
- `toolUseResult` у `user` перед записью может быть урезан: `originalFile` длиннее порога
  заменяется на `null` (`QPt`, `cli:593141`, `cli:413868`).
- Субагент: `BGe` (`cli:414461`) пишет пачку с `isSidechain: true` и `agentId`, файл —
  `subagents/agent-<agentId>.jsonl` (`cli:414094`–`414095`); проверки на повтор `uuid` для них нет
  (`cli:414108`). Первая строка файла субагента — `user` с `parentUuid: null` (12 535 таких строк
  на 12 541 файл).
- Служебные записи (`always`) пишутся в файл как есть; `content-replacement`, `api-request*`,
  `fork-context-ref`, `observer-ref` с `agentId` уходят в файл субагента (`route-by-agent`,
  `cli:414069`–`414087`).
- Пока файла сессии ещё нет, записи копятся в `pendingEntries` (`cli:414046`–`414048`); файл
  создаётся при первой строке `user`/`assistant`/`system` (`cli:413802`–`413807`).

### 7.3 Виды записей

Таблицу строит `scripts/table.py` из двух таблиц политик CLI и обоих проходов сканера.
«Политика файла» — что делает переписывание файла после сжатия (`transcript` — строки цепочки,
`boundary-cleared` — до границы выкидываются, `last-wins` — оставляется последняя копия,
`accumulate` — все). «Политика записи» — `appendEntry` (`dedup-transcript` — запись только нового
`uuid`, `always` — всегда, `route-by-agent` — в файл субагента при `agentId`). Последний столбец:
для `user`/`assistant`/`system`/`attachment`/`api-request`/`api-request-shape` — записи с
`version: "2.1.312"`; для записей без поля `version` — все записи этого вида в файлах, где есть
хоть одна запись 2.1.312 (такие файлы дописывали и другие версии, поэтому столбец — верхняя оценка).

| Вид записи | Политика файла (409328) | Политика записи (411518) | Где создаётся, cli:строка | Всего, сессии | Всего, субагенты | В файлах с 2.1.312 |
|---|---|---|---|---:|---:|---:|
| `user` | transcript | dedup-transcript | 413793 insertMessageChain | 367907 | 770134 | 17376 |
| `assistant` | transcript | dedup-transcript | 413793 insertMessageChain | 648952 | 1270083 | 29081 |
| `system` | transcript | dedup-transcript | 413793 insertMessageChain | 43122 | 1319 | 800 |
| `attachment` | transcript | dedup-transcript | 413793 insertMessageChain | 203078 | 121902 | 6633 |
| `progress` | boundary-cleared | dedup-transcript | (44 мест создания, в файлах 0) | 0 | 0 | 0 |
| `file-history-snapshot` | boundary-cleared | always | 413892 | 20427 | 0 | 887 |
| `file-history-delta` | boundary-cleared | always | 414887 | 8476 | 0 | 468 |
| `last-prompt` | boundary-cleared | always | 413165, 414420 | 115161 | 0 | 10253 |
| `continued-in` | boundary-cleared | always | 333504 | 0 | 0 | 0 |
| `content-replacement` | accumulate | route-by-agent | 413921, 967194 | 0 | 0 | 0 |
| `api-request-shape` | boundary-cleared | route-by-agent | 413988 | 2400 | 2169 | 442 |
| `api-request-blob` | boundary-cleared | route-by-agent | 414007 | 43567 | 88771 | 14599 |
| `api-request` | boundary-cleared | route-by-agent | 405848 | 75328 | 110344 | 15166 |
| `fork-context-ref` | accumulate | route-by-agent | 414471 | 0 | 0 | 0 |
| `frame-link` | accumulate | always | 413233 | 4453 | 0 | 2123 |
| `summary` | last-wins | always | ext 58548 (fork) | 0 | 0 | 0 |
| `custom-title` | last-wins | always | 413171, 416933; ext 58372 | 6198 | 0 | 531 |
| `ended-by-model` | last-wins | always | 416922 | 0 | 0 | 0 |
| `ai-title` | last-wins | always | 413172, 416947 | 96670 | 0 | 10119 |
| `tag` | last-wins | always | 413173 | 0 | 0 | 0 |
| `relocated` | last-wins | always | 413175, 414979 | 431 | 0 | 0 |
| `agent-name` | last-wins | always | 413177, 417530 | 591 | 0 | 530 |
| `agent-color` | last-wins | always | 413179, 417541 | 0 | 0 | 0 |
| `agent-setting` | last-wins | always | 413181 | 114865 | 0 | 10181 |
| `pr-link` | last-wins | always | 413225, 416959 | 4157 | 0 | 1492 |
| `artifact-comment-monitor` | accumulate | always | 710549 | 136 | 0 | 16 |
| `artifact-autoreact-ledger` | last-wins | always | 409476, 711645 | 554 | 0 | 100 |
| `bridge-session` | last-wins | always | 413242 | 0 | 0 | 0 |
| `history-suppression` | last-wins | always | 58308 (wlt), 413239 | 0 | 0 | 0 |
| `attribution-snapshot` | last-wins | always | 418121 (буфер) | 0 | 0 | 0 |
| `mode` | last-wins | always | 413182 | 47633 | 0 | 5567 |
| `permission-mode` | last-wins | always | 413184 | 13 | 0 | 0 |
| `isolation-latch` | last-wins | always | 413186 | 0 | 0 | 0 |
| `dev-mods` | last-wins | always | 413188 | 0 | 0 | 0 |
| `memory-mode` | accumulate | always | 413196 | 0 | 0 | 0 |
| `atis-latch` | last-wins | always | 413207, 413828 | 91471 | 0 | 10179 |
| `tools-used` | last-wins | always | 413218, 413835 | 2298 | 0 | 1005 |
| `worktree-state` | last-wins | always | 413222, 417726 | 450 | 0 | 0 |
| `cost-state` | last-wins | always | 437971 | 1341 | 0 | 198 |
| `queue-operation` | last-wins | always | 421420 | 108788 | 0 | 9500 |
| `observer-ref` | last-wins | route-by-agent | 414484 | 0 | 0 | 0 |
| `session-owner` | — | — | нет в 2.1.312 (пишет 2.1.319) | 7587 | 0 | 2560 |

Что записи значат и когда пишутся (без строк беседы — о них 1.4):

- **Метаданные сессии** — `last-prompt` (`lastPrompt` до 200 символов, `leafUuid`; явный — с
  `explicit: true` и при откате `rewound: true`, `cli:414420`), `custom-title`, `ai-title`, `tag`,
  `relocated` (`relocatedCwd` после смены папки, `cli:414979`), `agent-name`, `agent-color`,
  `agent-setting`, `mode`, `permission-mode`, `isolation-latch`, `dev-mods`, `memory-mode`
  (`afterUuid`), `atis-latch`, `tools-used`, `worktree-state`, `pr-link`, `frame-link`
  (`artifactCount`), `history-suppression`, `bridge-session`. Все собирает
  `planReAppendSessionMetadata` (`cli:413123`–`413292`) и дописывает в хвост, если запись
  отличается от последней такой же в последних 32 КиБ файла (`cli:413259`–`413290`). Повод — каждые
  `hw/2` = 32 768 байт записанного (`cli:412948`, `hw = 65536`, `cli:57178`), выход, создание
  файла, после переписывания файла и переезда (`cli:413613`, `cli:413723`, `cli:413757`,
  `cli:413765`). Отсюда 115 161 `last-prompt` и 96 670 `ai-title` на 1 633 сессии.
  `atis-latch` и `tools-used` дополнительно пишутся перед пачкой строк беседы, если изменились
  (`cli:413818`–`413837`).
- **`queue-operation`** — журнал очереди ввода (`SNo`, `cli:421415`): `operation`
  (`enqueue`/`dequeue`/`remove`), `content` (текст), `reason` (`absorbed_mid_turn` — взято
  посреди хода, `delivered_to_agent` — передано агенту; `cli:762510`, `cli:1491873`), `commandUuid`,
  `deliveryId`. В файлах с 2.1.312: `enqueue` 4 764, `dequeue` 2 610, `remove` 2 126 (из них
  `absorbed_mid_turn` 1 597, `delivered_to_agent` 184).
- **`file-history-snapshot`** (`messageId`, `snapshot`, `isSnapshotUpdate`, `cli:413892`) и
  **`file-history-delta`** (`messageId`, `snapshotMessageId`, `trackingPath`, `backup`,
  `cli:414886`) — снимки файлов для отката правок; привязаны к `uuid` строки `user` через
  `messageId`.
- **`api-request`**, **`api-request-shape`**, **`api-request-blob`** — запись запросов к API для
  воспроизведения (`writeRequestReplayRecords`, `cli:413976`; `api-request` — `cli:405842`):
  форма запроса (system + tools) по хэшу, куски сообщений по хэшу, сам запрос со ссылками на них
  (`params`, `base`, `tail`, `keep`, `cacheControl`, `requestDigest`, `querySource`). С `agentId` —
  в файл субагента. Ключи `api-request` в файлах с 2.1.312 — по 17 143 у каждого из перечисленных.
- **`cost-state`** — итог расхода сессии (`totalCostUSD`, длительности, `modelUsage` по моделям,
  `cli:437966`); при чтении проверяется схемой (`cli:409287`, `cli:409526`).
- **`content-replacement`** — замены содержимого инструментов (`replacements`, `agentId`,
  `cli:413921`); **`fork-context-ref`** (`cli:414471`) и **`observer-ref`** (`cli:414483`) — ссылки
  на чужой контекст; **`continued-in`** (`continuedInSessionId`, `cli:333503`) — сессия
  продолжилась в другой; **`ended-by-model`** (`cli:416920`); **`artifact-*`**,
  **`attribution-snapshot`** — служебные записи артефактов и атрибуции. В файлах ноутбука ни одной
  из `content-replacement`, `fork-context-ref`, `observer-ref`, `continued-in`, `ended-by-model`,
  `attribution-snapshot`, `bridge-session`, `history-suppression`, `tag`, `agent-color`,
  `isolation-latch`, `dev-mods`, `memory-mode` нет (0 по `scan.py`).
- **`summary`** — старый вид (`summary`, `leafUuid`): CLI 2.1.312 его только читает (ключ
  `leafUuid`, `cli:409497`), пишет его расширение при развилке сессии (`ext:58546`–`58550`). В
  файлах — 0.
- **`progress`** — в таблицах есть, мест создания 44, но на диск 2.1.312 его не пишет: в файлах
  всех версий 0 строк, и в 28 файлах живых прогонов на стенде (1 104 записи, среди них Bash 70 с с
  выводом, субагент с Bash, хуки Stop) — тоже 0.
- **`session-owner`** (`accountUuid`, `organizationUuid`) и `repl-mode` — пишет 2.1.319 (9
  вхождений `session-owner` в его бинаре), в 2.1.312 их нет (`grep -a -c` = 0). В файлах —
  7 587 `session-owner`.

### 7.4 Строки беседы: поля

Общие поля всех четырёх видов (по `insertMessageChain`, см. 7.2): `type`, `uuid`, `parentUuid`,
`isSidechain`, `timestamp`, `sessionId`, `version`, `cwd`, `gitBranch`, `userType`, `entrypoint`,
`slug` (не у всех), `agentId` (у субагентов), `teamName`/`agentName` (команды), `sessionKind`.

**`user`.** Конструктор — `Ie` (`cli:588636`–`588732`), полный список полей сообщения:
`message {role, content}`, `isMeta`, `promptShellHandOff`, `mcpSkill`, `invokedSkill`,
`ephemeral`, `turnCompanion`, `usageLimitNote`, `replacesSpan`, `isVisibleInTranscriptOnly`,
`isVirtual`, `isCompactSummary`, `repeatsCompactSummary`, `modelOnlyText`, `summarizeMetadata`,
`toolUseResult`, `classifierMetaLines`, `classifierBoundary`, `serverClassifierContext`,
`hostClassifierContext`, `toolDenialKind`, `toolDenialEndsTurn`, `toolDenialMonitorBlocked`,
`toolDenialUnanswered`, `userFeedback`, `toolResultRemedy`, `permissionDecision`,
`sandboxViolations`, `sandboxed`, `mcpMeta`, `toolEndsTurn`, `imagePasteIds`,
`sourceToolAssistantUUID`, `permissionMode`, `origin`, `promptSource`, `promptId`,
`interruptedMessageId`, `refusedMessageId`, `interruptedByShutdown`. В записи добавляются ещё
поля очереди и хода — `mergedSendUuids`, `turnAttributionKey`, `clientPlatform` (`kA`,
`cli:1495861`), `queueSkipAttachments`, `queueTranscriptOnly` (`cli:594335`, `cli:594341`),
`turnOrigin`, `turnPosition`, `sourceToolUseID`, `summarizedTurnPromptUuids`,
`joinedNotice*`, `wakeupSource`.
Встречено у 17 376 строк `user` 2.1.312 (`scan2.py`, ключи записей с `version` 2.1.312):
`permissionDecision` 16 123, `sourceToolAssistantUUID` 16 143, `sandboxed` 10 905, `toolUseResult`
8 157, `serverClassifierContext` 5 731, `mcpMeta` 1 044, `permissionMode` 631, `promptSource` 631,
`turnOrigin` 624, `turnPosition` 593, `isMeta` 525, `origin` 468, `queueSkipAttachments` 332,
`turnAttributionKey` 308, `turnCompanion` 201, `toolEndsTurn` 62, `queueSkipSlashCommands` 53,
`joinedNotice{Lengths,Own,RunIds}` 53, `isCompactSummary` 38, `isVisibleInTranscriptOnly` 38,
`queueTranscriptOnly` 37, `toolDenialKind` 31, `sourceToolUseID` 29, `imagePasteIds` 19,
`summarizedTurnPromptUuids` 14, `classifierBoundary` 5, `mergedSendUuids` 1,
`classifierMetaLines` 1. По всем версиям ещё `interruptedByShutdown` (59 в сессиях),
`refusedMessageId` (54), `wakeupSource` (49), `toolDenialUnanswered` (1).
`promptSource` в 2.1.312: `system` 318, `sdk` 313 (по всем версиям ещё `typed` 6). `origin.kind`
по всем версиям: `human` 19 088, `task-notification` 14 828, `peer` 1 557. `toolDenialKind`, по
тексту самого CLI (`cli:788572`): `user-rejected`, `permission-rule`, `automode-*`, а
`interrupted`/`cancelled` — это остановка, не отказ.
`toolUseResult` — структурный результат инструмента (не то, что видит модель): по всем версиям в
сессиях `dict` 261 717, `list` 42 013, строка 17 515 (из них начинаются с `Error:` 8 912,
`User rejected` 388, `InputValidationError` 39).
`isMeta: true` — строка для модели, а не реплика человека: по всем версиям в сессиях
`[Image #N]`-подписи к картинкам 1 419, сообщения других сессий («Another Claude session…»)
1 505, `<local-command-caveat>` 355, `<system-reminder>` 85, прочее 3 997 (`scan2.py`,
`meta_user_class`). Последнюю реплику человека CLI ищет, пропуская `isMeta`, `isCompactSummary`,
встроенные команды и тексты вида `<тег …>`/`[Request interrupted by user…]` (`K4n`,
`cli:415897`; регулярка `yAt`, `cli:411496`). `isVisibleInTranscriptOnly: true` — у 1 519 строк
сессий, ровно столько же, сколько выжимок (`isCompactSummary`); CLI ставит оба флага вместе
(`cli:483086`–`483087`). `isSidechain: true` — у всех строк файлов субагентов.

**`assistant`.** Строка — один блок содержимого API-сообщения; общий `message.id`, номер блока —
`apiBlockIndex`. Поля в 29 081 строке 2.1.312: `requestId` 29 046, `effort`/`requestedModel`/
`requestRef`/`apiBlockIndex` 29 065, `perTurnEffort` 29 071, `wireToolInputs` 16 145,
`attributionAgent` 13 919 (у субагентов), `serverClassifierRequest` 12 498,
`thinkingDurationMs` 11 201, `thinkingDisplay` 11 200, `attributionSkill` 2 953,
`attributionMcpServer`/`attributionMcpTool` 2 825, `wireIngestContext` 1 487, `isApiErrorMessage` 16,
`error` 6, `apiErrorStatus` 6, `apiErrorIsTransient` 5, `apiErrorCode` 1, `resendCannotCure` 1. На
agent-node-227 ещё `isAbortedMidStream: true` (2 строки) — ответ оборван на середине (`cli:506817`,
`cli:723092`; в кадре потока — `aborted`, `cli:723317`).
Синтетический ответ (не от модели) строит `AYn` (`cli:588415`): `message.model: "<synthetic>"`
(`ki`, `cli:6506`), `message.id` — новый случайный, `stop_reason: "stop_sequence"`, нулевой
`usage`, плюс `apiError`, `apiErrorParams`, `apiErrorIsTransient`, `quotaLimits`, `error`,
`errorDetails`, `truncatedAfterOutput`, `isApiErrorMessage`, `isVirtual`.

**`system`.** Подвиды в файлах (все версии, сессии): `stop_hook_summary` 36 105, `api_error`
5 271, `compact_boundary` 1 519, `informational` 114, `model_refusal_fallback` 44,
`local_command` 33, `model_refusal_no_fallback` 28, `model_consent_fallback` 4, `turn_duration` 4.
В 2.1.312: `stop_hook_summary` 693, `api_error` 60, `compact_boundary` 38 (31 + 7 у субагентов),
`informational` 9. `api_error` — строка повтора запроса (`w2`, `cli:593380`: `error`, `retryInMs`,
`retryAttempt`, `maxRetries`, `source`, `level: "error"`). `compact_boundary` — `c2`
(`cli:593367`), см. 1.5.

**`attachment`.** Поле `attachment` с собственным `type`; в 2.1.312 у строки ещё `rendered`,
`renderedRole` (3 685 из 6 633), `renderedInHumanTurn` (313), `renderedBesideToolResult` (170).
Видов `attachment.type` в файлах — 58; частые: `output_style` 74 080, `deferred_tools_record`
31 810, `queued_command` 14 234, `todo_reminder` 8 760, `hook_additional_context` 7 289. Для
разбора важен `queued_command` — сообщение, взятое из очереди посреди хода (см. 7.6):
`prompt`, `commandMode` (`prompt`/`task-notification`), `origin`, `isMeta`, `source_uuid`
(`uuid` присланного сообщения), `delivery_id`.

### 7.5 Цепочка: ветки, сжатие, откат и развилка, resume, субагенты

- **Обычный ход.** `user` → `attachment`… → `assistant` (блок 0) → `assistant` (блок 1) … Строка
  `tool_result` висит на строке с `tool_use`, поэтому у одной строки `assistant` двое детей:
  следующий блок того же сообщения и `tool_result`. Это не развилка. Родителей с двумя и более
  детьми `user`/`assistant` в файлах сессий (`scan2.py`, `branch`): пара `assistant` +
  `tool_result` — 10 952 (741 из них в файлах с 2.1.312); настоящие развилки — два `assistant`
  (1 903), несколько `tool_result` (891), два `user` (157), `assistant` + `user` (42). В 2.1.312 у всех 8 048 строк
  с `sourceToolAssistantUUID` он совпадает с `parentUuid`; в записях других версий не совпадал у
  34 437.
- **Сжатие** (`c2`, `cli:593367`; строка-выжимка — `cli:483086`, `cli:492469`;
  цепочка — `cli:413840`–`413855`). Граница: `type: "system"`, `subtype: "compact_boundary"`,
  `content: "Conversation compacted"`, `parentUuid: null`, `logicalParentUuid` — последняя строка
  до сжатия, `compactMetadata` (в 2.1.312: `trigger` 31 из 31, `preTokens`, `postTokens`,
  `cumulativeDroppedTokens`, `durationMs` — 31; `preCompactDiscoveredTools`, `preservedSegment`,
  `preservedMessages` — 30; `runningTurnSends` 17; `precomputed` 8). У всех 38 границ 2.1.312
  `parentUuid` = `null`, `logicalParentUuid` задан, в 23 из 31 он найден в том же файле. Остальные
  8 — ошибка самого CLI: `logicalParentUuid` берётся из памяти (`r.at(-1)?.uuid`, `XNr`,
  `cli:483771`–`483772`), а на диск пишутся только сообщения, прошедшие фильтр `EE` (`cli:420187`–`420201`,
  `cli:420269`–`420273`): он выкидывает `progress`, вложения `hook_success` без вывода,
  `system` с `persist: false` и др. Если последнее сообщение в памяти — такое, граница ссылается на
  строку, которой на диске нет (у предвычисленного сжатия берут `r.findLast(EE)?.uuid`, `cli:494420`,
  — там ошибки нет). У всех 8 последняя записанная строка перед границей — `user(tool_result)`, у
  найденных 23 `logicalParentUuid` совпадает с последней записанной строкой; у субагентов 2.1.312 — 8
  из 9 без родителя, по всем версиям — 463 из 1 488 в сессиях и 1 031 из 1 312 у субагентов. Дальше
  в 2.1.312: ребёнок границы — `attachment/instructions` (30 из 31), цепочка вложений, затем строка
  `user` с `isCompactSummary: true` и `isVisibleInTranscriptOnly: true`, `isMeta` нет, `origin`
  нет, `content` — строка «This session is being continued from a previous conversation…»
  (`cli:195277`), её родитель — `attachment/date` (30 из 31). В записях других версий выжимка —
  прямой ребёнок границы в 824 + 759 случаях (сессии + субагенты), после `attachment/date` — в
  664 + 553. `preservedSegment` (`headUuid`, `anchorUuid`, `tailUuid`)
  и `preservedMessages` (`uuids`, `anchorUuid`) — объекты у 38 из 39 найденных границ: сохранённый
  хвост остаётся в файле выше границы, при чтении читатель SDK переставляет `parentUuid` его
  головы на `anchorUuid`, а детей якоря — на хвост (`ext:57041`–`57063`).
- **Переписывание файла после сжатия** (`performCompactTranscript`, `cli:413480`–`413625`):
  файл ≥ 5 МиБ (`PGs`, `cli:411561`), выигрыш ≥ 10 % (`rdo`, `cli:411564`), строки до последней
  границы выкидываются, кроме сохранённого хвоста, его снимков и «last-wins» метаданных
  (`c$e`, `cli:409485`); запись во временный `<file>.compact.tmp.<hex>` и rename поверх
  (`cli:413484`, `cli:413606`), дописанное во время работы переносится. Включается только
  `store.localGcEnabled`: в `-p` без `sdkUrl` — при `CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1`
  (`cli:793877`), с `sdkUrl` — по той же переменной или флагу `tengu_transcript_local_gc`
  (`cli:1470799`, `cli:1513768`). По умолчанию файл только дописывается.
- **Откат и развилка.** Откат (rewind) пишет явный `last-prompt` с `leafUuid` и `rewound: true`
  (`O1t`, `cli:414420`); читатель SDK по `rewound` и `cutUuid` убирает отрезанную ветку
  (`JTe.readCheckpoint`, `ext:57537`–`57555`). В файлах `rewound` — 0. Развилку расширения
  (`forkSession`, `ext:58393`) пишет новый файл: копия цепочки с новыми `uuid`, переписанными
  `parentUuid`, `logicalParentUuid`, `source_uuid`, `compactMetadata`, затем `summary`
  (`leafUuid`) и `custom-title` «… (fork)» (`ext:58520`–`58552`).
- **Resume.** Тот же `sessionId` — тот же файл (`cm`, `cli:411636`); родителя новой пачки
  передаёт вызывающий, CLI запоминает последний лист (`currentSessionLeafUuid`, `cli:413883`) и
  пишет его в `last-prompt.leafUuid`. Если файл дописывали два
  процесса, у одного родителя появляются два ребёнка (cc_remote `docs/SPEC.md` §2.4, способ B).
  Одинаковый `uuid` дважды в одном файле встретился: `assistant` 2 040, `user` 1 271,
  `attachment` 621, `system` 103 строк (`scan.py`, `dup_uuid`). Все 4 035 — в 39 файлах сессий
  VS Code (`entrypoint: claude-vscode`) версий 2.1.209–2.1.280; в версиях новее 2.1.280 — 0 на
  690 879 записей, у субагентов и на нодах — 0. Копия всегда позже оригинала (на 11–10 000 строк),
  с тем же `timestamp` и `sessionId`, но с `cwd`/`version`/`slug`/`promptId` соседних строк и не
  пришита к текущей цепочке; между ними всегда есть `last-prompt`. Похоже, старые куски беседы
  заново переписывал процесс, который вёл сессию позже (гипотеза: кода тех версий нет). В 2.1.312
  так быть не должно: множество `uuid` для сверки строится по всему файлу и дозаполняется при
  возобновлении (`cli:412184`–`412199`, `t5n` — `cli:419655`, сверка — `cli:414093`–`414107`).
  Сканеры — `dupscan.py`, `dupscan2.py` в `vendor-src/scripts/`.
- **Какой лист берёт CLI при `--resume`.** Не так, как читатель SDK во вкладке (тот берёт последнюю
  строку файла без детей, 8.2 п. 5). `--resume` в `-p` → `CHe` → `oDe` → `roe(path)` без
  `keepAllLeaves` (`cli:1513834`, `cli:419598`, `cli:419464`–`419482`). Отметка — `leafUuid`
  последнего `last-prompt` (`explicit` сбрасывается любой строкой беседы после неё; `leafUuid: null`
  с `explicit` — сессия очищена; граница сжатия отметку сбрасывает; `cli:419054`–`419072`). Выбор
  (`zr`, `cli:419188`–`419285`): очищенная сессия — пустой разговор; если отметки в файле нет вовсе,
  а самая поздняя по времени строка — не последняя по файлу, берётся она; иначе кандидат — отметка
  (если она не явная, а последняя строка файла — её прямой потомок, то последняя строка); при
  нескольких листьях — отметка, если она среди них, иначе последняя строка файла. От кандидата вверх
  до ближайшего `user`/`assistant`; цепочка — по `parentUuid`, при оборванном родителе — подбор по
  времени (`tengu_chain_timestamp_fallback`, `cli:416065`–`416087`). Если в один файл писали два
  процесса, CLI продолжит с `last-prompt.leafUuid` последнего писавшего, а вкладка — с последней
  строки файла без детей: ветки могут разойтись.
- **Субагенты.** Свой файл, `isSidechain: true`, `agentId`, первая строка `user` с
  `parentUuid: null`. В основном файле от субагента — только вызов `Agent`/`Task`, его
  `tool_result` и уведомления.

### 7.6 Граничные случаи, найденные на практике

**Сообщения, которые CLI взял из очереди вместе.** При выдаче из очереди (`cli:1484811`–`1484840`)
следующие `prompt` подтягиваются к первому, и при нескольких сообщениях `pp` (`cli:1481797`)
склеивает их: `value` — тексты через `\n` (`WU`, `cli:1481790`), `uuid` — **`uuid` последнего
сообщения, у которого он есть** (`e.findLast((R) => R.uuid)`), `origin` первого. `Y_`
(`cli:1489525`) кладёт `uuid` остальных в `mergedSendUuids`, строка `user` пишется с этим `uuid`
(`MKn`, `cli:1497334`) и полем `mergedSendUuids` (`kA`, `cli:1495861`). С
`--replay-user-messages` остальные получают эхо `isReplay` со своими `uuid` (`replayMerged`,
`cli:1508008`–`1508024`). В файлах 6 строк с `mergedSendUuids` (1 — 2.1.312), во всех 6 свой
`uuid` в список не входит. Вживую (`t1-*`, сообщения `$U2` и `$U3` отправлены с интервалом 0,5 с):
- склейка бывает, только когда сообщения ждут в очереди вместе — например, пришли во время
  финального ответа; тогда в транскрипте одна запись `user` с `uuid = $U3`,
  `mergedSendUuids = ['$U2']` и текстом обоих через `\n`; с `--replay-user-messages` приходят два
  эха — `$U2` со своим текстом (сразу при выдаче из очереди, после `result` прошлого хода) и `$U3` со
  **склеенным** текстом обоих; `result.user_message_uuid = $U3`, `user_message_uuids = [$U2, $U3]`;
  без replay эха нет, оба `uuid` — в `command_lifecycle`, `status.user_message_uuids` и `result`;
- посреди команды склейки нет: каждое сообщение — своё вложение `attachment/queued_command` с
  `source_uuid` и `queue-operation remove absorbed_mid_turn`, эхо у каждого своё, в момент
  вливания; так же пишется сообщение, взятое посреди хода, а в поток идёт `command_lifecycle started`
  (`cli:762476`–`762516`);
- два кадра на старте процесса (через 0,3 с и одной записью в stdin) не склеились: первый CLI взял
  в ход через 4–5 мс после `queued`, второй влил посреди хода.

Во всех снятых случаях `uuid` записи поток называл. Запись «с uuid, которого поток не называл» из
DrZlo13/hqc#42 (коммит `feb3cf3`) этим не объяснена.

**`<task-notification>`.** Уведомление о фоновой задаче — обычная строка `user`: `content` —
строка `<task-notification><task-id>…<status>…</status><summary>…`, `isMeta` нет,
`origin.kind: "task-notification"` (14 828 по всем версиям), `promptSource` `system`/`sdk`. В
2.1.312: 266 таких строк (`completed` 210, без статуса 32, `stopped` 16, `failed` 8). Если
уведомление пришло посреди хода — это `attachment/queued_command` с
`commandMode: "task-notification"` (в 2.1.312 — 134 в сессиях, 179 у субагентов). После
`--resume` CLI просматривает восстановленные сообщения на фоновые задачи без итога (`KYt`,
`cli:1353895`) и кладёт уведомление с текстом «… didn't finish before the previous session ended»
(`cli:1353892`); в файлах 35 таких строк, у всех `<status>stopped</status>` (11 — 2.1.312).

**Сжатие.** Записи — граница, вложения, строка-выжимка (7.5). Выжимка — `message.content`
строки `user` с `isCompactSummary: true`. Счётчики сходятся: границ в сессиях 1 519, строк
`isCompactSummary` 1 519, `isVisibleInTranscriptOnly` 1 519 (`scan.py`).

**«[Request interrupted by user]» и отказ в инструменте.** Строку строит `Vv` (`cli:588739`):
`user`, `content` — список с одним текстом `[Request interrupted by user]` (`gk`) или
`[Request interrupted by user for tool use]` (`Zu`, `cli:95475`), `isMeta` нет. Отказ в вызове —
`tool_result` с `is_error: true` и текстом «The user doesn't want to proceed with this tool use…»
(`Ty`, `cli:95518`), `toolUseResult: "User rejected tool use"` (`kAt`, `cli:95520`),
`toolDenialKind`; следом строка `[… for tool use]`: 354 из 358 таких строк в сессиях висят на
`tool_result` (`scan2.py`, `interrupt`). Просто `[Request interrupted by user]` висит на `assistant`
(214), на `tool_result` (171), на `user` (153), на вложениях. Весь список «текстов прерывания»,
которые CLI считает своими, — `BPe` (`cli:95494`, функция `nm`, `cli:95505`): ещё «session ended before
this agent finished», «Tool call did not complete…», «Tool call interrupted: the session
ended…», «Tool call result not in this copy…», «The user doesn't want to take this action right
now…», два «Tool call skipped…». У сообщений, прерванных выходом процесса, —
`interruptedByShutdown: true` (59 строк по всем версиям).

**Ответы `<synthetic>`.** Не от модели, а от CLI (`AYn`, `cli:588415`). По всем версиям 811 в
сессиях; по тексту: «API Error…» с `isApiErrorMessage` и `error`
(`rate_limit`/`server_error`/`invalid_request`/`authentication_failed`/`unknown`…), «You've hit
your…» (`rate_limit`), «Credit balance…» (`billing_error`), «No response requested.» (`d2`,
`cli:6505`; 214, без `isApiErrorMessage`). В 2.1.312 — 16 строк (`scripts/scan2.json`): «No response
requested.» 10, «API Error» с `rate_limit` 3 и `verification_required` 1 в сессиях, «API Error» с
`rate_limit` 2 у субагентов.

**Локальные команды.** `/cost` и подобные пишутся тремя строками `user`: `isMeta: true` с
`<local-command-caveat>…` (`Xye`, `cli:588748`), затем без `isMeta` — `<command-name>/cost</command-name>
<command-message>…<command-args>…` и `<local-command-stdout>…</local-command-stdout>` (по всем
версиям 356 / 356 / 333 строки, из них в 2.1.312 — 1 / 1 / 1). Изредка вывод — строка
`system/local_command` (33 на ноутбуке, все — других версий; на agent-node-227 — 4 строки 2.1.312 с
`<local-command-stdout>`, 7.7). Вызов навыка (`<command-message>` с `isMeta`, список)
— 328 у субагентов.

### 7.7 Транскрипты воркеров hqc

Те же сканеры (`scan.py`, `scan2.py`, `dupscan.py`, `cbscan.py`, `vescan.py`) прогнаны 2026-11-12 по
`~/.claude/projects/` пользователя `claude` на пяти нодах; результаты — `vendor-src/scripts/nodes/<адрес>/`,
сравнение — `scripts/table312.py`. Записи воркера hqc на 2.1.312 есть только на agent-node-227 (46
файлов сессий `claude-vscode`, 3 331 запись, и 12 файлов субагентов); на 226, agent-node-3, 129 и
vvzvladmbpm1 — по нескольку файлов старых интерактивных сессий. Другого каталога у них нет:
`CLAUDE_CONFIG_DIR` (`hqc:crates/hqc-worker/src/adapter/claude_code/paths.rs:54`–`56`) нет ни в окружении
службы `hqc-worker` (`systemctl show`, проверялось только имя переменной), ни в plist vvzvladmbpm1, ни в
`~/.hqc/worker.toml`, а каталог настроек у пользователя `claude` один — `~/.claude`. Значит, сессий
hqc на этих воркерах просто почти не было.

| 2.1.312, строки | Ноутбук (2026-11-11) | Ноды (2026-11-12) |
| --- | ---: | ---: |
| `assistant` / `user` | 29 081 / 17 376 | 1 097 / 1 017 |
| `<synthetic>` (из них «No response requested.») | 16 (10) | 2 (1) |
| `[Request interrupted by user …]` | 0 | 2 |
| отказ в инструменте (`toolUseResult: "User rejected tool use"`) | 0 | 2 |
| `interruptedByShutdown` | 0 | 2 |
| `system/local_command` | 0 | 4 |
| `assistant` с `isAbortedMidStream` | 0 | 2 |
| `mergedSendUuids` (свой `uuid` не в списке) | 1 | 2 |
| границ сжатия (с `logicalParentUuid` в своём файле) | 38 (24) | 2 (2) |
| `message.id` в файлах с 2.1.312 (разбиты на несколько строк) | 31 215 (22 308) | 952 (139) |
| развилок «`assistant` + `tool_result`» | 864 | 0 |
| повторов `uuid`, все версии | 4 035 | 0 |

Отличия воркера: сообщения API реже разбиты на строки (меньше ответов с размышлением —
`thinkingDisplay` у 117 из 1 034 строк `assistant` против 5 846 из 15 160), поэтому нет развилок
«`assistant` + `tool_result`»; у всех 381 `queue-operation enqueue` есть `content` (на ноутбуке 1 243
без него), `remove` — только с причиной; в файлах сессий нет `agent-setting`, `session-owner`,
`frame-link`, `pr-link`, `custom-title`, `agent-name`, `file-history-delta`. Совпадает:
`sourceToolAssistantUUID` = `parentUuid` у всех 569 строк, `<task-notification>` — строки `user` с
`origin.kind: task-notification` и `promptSource: system`, битых строк нет. Ноутбук снят на сутки
раньше нод (за это время на нём прибавилось 9 файлов).

---

# Часть III. Как вкладка VS Code разбирает сообщения

## 8. Как вкладка понимает сообщения

### 8.0 Путь сообщения и модель строки

- Хост расширения читает поток CLI (`ext:125619`–`125731`) и шлёт во вкладку каждое сообщение
  как `io_message` (`ext:125715`), кроме: `system/bridge_state` (`ext:125642`–`125653`);
  `system/hook_started|hook_progress|hook_response` всех событий, кроме `SessionStart` и `Setup`
  (`DVe`, `ext:123961`; `b3`, `ext:50679`); `system/session_title_changed` — хост сам ставит
  название (`ext:125708`–`125712`). Сам добавляет `system/status` с
  `userPromptSubmitHooksRunning` — число открытых хуков `UserPromptSubmit` (`ext:125654`–`125665`,
  `LVe`, `ext:123968`).
- Вкладка раскладывает `io_message` по каналу (`web:154285`–`154295`); `auth_status` с
  `isAuthenticating` попутно ставит окно входа AWS. Каждое сообщение — в своём `try`
  (`web:170880`–`170890`). Упавшее при разборе `user`/`assistant`, для которого в ленте ещё нет
  строки (`hasRowForFrame`), даёт строку «Couldn't display a message here.» (подряд — «Couldn't
  display N messages here.», `web:167702`, `web:170930`–`170937`), кроме `user` с
  `parent_tool_use_id` и синтетики с `origin` не `peer`/`channel` (`mGe`, `web:167721`); упавший
  `result` всё равно закрывает ход (`web:170915`). В консоль — «Skipped a message (…) that failed
  to process» (первые 5, `web:167766`).
- Строка ленты — `Ap` (`web:160591`): `type` ∈ `user`/`assistant`/`meta`/`compact`/
  `refusal_fallback`, блоки `Js` (`web:160702`, у блока `tool_use` — свой `toolResult`),
  `uuid`, `betaMessageId` (= `message.id`), `blockIndex`, `parentToolUseId`,
  `sdkParentToolUseId`, `hiddenFromChat`, `isSynthetic`, `isSynthesizedByLoop`, `origin`,
  `interruptedByShutdown`, `notAPrompt`, `foldedIntoTurn`, `failure`, `compactMetadata`,
  `compactSummary`. Перевод сообщения в строку — `Ky` (`web:160526`–`160576`), вставка в ленту —
  `j8` (`web:172020`–`172075`), отрисовка — `qA` (`web:261240`–`261318`).
- Пустая строка (`isEmpty`, `web:160683`–`160694`) не рисуется: нет блоков, или все блоки —
  `tool_result`, или все — недописанный `tool_use`, или весь текст пуст / «(no content)».

Дальше — по видам сообщений. «Живой» — обработка в `processIncomingMessage` (`web:171028`) и
`processMessage` (`web:171277`); «история» — при открытии вкладки (8.2).

### `user` (живой, без `isReplay`)

- `Ky`: `parentToolUseId` = `parent_tool_use_id` (только у `user`), `isSynthetic` =
  `isSynthetic` или (`is_meta` и есть `origin`), `notAPrompt` = `is_meta` или
  `isCompletedLocalCommand`, `interruptedByShutdown`, `origin`; строковый `content` → один
  текстовый блок; блоки `type: "fallback"` выкидываются (`web:160527`–`160565`).
- `j8`: каждый `tool_result` находит свой `tool_use` (поиск с конца ленты по строкам `assistant`,
  `lme`, `web:171759`) и кладёт результат в него (`setToolResult`) с `interruptedByShutdown` и
  `toolDenialUnanswered`; `tool_use_result` передаётся только для `AskUserQuestion` и только если
  `tool_result` в сообщении один (`web:172021`–`172039`). Затем строка `user` добавляется в ленту
  как есть; если в ней только `tool_result`, она пустая и не рисуется.
- `tool_result` основной сессии во время хода ставит «ход с инструментами»
  (`turnHadToolRound`, `web:171176`–`171184`) — от него зависит, куда встанет эхо (см.
  `isReplay`). `tool_result` вызова `Artifact` с адресом открывает его в браузере
  (`web:171198`–`171217`).
- Отрисовка (`web:261243`–`261245`): строка с `parentToolUseId` (подсказка субагенту) не рисуется;
  `isSynthetic` не рисуется, кроме сообщения другой сессии (`PD`, `web:162269`). Блоки
  `image`/`document` рисуются вложениями (`web:261026`).
- Текстовый блок разбирается `pc` (`web:161386`–`161445`) в порядке: ровно
  `[Request interrupted by user]` → «Interrupted», ровно `[Request interrupted by user for tool
  use]` → «Tool interrupted» (`Xk`, `web:161382`); `<task-notification>…</task-notification>` →
  карточка уведомления (`AD`, `web:161462`: `task-id`, `tool-use-id`, `status`, `summary`,
  `result`, `usage` из `subagent_tokens`/`tool_uses`/`duration_ms`; лишний текст после тега или
  `<` внутри `summary` — не карточка); сообщение другой сессии; обёрнутые уведомления
  (`<teammate-message>`, `<bash-stdout>`, `<bash-stderr>`, `<tick>`, `mcp-resource-update`,
  `mcp-polling-update`, `<local-command-caveat>`, `Qjt`, `web:161338`–`161351`); `<ide_selection>`,
  `<ide_opened_file>`, `<terminal …>`, `<browser …>`; `<local-command-stdout>X</…>` → результат
  команды, `<local-command-stderr>` → результат с ошибкой (`Aqt`, `web:161888`–`161934`);
  `<post-tool-use-hook><ide_diagnostics>` → диагностика; иначе текст, а
  `<command-name>…<command-args>…` превращается в строку `/команда аргументы` (`Eqt`,
  `web:161845`; `wUe`, `web:160916`).

### `user` с `isReplay: true`

Эхо своего сообщения (`--replay-user-messages`), `web:171151`–`171174`:
- Если строка с этим `uuid` уже есть (вкладка сама его отправила): текст заменяется пришедшим
  (`aUe`, `web:160577`), `createdAt` — по `timestamp` эха; новой строки нет.
- Если нет (сообщение пришло не из этой вкладки): строка вставляется в
  `replayInsertIndex` (конец последнего законченного хода) — или в конец ленты, если сообщение не
  человеческое (`vGe`), ход начат этой вкладкой или в ходе были инструменты; не человеческое
  сообщение во время хода после ответа помечается `foldedIntoTurn`.
- Эхо снимает ожидание у «отложенных» сообщений до него включительно (`heldPickupFor`,
  `web:169228`–`169231`) и, если ответа ещё не было, считается началом хода.
- Эхо своего сообщения, ждавшего названия сессии (`unnamedSends`), даёт сессии название, если
  текст не служебный (`KWe`, `web:160338`; `web:171153`–`171156`).

### `user` — выжимка сжатия (`isCompactSummary`, живой поток)

Сразу после `system/compact_boundary` вкладка держит строку сжатия открытой
(`pendingCompactMessage`); следующий `user` с `isSynthetic` и строковым `content` не попадает в
ленту, а становится текстом свёрнутой строки «Compacted chat» (`web:171144`–`171150`). Любое другое
сообщение, кроме `prompt_suggestion` и `command_lifecycle`, это ожидание снимает. `isCompactSummary`
в основной сессии ещё помечает кэш промпта «сжат» (`web:171286`–`171289`).

### `user` / `assistant` с `parent_tool_use_id` (субагенты)

Строки субагентов идут в ту же плоскую ленту. У `assistant` `parent_tool_use_id` попадает только в
`sdkParentToolUseId`; `hiddenFromChat` ставится только в живом потоке, если
`$Ve(карта агентов, лента, parent_tool_use_id)` истинна (`web:171186`–`171191`, `web:163030`–`163047`);
скрытые строки не рисуются (`web:261269`). Правила `$Ve`:
1. Ищется агент, чей вызов запуска или «пробуждения» — `parent_tool_use_id` кадра
   (`[toolUseId, ...wakeToolUseIds]`, `web:162751`–`162753`; `wakeToolUseIds` пополняются, когда та
   же задача приходит `task_started` с другим `tool_use_id`, `web:162705`–`162735`).
2. Агент фоновый (`isBackgrounded === true`): кадр скрыт, если у самого агента или его фонового
   предка последний вызов запуска/пробуждения лежит в видимой строке ленты как `tool_use`
   `Agent`/`Task`/`Skill`/`skill__*`/`SendMessage` (`web:163049`–`163068`); если такой строки нет
   (вытеснена, скрыта) — кадр показывается.
3. Агент известен, но `isBackgrounded` не определён — показывается.
4. Агент не найден или не фоновый — скрыт, если сам вызов запуска сделан внутри субагента
   (`aj`/`nfe`, `web:162999`–`163011`).

`isBackgrounded` у агентов из истории — по `run_in_background` вызова (есть результат — `false`,
`web:163138`–`163150`), в живом потоке — из `is_backgrounded` в `task_started`/`task_updated`. В
истории строк субагентов нет вовсе (`isSidechain` отбрасывается, `ext:57390`). Потоковые дельты субагента
собираются отдельным сборщиком на каждый `parent_tool_use_id` (`web:163703`–`163715`). Вызовы
`Agent`/`Task` в ответе пополняют карту агентов (`web:171192`–`171197`). На расход, модель,
`TodoWrite`, кэш промпта и «был ответ» влияют только строки без `parent_tool_use_id`
(`web:171283`–`171331`).

### `assistant`

- `Ky`: `betaMessageId` = `message.id`, `model` (не `<synthetic>`), `isSynthesizedByLoop` =
  `message.model === "<synthetic>"`, `failure` из `error`, `api_error`, `api_error_params.remedy`,
  `commandPlugin` из `local_command_run.plugin`, подсказка `narration_hint` `hidden`/`faint` на
  все блоки (`web:160539`–`160564`).
- `narration_hint` читается только у блока `thinking`, чья подпись декодируется в `narration` и
  текст непуст (`U5`, `web:227905`–`227927`): такой блок рисуется не сворачиваемым «Thinking», а
  строкой текста с хвостом « · summarized» (подсказки нет), « · summary» (`faint`) или без хвоста
  (`hidden`) (`web:227929`–`227940`, `web:230724`–`230725`); остальные блоки подсказку не используют.
  В живом кадре её ставит сам CLI, только при `entrypoint: claude-vscode` (`qo`, `cli:723336`–`723342`)
  — то есть и у воркера hqc; в истории хост дописывает её сам: `hidden` для моделей с
  `quizzical_shore` или `opus_5_5_prompt_bundle`, иначе `faint` (`ext:123048`–`123104`, `ext:137571`).
- Вставка (`j8`, `web:172041`–`172071`), если вкладка видела потоковые события: ищется строка с
  тем же `uuid`, иначе потоковая строка без `uuid` с тем же `message.id` и тем же типом первого
  блока (пустой текст не сопоставляется); найденная заменяется, тайминги блоков переносятся. Иначе
  — новая строка в конец.
- Окончательный ответ основной сессии с другим `message.id`, чем у текущей потоковой попытки,
  убирает недописанные потоковые строки той попытки (`retireAbandonedStreamedRows`,
  `web:171092`–`171099`, `web:169512`).
- `supersedes` (список `uuid`) у `assistant` убирает из ленты строки с этими `uuid`
  (`Eue`, `web:153828`; `evictRetractedMessages`, `web:169482`).
- `error` + `api_error`: при `authentication_failed` (не провайдер, не 3p) и шлюзовых
  `gateway_signin_required`/`gateway_session_expired` вкладка открывает вход (`ffe`,
  `web:163669`; `web:171300`–`171311`).
- `TodoWrite` основной сессии → список задач (`web:171312`–`171320`); `usage` → счётчик токенов
  (`web:171321`–`171329`, `updateUsage`, `web:171732`).
- Отрисовка: все блоки — инструменты, скрытые своим рендером, — строка не рисуется
  (`web:261270`). Статус вызова: нет результата и сессия не занята → «failure», `is_error` →
  «failure» (`rVe`, `web:161966`).

### `assistant` с `message.model: "<synthetic>"`

Рисуется как обычный ответ (в `qA` для него исключения нет), но: не считается «ответом» хода
(`turnHadReplyFrame`, `web:171291`–`171295`), не обновляет модель и кэш промпта
(`web:171322`, `web:171330`), не сбрасывает потоковую попытку (`web:171094`), не копируется
кнопкой копирования (`CLe`, `web:234642`). Ошибка (`error`) показывается через `failure`.

### `stream_event`

Сборщик `mfe`/`T$e` (`web:163693`–`163783`), по одному на `parent_tool_use_id` (`"root"` для
основной сессии):
- `message_start` основной сессии — новая попытка: убираются потоковые строки предыдущей попытки,
  которые так и не заменил окончательный `assistant` (повтор запроса, обрыв), запоминается
  `message.id` (`web:163707`–`163711`, `web:168375`–`168384`).
- `content_block_start` — новая строка `assistant` без `uuid`, с `betaMessageId` и `blockIndex`,
  блок помечен недописанным; блоки `type: "fallback"` в ленту не попадают (`web:163735`–`163739`,
  создание строки — `web:168366`–`168370`).
- `content_block_delta` (`yKt`, `web:163798`–`163844`): `text_delta` — к тексту;
  `citations_delta` — к цитатам; `input_json_delta` — копит JSON входа (`tool_use`,
  `server_tool_use`); `thinking_delta` — к `thinking` (у `redacted_thinking` пропускается),
  `estimated_tokens` в дельте — оценка токенов размышления; `signature_delta` — подпись;
  `compaction_delta` — пропускается; дельта не того типа, что блок, — исключение: кадр пропущен,
  лог в консоль (строки-заглушки для `stream_event` нет).
- `content_block_stop` — блок закончен, вход `tool_use` разбирается из JSON (не разобрался —
  остаётся строкой, `xKt`, `web:163846`).
- `message_delta` — `stop_reason`, `usage`; `message_stop` — сборщик сбрасывается.
- Любое `stream_event`, кроме `ping`, отмечает «был ответ» (`web:171291`–`171295`).
- Подряд идущие удачные `Read` склеиваются в «ReadCoalesced» прямо при добавлении строк
  (`lfe`, `web:163298`; `web:168368`).

### `system/thinking_tokens`

`estimated_tokens` → оценка токенов размышления; после первого такого сообщения оценка из
дельт больше не используется (`PKe`, `web:166620`; `web:171279`–`171282`, `web:168371`–`168374`).

### `system/init`

Сессия занята, флаги хода сброшены (`web:171239`–`171247`); режим разрешений, модель,
`fast_mode_state`, `analytics_disabled`. Если `session_id` отличается от текущего — **лента
очищается целиком**: название, сообщения, задачи, карта агентов, расход, кэш промпта
(`web:171347`–`171386`). Первое `init`/`status` с `launchTiming` или `connectSnapshot` отмечает
канал готовым (`web:170881`–`170886`).

### `system/status`

Текст статуса (`status`), если это не счётчик хуков; `launchTiming` — процесс поднялся;
`startupHooksRunning`, `userPromptSubmitHooksRunning`; `sessionHeldElsewhere` — сессию держит
другой процесс; `interruptedTurnLeft` + `newestMessageAt` — вставка «No reply was recorded for
this message…»; `resume_held_for_plan`; `resumeDropped` — сессия не возобновляется;
`permissionMode` (`web:171416`–`171470`).

### `system/compact_boundary`

Новая строка `compact` («Compacted chat · auto|manual · N k tokens freed», `HTt`,
`web:235028`–`235055`) в конец ленты и ожидание выжимки (`web:171141`–`171143`); счётчик токенов —
0, кэш промпта — «сжат» (`web:171473`–`171475`). Без выжимки строка пишет «Conversation was
compacted to free up context.».

### `system/informational`

С `tag: "brass_thimble"` — строка `meta` с `content` (`web:171471`). Иначе строка `meta` только
при `level` не `info`, непустом тексте и если в ленте уже есть беседа (`sme`, `web:172224`;
`web:171481`–`171486`).

### `system/task_started`, `task_progress`, `task_updated`, `task_notification`, `background_tasks_changed`

Строк ленты не дают, только состояние (`web:171551`–`171724`):
- `task_started`: не-фоновые `local_bash`/`local_agent` с `tool_use_id` — «вызов на переднем
  плане»; для `task_type` не `local_agent` запоминается время старта фоновой задачи; для
  `local_agent` — запись в `subagentTasks` (описание, промпт, фон) и в карту агентов.
- `task_progress`: `usage` (токены, вызовы, длительность), `summary`, последние 3 инструмента.
- `task_updated`: `patch.status` `completed`/`failed`/`killed` убирает субагента из
  «работающих»; смена `is_backgrounded` — обновление.
- `task_notification`: итог в карту агентов (`status`, `summary`, `usage`, `output_file`); для
  задач не из карты — исход (`completed` → «finished»), не больше 1000 записей.
- `background_tasks_changed`: фоновые субагенты, которых нет в `tasks`, убираются; список фоновых
  задач (`local_agent`/`local_workflow`) и описаний остальных.

### `system/model_refusal_fallback`, `model_consent_fallback`, `model_fallback`

`model_refusal_fallback` (при включённом гейте) — строка `refusal_fallback` и удаление строк из
`retracted_message_uuids` (`HPe`, `web:153767`; `web:171121`–`171140`). `model_consent_fallback` —
переключение на модель по умолчанию и уведомление (`web:171100`–`171120`). `model_fallback` —
запоминает запасную модель хода (`web:171069`–`171072`).

### `system/notification`, `system/per_turn_effort_changed`

`notification` с ключом `fast-mode-overage-rejected` — выключение быстрого режима и уведомление
(`web:170981`–`171014`); прочие не обрабатываются. `per_turn_effort_changed` — снимает
«ultrathink доступен» (`web:171067`).

### `system/api_retry`, `system/hook_*`, `tool_progress`, `tool_use_summary`, `auth_status`

В разборе вкладки `api_retry` не встречается ни разу (`grep -c api_retry` по webview = 0) —
повтор запроса не виден. `hook_*` до вкладки не доходят (8.0). `tool_progress`,
`tool_use_summary`, `auth_status` строк не дают (`Ky`, `web:160567`–`160575`); других обработчиков
`tool_progress` во вкладке нет (2 вхождения: `Ky` и таблица видов `web:167739`).

### `command_lifecycle`

Строк не даёт (`web:171073`–`171085`). `ran_ahead: true` — у отложенного сообщения пометка «Ran
at once, while Claude was working.»; `started` — отложенные сообщения до этого `command_uuid`
включительно встают в ленту; `completed`/`cancelled`/`discarded`/`refused` — это одно сообщение
встаёт в ленту (`heldPickupFor`, `web:169219`–`169227`). Любой `command_lifecycle` снимает
`command_uuid` с ожидания подтверждения (`web:170964`–`170966`).

### `result`

Конец хода (`endTurn`), `replayInsertIndex` — конец ленты, флаги хода сброшены
(`web:171543`–`171548`, `web:171248`); `total_cost_usd` и `modelUsage[модель]` →
`contextWindow`, `maxOutputTokens` (`web:171487`–`171501`); `fast_mode_state`; `user_message_uuid(s)`
— подтверждение отправленных (`web:170967`–`170971`). Если поток кончился без `result` при занятой
сессии или с неподтверждёнными отправками — ошибка «The connection to Claude Code ended before
this message completed — it may not have been processed, so please send it again.»
(`web:170957`–`170963`).

### `rate_limit_event`

`rate_limit_info` → полоса лимита: `status: "allowed"` — ничего; `rejected` — «You've hit your
5-hour limit · resets …» / «Usage is paused…» / предложение кредитов; иначе при `utilization`
≥ 0,7 — «You've used N% of your …» (`_en`, `web:172185`–`172222`; `web:171509`–`171542`).

### `prompt_suggestion`

Только подсказка следующего сообщения (`web:171040`–`171043`).

### 8.2 История: что вкладка получает при открытии

Цепочка вызовов: `loadFromServer` (`web:168861`) → `getSession` хоста (`ext:128502`) →
`readSessionForHost` (`ext:58318`) → читатель SDK (`ADn`, `ext:87619`; та же логика, что
`getSessionMessages`, `d312:926`) → `readTrailingHookNotices` (`ext:58337`) → ответ
`get_session_response {messages, sessionDiffs, liveInOtherSurface}`.

Читатель SDK (строки первой копии):
1. Читает файл; если он больше 5 МиБ (`qxe`, `ext:27397`) и не задано
   `CLAUDE_CODE_DISABLE_PRECOMPACT_SKIP`, — только часть после последней границы сжатия
   (`m3t`, `ext:56908`–`56923`; учёт сохранённого хвоста — `Vxe`, `ext:27174`).
2. Берёт только строки `user`/`assistant`/`progress`/`system`/`attachment` со строковым `uuid`;
   все остальные виды (7.3) отбрасывает; попутно считает `queue-operation remove
   absorbed_mid_turn` по `deliveryId`/`commandUuid` (`b3t`, `ext:56998`; `_3t`, `ext:56944`).
3. Повтор `uuid` — последняя строка выигрывает (`Map` по `uuid`, `ext:57036`–`57037`).
4. Пришивает сохранённые хвосты сжатия (`ext:57039`–`57064`).
5. Выбирает лист: последняя по порядку в файле строка без детей, не `isSidechain`, не команды, не
   `progress`, не `attachment/fork_briefing`, от которой вверх по цепочке есть `user`/`assistant`;
   иначе — самая поздняя из найденных `user`/`assistant`, предпочитая не `isMeta`
   (`S3t`, `ext:57065`–`57116`). Ветки, не ведущие к этому листу, в историю не попадают.
6. Проходит от листа к корню (`ext:57117`–`57126`) и вставляет строки того же `message.id`,
   оставшиеся на соседних ветках, и `tool_result` к ним (параллельные вызовы, `q6`,
   `ext:57143`–`57264`).
7. Добавляет `attachment/queued_command`, висящие после листа (`A3t`, `ext:57421`).
8. `kA` (`ext:57310`): после `isMeta` с `<local-command-caveat>` строки `<command-name>` и
   `<local-command-stdout|stderr>` получают `isCompletedLocalCommand` (`v3t`, `ext:57290`;
   метки — `GTe`, `ext:56862`); `attachment/queued_command` становится `user` с
   `uuid` = `source_uuid` (или `uuid` вложения), `isQueuedCommand: true`, `origin` (`C3t`,
   `ext:57344`; `BTe`, `ext:56770`).
9. Фильтр (`x3t`, `ext:57386`): только `user`/`assistant` (`system` — лишь при
   `includeSystemMessages`, а хост его не передаёт, `ext:128513`); `isMeta` — только с `origin`
   вида `channel`/`observer`/`observer-activity`/`slack-ping`/`voice-call`/`peer` (`CA`,
   `ext:56834`); `isSidechain` и `teamName` — выкинуть.
10. Форма (`V6`, `ext:57395`): `type`, `uuid`, `session_id`, `message`, `parent_tool_use_id:
    null`, `interruptedByShutdown`, `toolDenialUnanswered`, `isCompactSummary`, `is_meta` (если
    `isMeta`, `isCompactSummary` или `isVisibleInTranscriptOnly`), `isQueuedCommand`,
    `isCompletedLocalCommand`, `timestamp`, `origin`. `toolUseResult` и остальные поля строки во
    вкладку не идут.

Хост дописывает в конец `system/informational` с `preventContinuation: true`, не `info`, которые
висят после последнего сообщения и за которыми нет `user` (`U3t`, `ext:58609`), и в окне VS Code
ставит ответам подсказку `narration_hint` по модели (`stampReplayedFrames`, `ext:128565`; `dVe`,
`ext:123092`; `ext:137571`).

Вкладка (`web:168873`–`168970`) прогоняет каждое сообщение: `system/informational` → строка
`meta`; остальное — `processMessage` (счётчики, задачи), удаление отозванных строк (`hGe`,
`web:171983`) и `j8` без потоковой логики; `foldedIntoTurn` — у `isQueuedCommand`, пришедшего сразу
после `tool_result` того же ответа (`rGe`, `web:167927`–`167948`). Затем склейка `Read` (`C8`,
`web:163312`), обрезка до лимита строк (`ome`/`wGe`), сброс задач и фоновых задач, карта агентов
из истории, восстановление ответов на вопросы (`jpe`, `web:162128`), заметка о кэше промпта,
запуск процесса (`launchClaude`) и, если последний `AskUserQuestion` без ответа, повторный показ
вопроса (`maybeReplayUnansweredQuestion`, `web:168984`; `Upe`, `web:161989`).

Отличия истории от живого потока, следующие из этого:
- **Сжатие в истории не отмечено**: границы нет (это `system`), выжимка приходит строкой `user`
  с `is_meta`/`isCompactSummary`, без `origin`, поэтому `isSynthetic` не ставится и `qA` рисует её
  обычным пузырём пользователя (`oBn`/`YFt`, `web:260968`–`261040`) с полным текстом «This session is
  being continued…»: особой обработки этого текста во вкладке нет, строки «Compacted chat» нет — она
  рождается только из живого `compact_boundary` (`rUe`, `web:160520`–`160522`). `notAPrompt: true`
  убирает её из навигации по сообщениям пользователя (`Qfe`, `web:166883`–`166895`) и из поиска
  «промпта без ответа» (`Sje`, `web:165122`). Прослежено по коду; глазами вкладку не смотрели.
- `isMeta`-строки (`[Image #N]`, `<local-command-caveat>`, напоминания) в истории не видны.
- `<task-notification>` в истории — обычная строка `user`, рисуется карточкой уведомления.
- Расход и счётчик токенов после открытия — только из `usage` ответов истории; `result` в
  файле нет.

### 8.3 Что рисуется, а что нет (сводка `qA`, `web:261240`–`261318`)

| Строка | Рисуется? |
|---|---|
| любая с `isEmpty` | нет |
| `user` с `parentToolUseId` | нет (кроме режима только чтения с текстом) |
| `user` с `isSynthetic` | нет, кроме сообщения другой сессии |
| `user` прочие | да, блоками по `pc` |
| `assistant` с `hiddenFromChat` | нет |
| `assistant`, все блоки — скрытые инструменты | нет |
| `assistant` прочие, в том числе `<synthetic>` | да |
| `meta` | текстом |
| `compact` | свёрнутой строкой «Compacted chat» |
| `refusal_fallback` | плашкой отказа |

## 9. Логика вкладки, не привязанная к одному сообщению

- **Порядок.** Лента — массив в порядке прихода. `replayInsertIndex` — место для эха чужих
  сообщений (конец последнего законченного хода): после `result` = длина ленты
  (`web:171543`–`171544`), при вставке `meta`-строк и вытеснении сдвигается
  (`web:169270`–`169273`, `shiftIndicesPastEvicted`, `web:169503`).
- **Своё сообщение.** `send` (`web:169066`) сразу кладёт строку с новым `uuid`
  (`crypto.randomUUID()`, `web:169114`); если идёт ход, сообщение вкладки ждёт в
  `heldPrompts` (`turnRunningOrQueued`, `web:168020`) и встаёт в ленту по `command_lifecycle
  started`/концу или по своему эху (`web:169128`–`169134`, `web:169215`–`169254`). Повторное
  добавление пропускается, если строка с этим `uuid` уже есть (`appendPickedUp`, `web:169241`).
- **Дубли.** По `uuid`: эхо обновляет свою строку (`web:171152`–`171163`), окончательный
  `assistant` заменяет строку с тем же `uuid` или потоковую с тем же `message.id`
  (`web:172050`–`172069`); отметка «уже есть строка для кадра» — `hasRowForFrame`
  (`web:170946`). Удаление по списку `uuid`: `supersedes`, `retracted_message_uuids`
  (`web:169482`–`169503`). Отдельного дедупа `user` по `uuid` при `j8` нет: два кадра `user` с
  одним `uuid` без `isReplay` дадут две строки (по коду).
- **Лимит строк.** `wGe` (`web:171897`–`171950`), константы `Sq = 600`, `MJt = 300`, `xq = 100`,
  `PJt = 10` (`web:167672`–`167675`), `wq(l) = max(300, 600 − l)`:
  1. До 600 строк — ничего. Строки делятся на три группы: «беседа» (`compact`, `refusal_fallback`,
     `meta`, `assistant` основной сессии с текстом, реплики человека, плюс результаты
     `Agent`/`Task`/`Skill`/`SendMessage` из строк беседы; `web:171767`–`171792`,
     `web:171882`–`171896`), скрытые (`hiddenFromChat`) и прочее.
  2. Бюджеты: беседы больше 600 — остаётся 500; прочее и скрытые — каждая группа до
     `wq(оставленная беседа) − 100`, если превышает `wq(…)` (`web:171901`–`171908`).
  3. Ступени защиты 1–11 (`web:171793`–`171803`): скрытые — 1; обычные — 2; с текстом и `meta` —
     3; законченный вызов `Agent`/`Task` — 4; не-человеческий промпт с `origin` — 5; член группы — 6
     (идущий — 10); ход группы — 7 (идущий — 10); последние 100 строк живой ленты — 8, последние 10 —
     9; недописанные, идущие вызовы и потоковые строки без `uuid` — 9; строки последнего `TodoWrite` —
     11. `tool_result` наследует ступень своего вызова (`web:171915`–`171918`).
  4. Порядок: если беседы больше 500 — сначала её строки от старых к новым (кроме `TodoWrite`), затем
     всё не-беседа до первой оставшейся строки беседы со ступенью ниже 9; дальше по ступеням 1 → 11, от
     старых к новым, пока у группы не исчерпан лимит; вызов уходит вместе со своим `tool_result`
     (`web:171920`–`171943`).
  5. Живая лента — с защитой последних 100 (`rme`, `web:171219`–`171225`), история — без неё
     (`ome(…, protectRecent: !1)`, `web:171951`–`171953`).
- **История против живого потока.** Файл читается один раз на открытие (`loadingPromise`
  кэшируется, `web:168862`); повторно — только diff файлов (`reloadSessionDiff`,
  `web:168856`). Строки, которые в файл дописал не процесс вкладки, вкладка не увидит, пока её не
  открыть заново (совпадает с опытом cc_remote, `docs/SPEC.md` §2.4, способ B). Хвост `system`
  в ответе хоста не входит в счёт перенесённых строк (`web:168885`).
- **Смена сессии и перезагрузка.** `system/init` с другим `session_id` очищает ленту
  (`web:171347`–`171386`). `loadFromMessages` (`web:169014`) строит ленту из готового списка
  (телепорт). После перезагрузки окна или перезапуска расширения посреди хода вкладка возобновляет
  ход и пишет «Continuing the step that was interrupted when the window reloaded.» или «No reply
  was recorded for this message before the session was reloaded.» (`ame`,
  `web:172230`–`172241`; `web:168954`–`168968`, `web:170973`–`170980`).
- **Ошибки разбора.** Одна битая запись не роняет ленту: строка «Couldn't display…», счётчик
  пропусков (`pendingRebuildSkips`, `web:168913`), телеметрия.


---

## 10. Что осталось открытым

Остальные пункты прежнего списка закрыты кодом или живыми прогонами и внесены в разделы выше.

1. Запись «с uuid, которого поток не называл» из DrZlo13/hqc#42 (7.6): ни склейка во время финального
   ответа, ни сообщения посреди команды, ни два кадра на старте её не дали — во всех случаях `uuid`
   записи поток называл.
2. Почему старые версии VS Code (2.1.209–2.1.280) писали один `uuid` дважды (7.5): кода тех версий
   нет, механизм — гипотеза.
3. Какое именно сообщение было последним в памяти перед 8 границами сжатия без родителя на диске (7.5):
   механизм найден, но само сообщение на диск не попало.
4. `system/files_persisted`: отправитель в коде не найден, в 33 прогонах кадра не было.
5. Откуда ~27 с ожидания MCP-сервера перед первым ходом (1.2): ни один из найденных таймаутов (5 и
   30 с) не даёт этого числа.
6. `priority:"now"`: ветку прерывания и отмены в `wi()` (3.2) живьём дать не удалось — Bash, вызов
   MCP и субагент в переднем плане переносятся в фон; какой вызов окажется непереносимым, не найдено.
7. `Agent` без поля `run_in_background` ушёл в фон при `claude-vscode` (5.6), а без `claude-vscode` с
   `run_in_background:false` шёл в переднем плане (`p9-now-agent-human`); остальные сочетания не сняты.
8. Как вкладка рисует выжимку из истории, ответы с `narration_hint` и скрытие строк субагентов
   (8, 9), прослежено только по коду: глазами вкладку не смотрели — GUI-автоматизация на ноутбуке
   запрещена.
9. Условие `s` в `zHt` (2.1, `assistant`): когда текст и размышления субагентов идут в поток при
   `initialize.forwardSubagentText`, не прослежено.
10. Каждый живой случай снят одним прогоном (кроме места эха первого промпта — 24 прогона) и одной
    моделью (haiku); период `keep_alive` вживую не измерен — без `CLAUDE_CODE_REMOTE` кадров нет, а
    этот режим меняет транспорт.
