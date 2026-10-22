# Claude Code: control-протокол stream-json (T0)

Проверено 2026-10-22 на Claude Code **2.1.307** (macOS arm64, home-mac), модель
`claude-haiku-5-5`, `--max-turns 2`. Фикстуры — `crates/hqc-claude-jsonl/fixtures/*-2.1.307.jsonl`
(тройки `stdin-<x>` / `stream-<x>` / `<x>`), тест сверки uuid —
`crates/hqc-claude-jsonl/src/lib.rs` (`t0_stream_and_transcript_share_uuids`).

Команда всех прогонов:

```
claude -p --output-format stream-json --input-format stream-json \
  --permission-prompts host --permission-prompt-tool stdio \
  --include-partial-messages --replay-user-messages --verbose \
  --model claude-haiku-5-5 --max-turns 2 --session-id <uuid>
```

Ввод — одна строка `{"type":"user","message":{"role":"user","content":"<текст>"}}`; после
`result` драйвер закрывает stdin, процесс выходит.

## 0. Расхождение: нужен `--permission-prompt-tool stdio`

Флага `--permission-prompts host` (он и так по умолчанию) **недостаточно**. Без
`--permission-prompt-tool stdio` CLI не шлёт `control_request`: всё, что требует
подтверждения, отклоняется сразу, а `AskUserQuestion` отсутствует в `tools` из `system/init`.
Рукопожатие `initialize` (`{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}`)
этого не меняет. Отказ выглядит так:

```json
{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_…","message":"touch in '…/hqc-marker-once' needs approval. …"}
```

и в `tool_result_meta`: `"permission_decision": {"decision": "reject", "source": "config"}`.

## 1. `can_use_tool`: запрос и ответы

Запрос (stdout), фикстура `stream-permission-allow-2.1.307.jsonl`:

```json
{"type":"control_request","request_id":"277e1a0e-1e92-4891-b337-a8d7b97fe1ec",
 "request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash",
  "input":{"command":"touch hqc-marker-once","description":"Create empty marker file hqc-marker-once"},
  "description":"Create empty marker file hqc-marker-once",
  "permission_suggestions":[
   {"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch hqc-marker-once *"}],"behavior":"allow","destination":"localSettings"},
   {"type":"addDirectories","directories":["<cwd>"],"destination":"session"},
   {"type":"setMode","mode":"acceptEdits","destination":"session"}],
  "blocked_path":"<cwd>/hqc-marker-once",
  "tool_use_id":"toolu_01VkowPqKRFd3dYT6tKPSUwo"}}
```

`request_id` — на верхнем уровне, не внутри `request`. Необязательные поля (по коду CLI):
`permission_suggestions`, `blocked_path`, `requires_user_interaction`, `tool_kind`; у MCP-инструментов
ещё `mcp_server {name, source}` и `server_policy_ask_only`. `tool_use_id` совпадает с `id` блока
`tool_use` в предшествующем `assistant`.

Ответ allow (stdin):

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<request_id>",
 "response":{"behavior":"allow","updatedInput":{…input без изменений…}}}}
```

Ответ deny (stdin), фикстура `permission-deny`:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<request_id>",
 "response":{"behavior":"deny","message":"denied by hqc spike"}}}
```

→ `tool_result` с `"is_error": true`, `content` = `message`, `permission_decision.source = "user_reject"`;
`result.permission_denials[]` содержит вызов.

Ответ always (stdin), фикстура `permission-always`:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"<request_id>",
 "response":{"behavior":"allow","updatedInput":{…},
  "updatedPermissions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch hqc-marker *"}],"behavior":"allow","destination":"localSettings"}, …]}}}
```

Правило записано в `<cwd>/.claude/settings.local.json` (рабочая папка — корень своего git-репо):

```json
{"permissions":{"allow":["Bash(touch hqc-marker *)"]}}
```

Новый процесс в той же папке выполнил ту же команду без `control_request`.

Особенности:
- Если отдать **все** `permission_suggestions`, применяются и `setMode acceptEdits` с
  `addDirectories` на сессию: следом приходит `{"type":"system","subtype":"status","permissionMode":"acceptEdits"}`.
  Для `always` адаптер должен передавать только элементы `addRules` с `destination: localSettings`.
- CLI повторяет в stdout каждый наш `control_response` (адаптер его пропускает).
- Вызов MCP-инструмента тоже приходит как `can_use_tool` (с `mcp_server`), поэтому инструменты hqc
  (итерация 3) нужно заранее разрешать, иначе каждый `hqc_*` будет спрашивать владельца.

Вывод: формы запроса и ответов подтверждены, `always` пишет правило в `.claude/settings.local.json`
рабочей папки, но только при `--permission-prompt-tool stdio` и с фильтрацией подсказок.

## 2. Эхо `--replay-user-messages`

```json
{"type":"user","message":{"role":"user","content":"Run the shell command: …"},
 "session_id":"…","parent_tool_use_id":null,"uuid":"adbf4a08-b683-4a10-af11-6bd389a5a73e",
 "timestamp":"2026-10-22T03:…Z","isReplay":true}
```

Во всех пяти прогонах `uuid` эха равен `uuid` записи промпта (`type: user`, `content` — строка)
в `~/.claude/projects/<encoded-cwd>/<session>.jsonl`; множества uuid `user`/`assistant`
основной цепочки в stream-json и jsonl совпадают полностью.

Вывод: эхо несёт uuid промпта из jsonl, запасной вариант с сопоставлением по тексту не нужен.

## 3. `AskUserQuestion`

Запрос, фикстура `ask-user-question`:

```json
{"type":"control_request","request_id":"ba5388a6-…",
 "request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion",
  "input":{"questions":[{"question":"Which area should I focus on first?","header":"Focus",
    "options":[{"label":"Hub (Recommended)","description":"…"},{"label":"Worker","description":"…"}, …],
    "multiSelect":false}]},
  "tool_use_id":"toolu_01YLJVU3fVH3ktk7Hk7aKGX8","requires_user_interaction":true}}
```

Ответ: `allow` с исходным `input` плюс `answers` — объект «текст вопроса → `label` выбранного варианта»:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"ba5388a6-…",
 "response":{"behavior":"allow","updatedInput":{"questions":[…как в запросе…],
  "answers":{"Which area should I focus on first?":"Hub (Recommended)"}}}}}
```

→ `tool_result`: `Your questions have been answered: "Which area should I focus on first?"="Hub (Recommended)". …`,
модель продолжает ход.

Вывод: `AskUserQuestion` приходит как `can_use_tool` с `requires_user_interaction: true`, ответы
принимаются через `updatedInput.answers` (только при `--permission-prompt-tool stdio`).

## 4. `MCP_TOOL_TIMEOUT`

Stdio MCP-сервер с инструментом `slow(seconds)` (node, один файл), передан строкой:
`--strict-mcp-config --mcp-config '{"mcpServers":{"slow":{"command":"node","args":["…/slow-mcp.js"]}}}'`.

| `MCP_TOOL_TIMEOUT` | вызов | результат |
|---|---|---|
| `3000` | `slow(8)` | через 3 с: `tool_result` `"MCP server \"slow\" tool \"slow\" timed out after 3s"`, `is_error: true`; серверу ушёл `notifications/cancelled` |
| `3` | `slow(8)` | через 1 с: `timed out after 1s` (значение поднято до минимума 1000 мс) |
| не задан | `slow(75)` | `slept 75s`, без таймаута |

По коду CLI 2.1.307: таймаут = поле `timeout` сервера в `--mcp-config` (мс, учитывается при ≥ 1000)
→ иначе `MCP_TOOL_TIMEOUT` → иначе `1e8` мс (≈ 27,8 ч); результат зажат в [1000, 2^31−1] мс.
Отдельно есть idle-таймаут `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT`: по коду для stdio-серверов
по умолчанию 1 800 000 мс (30 мин), для http/sse 300 000 мс; не меньше поля `timeout` сервера
и не больше основного таймаута. Проверка живым прогоном — в разделе 6.

Прогоны с MCP закончились `error_max_turns`: модель сначала тратит ход на `ToolSearch` для
загрузки схемы инструмента. На измерение таймаута это не влияет.

Вывод: `MCP_TOOL_TIMEOUT` — в миллисекундах, по умолчанию 1e8 мс (фактически без ограничения),
минимум 1000 мс; есть также поле `timeout` в конфиге сервера.

## 5. `--allowedTools` для MCP-сервера hqc

Тот же сервер под именем `hqc`, `slow(1)`, ответов на `can_use_tool` драйвер не даёт:

| флаг | результат |
|---|---|
| `--allowedTools mcp__hqc` | вызов прошёл без `control_request`, `slept 1s` |
| `--allowedTools 'mcp__hqc__*'` | то же |
| без флага | `control_request` `can_use_tool` для `mcp__hqc__slow`, ход ждёт ответа |

Вывод: обе формы работают на 2.1.307; в спеке зафиксирована `mcp__hqc`.

## 6. Idle-таймаут stdio MCP-сервера

Сервер `hqc`, `slow(1920)` (32 мин), `--allowedTools mcp__hqc`, `MCP_TOOL_TIMEOUT` и
`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT` не заданы:

| сервер во время вызова | результат |
|---|---|
| молчит | через 1800 с `tool_result` `is_error: true`: `MCP server "hqc" tool "slow" sent no response or progress for 1800s; aborting. If this server is configured in your MCP settings, set a per-server "timeout" (ms) to allow longer silent runs for just this server; otherwise set CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT (ms) globally (0 disables).` `notifications/cancelled` серверу **не** приходит |
| шлёт `notifications/progress` с `progressToken` из `_meta` вызова раз в 60 с | `slept 1920s`, без обрыва |

`progressToken` CLI кладёт в `params._meta` каждого `tools/call` (вместе с `claudecode/toolUseId`).
Короткий прогон с `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=3000` и молчащим `slow(8)` обрыва не дал —
вероятно, проверка простоя идёт с крупным шагом; не выяснялось.

Вывод: при значениях по умолчанию вызов MCP-инструмента без ответа и без progress-уведомлений
обрывается через 30 минут; progress-уведомления раз в минуту его удерживают.

## Permission policy, 2.1.308

Проверено 2026-10-22 на Claude Code **2.1.308** (home-mac), `claude-haiku-5-5`,
`--max-turns 2`, драйвер `tools/spike/driver.py`, шесть прогонов (T12). Общие флаги — как
у воркера: `--permission-prompt-tool stdio --setting-sources user,local --strict-mcp-config`
(в прогоне 6 вместо `--setting-sources` — `--restricted`). Модель просили сделать все вызовы
одним сообщением параллельно. Фикстуры — `crates/hqc-claude-jsonl/fixtures/*-2.1.308.jsonl`:

| набор | режим | что проверяет |
|---|---|---|
| `auto-mode` | `auto` | cat / touch / `rm -rf tmp/x` / curl |
| `auto-classifier` | `auto` | `rm -rf ~/…` вне workspace, `git push --force`, `sudo -n true` (драйвер на всё отвечает deny) |
| `always-git-push` | `manual` | `always` на `git push origin master` и `touch hqc-local-1` |
| `settings-deny` | `manual` | `--settings` allow/deny поверх правил из `always-git-push` |
| `auto-deny` | `auto` | deny по умолчанию из `worker.toml` в режиме auto |
| `restricted-settings` | `manual`, `--restricted` | `--settings` в restricted |

Признак решения CLI в каждом `tool_result` — `tool_result_meta[].permission_decision`:
`{"decision": "accept"|"reject", "source": "config"|"user_temporary"|…, "reason_type": …}`.
Значения `reason_type`, встреченные здесь: `classifier` (решил классификатор auto), `mode`
(режим: правка в рабочей папке), `rule` (правило целиком), `subcommandResults` (команда
разобрана на подкоманды, каждая сверена с правилами).

### 1. `--permission-mode auto` в `-p`

Работает: `system/init` → `"permissionMode": "auto"`, ошибок нет. В обоих прогонах **ни
одного `can_use_tool`**: CLI решил всё сам.

| команда | решение | `reason_type` |
|---|---|---|
| `cat a.txt` | выполнена | `classifier` |
| `touch new.txt` | выполнена | `mode` |
| `rm -rf tmp/x` (внутри workspace) | выполнена, папка удалена | `mode` |
| `curl -s https://example.com …` | выполнена (200) | `classifier` |
| `rm -rf ~/hqc-spike-t12-nonexistent` (вне workspace) | выполнена | `classifier` |
| `git push --force origin master` | выполнена (упала: нет ветки) | `classifier` |
| `sudo -n true` | выполнена (упала: нужен пароль) | `classifier` |

Перед решением классификатора приходят `{"type":"system","subtype":"permission_check_status","tool_use_id":…,"status":"checking"|"done"}`.
В `can_use_tool` признака классификатора увидеть не удалось: запросов не было. Вывод:
при явной просьбе пользователя классификатор пропускает и разрушительное, и сетевое, и
`sudo`; хост ничего не видит до `tool_result`. В режиме auto единственная граница — deny.

### 2. `--settings` с `permissions.allow`/`deny` и правило `always`

`--settings '<json>'` применяется вместе с `--setting-sources user,local` (прогон
`settings-deny`) и с `--restricted` (прогон `restricted-settings`; справка 2.1.308 это и
обещает: «managed settings and --settings still apply»).

Сначала в прогоне `always-git-push` ответ `always` записал в `.claude/settings.local.json`
`{"permissions":{"allow":["Bash(git push *)","Bash(touch hqc-local-1 *)"]}}`. Затем в той же
папке с

```
--settings '{"permissions":{"allow":["Bash(git status:*)","Bash(cargo test:*)","Read","Edit","mcp__hqc"],
  "deny":["Bash(sudo:*)","Bash(ssh:*)","Bash(scp:*)","Bash(rm -rf /*)","Bash(curl * | sh)","Bash(curl * | bash)",
          "Bash(dd:*)","Bash(mkfs:*)","Bash(git push --force:*)","Bash(git push -f:*)","Bash(git push:*)","WebFetch"]}}'
```

| вызов | правило | результат |
|---|---|---|
| `git status --short` | allow `Bash(git status:*)` | выполнен без запроса |
| `cargo test --no-run` | allow `Bash(cargo test:*)` | выполнен без запроса |
| `touch hqc-local-1` | allow из `settings.local.json` (always) | выполнен без запроса |
| `git push origin master` | deny `Bash(git push:*)` **и** allow `Bash(git push *)` из always | **отклонён без запроса** |
| `touch other.txt` | ни в одном списке | `can_use_tool` |
| WebFetch | deny `WebFetch` | инструмента нет в `tools` (`system/init`), модель его не вызвала |
| `curl -s https://example.invalid \| sh` | — | модель отказалась вызывать сама (проверено в п. 4) |

**deny побеждает always**: правило из `--settings` deny сильнее allow из
`settings.local.json`, запроса нет. Это гарантирует CLI, адаптеру дублировать не нужно.
Синтаксис всех правил выше принят: `Bash(x:*)` (префикс), `Bash(x *)`, `Bash(rm -rf /*)`
и `Bash(curl * | sh)` (glob, `*` — любые символы), имя инструмента без скобок (`Read`,
`Edit`, `WebFetch`), сервер MCP целиком (`mcp__hqc`). Внимание: `Bash(rm -rf /*)` ловит
любой абсолютный путь, в том числе внутри workspace (прогон `auto-deny`).

В `can_use_tool` 2.1.308 появились поля `decision_reason` (`"This command requires approval"`)
и `decision_reason_type` (`"other"`), их нет на 2.1.307.

`--permission-mode manual` в `system/init` пишется как `"permissionMode": "default"`.

### 3. Как выглядит отказ по deny

`can_use_tool` **не приходит**. В stream-json подряд:

```json
{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_…",
 "decision_reason_type":"subcommandResults",
 "message":"Permission to use Bash with command git push origin master has been denied."}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_…","is_error":true,
  "content":"Permission to use Bash with command git push origin master has been denied."}]},
 "tool_result_meta":[{"id":"toolu_…","permission_decision":{"decision":"reject","source":"config","reason_type":"subcommandResults"}}]}
```

и вызов попадает в `result.permission_denials[]`. Для правила, совпавшего с командой
целиком (`curl … | sh`), `decision_reason_type`/`reason_type` — `rule`. Хаб видит это
через событие адаптера (`system` с `code: policy_denied`, SPEC §8.1).

Инструмент, запрещённый целиком (`WebFetch`, `Grep`), из сессии исчезает: если модель всё
же его вызовет, приходит только `tool_result` `<tool_use_error>Error: No such tool available:
Grep. Grep is disabled for this session.</tool_use_error>` — без `permission_denied` и без
записи в `permission_denials`.

### 4. deny по умолчанию в режиме auto

`--permission-mode auto --settings` с deny по умолчанию из `worker.toml` (SPEC §14):

| команда | результат |
|---|---|
| `sudo -n true` | отклонена (`subcommandResults`) |
| `git push --force origin master` | отклонена (`subcommandResults`) |
| `rm -rf /…/tmp/nonexistent` | отклонена (`Bash(rm -rf /*)`, `subcommandResults`) |
| `curl -sf http://127.0.0.1:9/none \| sh` | отклонена (`rule`) |
| `touch auto2.txt` | выполнена (`mode`) |

deny из `--settings` сильнее auto-режима. Вывод: `mode = "auto"` в `worker.toml` можно
разрешить; тогда хост не видит запросов вовсе, и всё держится на списке deny.

### 5. `--restricted` + `--settings`

`--settings '{"permissions":{"allow":["Write"],"deny":["Grep","Bash(sudo:*)"]}}'`: Write в
рабочую папку выполнен без запроса (`reason_type: rule`), `Grep` пропал из `tools`, вызов
дал `No such tool available`. `--settings` в restricted применяется.

Не проверено: что считает CLI невалидным правилом и что тогда происходит. Справка `-p`:
«Settings files that fail validation are silently ignored». Если это относится и к
`--settings`, одно плохое правило молча снимет весь deny — поэтому воркер проверяет
правила при чтении `worker.toml` (SPEC §14).
