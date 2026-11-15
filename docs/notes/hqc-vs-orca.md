# hqc против Orca: что там есть, а у нас нет

Orca (https://orca.dev, https://github.com/stablyai/orca, MIT — `README.md:323`) от Stably AI —
настольное приложение на Electron для macOS, Windows и Linux, которое умеет раздавать веб-клиент и
работает в паре с приложениями для iOS и Android. Каждая задача идёт в своём рабочем месте
(worktree или папка) со своими агентами, вкладками, терминалом и браузером. Агент запускается
программой в терминале или в виде «Chat UI»; там Claude работает через
`@anthropic-ai/claude-agent-sdk` 0.3.319 (`package.json:235`). Здесь — все функции Orca, которые
видит пользователь, и для каждой — есть ли она в hqc.

Устроены продукты по-разному. В Orca на каждой машине работает свой экземпляр — окно или
`orca serve` без окна, — и клиенты подключаются к каждой машине отдельно: по ссылке доступа через
Tailscale или локальную сеть либо через облачный посредник Orca Relay. В hqc один хаб, и воркеры
сами подключаются к нему. Поэтому функции Orca для доступа к отдельным машинам (SSH-цели, пары
телефона с каждым хостом) в hqc не нужны. SPEC hqc говорит «it is not an IDE: no file editor, diffs
only as artifacts» (`docs/SPEC.md:39`) — под это попадают git-панель, терминал, браузер и редактор
Orca.

## Что сравнивалось

- Orca — коммит `2cd3f168` (2026-11-15 07:45:48 −0800). Пути Orca даны от корня его репозитория.
- Чем читалась Orca: пользовательская документация `docs/site/content/docs/` (из 60 файлов открыты
  37, из них 36 целиком; 23 не открывались — список в «Не проверено»), `README.md`, заметки к выпуску
  v1.4.259 (первые 340 из 636 строк), к mobile-android-v0.0.64 и v0.0.65, главная https://orca.dev.
  Код — выборочно: компоненты чата `src/renderer/src/components/native-chat/`, поисковые записи
  настроек (295), 100 сочетаний клавиш в `src/shared/keybindings/definitions-core-{1,2,3,4}.ts`,
  строки и маршруты телефонного приложения `mobile/`, распознавание речи `src/main/speech/`,
  веб-клиент `src/renderer/src/web/`, облачные части `cloud/`. Поведение функций описано по
  документации и строкам интерфейса; Orca не запускалась.
- hqc — `origin/master` `5e11381` (2026-11-15 05:35 UTC). Пути hqc даны от корня репозитория. После
  него в master пришли провайдеры моделей (`1bc972a7`, SPEC 0.144: Anthropic-совместимый адрес на
  сессию, `[providers.<name>]` в `worker.toml`, `hqc_spawn(provider)`) — в таблицы это не вошло,
  касается строк «34 агента» и «Несколько аккаунтов».
- У каждого «нет» указаны выражения, по которым искалось в `docs/SPEC.md`, `client/src` и
  `crates/*/src`; «только в hqc» тоже проверялось поиском по Orca. «Есть» включает случаи, когда hqc
  делает то же иначе, — как именно, сказано в ячейке.
- Пометки: [мульти] — смысл есть только при нескольких поставщиках агентов; [десктоп] — только для
  настольного приложения; [моб] — только для нативного телефонного приложения.

## Коротко

| В hqc | Функций Orca |
| --- | ---: |
| нет | 68 |
| частично | 51 |
| есть | 32 |
| не нужно при хабе | 4 |
| не проверено | 2 |
| всего | 157 |

Из 68 «нет» 9 помечены [десктоп], 5 — [моб], [мульти] — 0.

Чего в hqc нет совсем и что ценнее всего, когда много сессий Claude Code на нескольких машинах, а
смотришь с браузера и телефона:

1. **Голосовой ввод** — на компьютере и на телефоне (ниже, «Как Orca делает голосовой ввод»).
2. **Продолжение после сброса лимита.** Orca сама пишет агенту «Continue where you left off», когда
   лимит сбросился, даёт «Continue Now» и продолжение под другим аккаунтом. В hqc сессия, упёршаяся
   в лимит, стоит, хотя время сброса hqc знает (`worker.limits`, SPEC:1634-1636).
3. **Лента событий по всем сессиям**: кто закончил, кто ждёт, превью последнего ответа; на телефоне —
   вкладка Feed. В hqc «Requests» — только открытые запросы, превью в строке сессии нет.
4. **Заполнение контекста** (кольцо «N% used») и **таймер кэша промпта**.
5. **Список задач агента** (TodoWrite), закреплённый над лентой.
6. **Side chat `/btw`** — спросить работающую сессию, не сбив её ход.
7. **Сводка хода**: изменённые файлы с числом строк, открытые агентом PR, строки о CI.
8. **Метка «New»** над пришедшим без тебя и кнопка **«Jump to latest»**.
9. **↑ в пустом поле** поднимает прошлый промпт; длинная вставка уходит вложением.
10. **Quick Commands** — сохранённые промпты и команды одним нажатием, общие с телефоном.

Из сделанного наполовину больше всего просятся доделать: очередь ввода (правка и удаление ждущих,
выбор «влить в ход или ждать»); переключатель режима плана; Stop у фоновых задач и ленту субагента
(готовятся отдельные PR); новую сессию на каждый
запуск расписания (описана в SPEC:4665-4666, в `crates/hqc-hub/src/schedules.rs` её нет); категории
уведомлений вместо одного переключателя.

Только в hqc, у Orca этого нет:

- Один хаб хранит историю, поиск и очередь ввода всех машин, даже когда машина выключена
  (SPEC:43-57, 4799-4800). Поиск Orca недоступные хосты пропускает
  (`sessionSearch.panel.hostSkipReason.unreachable` в `src/renderer/src/i18n/locales/en.json`).
- Живое зеркало и перехват сессий VS Code и `claude` в терминале (SPEC §9.2–9.3). В Orca
  `mirror … vs code session|take over … session` — 0 файлов; есть только история прошлых сессий с
  Resume.
- Перенос сессии в другой проект или папку: `hqc_move` и перетаскивание. В Orca `Move to
  project|move session to` — 0.
- Правила по папкам и потолок прав по цепочке сессий (SPEC:115-134, §10). В Orca `folder
  rules|deny rules|path rules` — 7 файлов, все про пути и права Windows; Orca идёт в обратную
  сторону — координатор может сам одобрять запросы рабочих
  (`docs/site/content/docs/cli/orchestration.mdx:89-99`).
- Полоса скорости токенов на панели проектов (SPEC:1764-1772). В Orca `tokens per
  second|tok/s|output_per_s` — только снимок экрана OpenCode среди материалов к баг-репортам.
- Поиск выделенного в чате текста в вебе (SPEC:1963-1965). В Orca такой пункт есть только в меню её
  встроенного браузера.
- Web Push в обычный браузер без нативного приложения. Получает ли push веб-клиент Orca — не
  проверено.

## Как Orca делает голосовой ввод

- **Компьютер.** Сочетание `Mod+E` (`src/shared/keybindings/definitions-core-1.ts:164-169`), режим
  Toggle или Hold (`docs/site/content/docs/settings.mdx:115-129`). Текст встаёт в то, что в фокусе:
  поле агента, терминал или редактор. Распознаёт своя машина: `transcribe-cpp` 0.4.1
  (`package.json:400-406`) в отдельном процессе (`src/main/speech/stt-worker-process.ts`), тишину
  отсекает детектор Silero, его модель скачивается в фоне (`src/main/speech/speech-vad-model.ts:4-13`).
- **Модели.** Каталог `src/main/speech/model-catalog.ts:19-241` — 17 моделей: 13 локальных и 4
  облачных (GPT Transcribe, OpenAI-compatible, GPT-4o mini Transcribe, GPT-4o Transcribe; нужен ключ
  OpenAI). Русский по спискам языков (`src/shared/speech/models/dictation-language.ts`) — у 4
  локальных (Parakeet TDT v3 — рекомендуемая, Whisper Tiny, Whisper Large v3 Turbo, Nemotron 3.5) и у
  всех 4 облачных. Замер самой Orca для русского — доля ошибок в словах: Parakeet TDT v3 3,4 %,
  Whisper Large v3 Turbo 3,4 %, Nemotron 3.5 14,6 %, Whisper Tiny 38,2 %
  (`src/main/speech/speech-model-error-rates.ts:8-62`); это 5 записей FLEURS на M4 Max, авторы в
  комментарии сами называют замер грубым.
- **Настройки** (`src/renderer/src/components/settings/voice/voice-pane-search.ts:16-269`): словарь
  терминов, личный и проектный; удаление слов-паразитов; «AI cleanup» — чистка расшифровки своим
  агентом, по умолчанию выключена и спрашивает согласия; остановка по тишине, держать модель в
  памяти, GPU, выбор микрофона, язык.
- **Телефон.** Приложение пишет звук (`@orca/expo-two-way-audio`, `mobile/package.json:28`) и шлёт
  его кусками на свой компьютер (`speech.dictation.start`, `speech.dictation.chunk`); распознаёт
  компьютер (`mobile/src/dictation/mobile-dictation-operations.ts:52-66`). Модель на компьютер можно
  скачать с телефона. Без окна (`orca serve` на сервере) диктовки нет
  (`docs/site/content/docs/remote-servers.mdx:208`).
- **hqc.** В SPEC и `client/src` выражения `voice|dictat|microphone|getUserMedia|speech` не
  встречаются; единственное упоминание — воркер выключает голос самого Claude Code
  (`crates/hqc-worker/src/title.rs:50`).

## Поле ввода

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Голосовой ввод | диктовка по сочетанию (Mod+E) в поле агента, терминал или редактор; модель на своей машине или облако | `docs/site/content/docs/settings.mdx:115-129`; `src/main/speech/model-catalog.ts:19-241`; `src/shared/keybindings/definitions-core-1.ts:164-169` | **нет**: `voice\|dictat\|microphone\|getUserMedia\|speech` — в SPEC и client 0; единственное — воркер выключает микрофон самого CLI (`crates/hqc-worker/src/title.rs:50`) |
| Queue или Steer во время хода | настройка «Queue follow-ups»; Cmd/Ctrl+Enter делает наоборот для одного сообщения, с пустым полем вливает первую ждущую карточку в ход | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:57-69`; `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerActions.tsx:127-139` | **частично**: ввод сам вливается в ход или ждёт (SPEC §17.2, 4748-4785), пометка «joined the turn» (`client/src/ui/cards.tsx:595`); выбора режима нет; «interrupt and send» есть только в SPEC:4779 |
| Ждущие сообщения карточками | правка, удаление, перетаскивание порядка, «Send this message now», «Clear all» | `src/renderer/src/components/native-chat/queue/NativeChatQueuedMessageCard.tsx:108, 304`; `src/renderer/src/components/native-chat/queue/NativeChatQueuedMessageRows.tsx:77`; `src/renderer/src/components/native-chat/queue/NativeChatQueuedMessageList.tsx:233`; `src/renderer/src/components/native-chat/queue/editing/NativeChatQueuedMessageComposer.tsx:71` | **частично**: «· awaiting delivery» у ждущего (`client/src/ui/cards.tsx:598`), «N queued» у строки (`client/src/ui/sessions.ts:118`); Retry и Discard — только у сообщения, которое клиент не смог отправить (`client/src/ui/cards.tsx:654-663`); ждущее на хабе не правится и не удаляется |
| Офлайн-очередь на телефоне [моб] | сообщение без связи ждёт: «Sends when the desktop is back» | `mobile/src/i18n/locales/en.json:3064-3070` | **частично**: ввод для выключенного воркера ждёт на хабе (SPEC:4799-4800); клиент без сети ничего не копит |
| Черновик переживает переподключение | текст и вложения остаются при обрыве и переподключении | `docs/site/content/docs/agents/native-chat.mdx:47`; `src/renderer/src/components/native-chat/drafts/native-chat-composer-draft-indexeddb.ts` | **есть**: черновик в браузере, «Draft» у строки (SPEC:1720-1722); вложения в черновике — не проверено |
| Вложения | вставка, перетаскивание на панель чата, картинки миниатюрами | `src/renderer/src/components/native-chat/attachments/intake/NativeChatPaneFileDropSurface.tsx:189-193` | **есть**: вставка и перетаскивание (`client/src/ui/Chat.tsx:1197`), HEIC→JPEG, до 32 файлов (SPEC:1606) |
| Длинная вставка — вложением | большой текст становится чипом «Pasted text»; предел сообщения 256 КБ | `src/renderer/src/components/native-chat/media/text/NativeChatTextAttachmentPreview.tsx:49`; `src/renderer/src/components/native-chat/paste/use-native-chat-pasted-text-file.ts:33` | **нет**: только в бэклоге (`docs/BACKLOG.md:35-46`) |
| Файл через `@` | список файлов рабочей папки по `@` | `src/renderer/src/components/native-chat/composer/pickers/NativeChatAutocompleteMenus.tsx:382-385`; `docs/site/content/docs/mobile.mdx:67` | **нет**: `mention\|@file\|file picker\|autocomplete` — по делу 0 (только `autocomplete="off"` в формах) |
| Слэш-команды и навыки | меню `/` с описаниями, отдельно Commands и Skills, источник навыка Project / Personal / Built-in / Plugin | `src/renderer/src/components/native-chat/composer/pickers/NativeChatAutocompleteMenus.tsx:149-150, 350-353`; `src/renderer/src/components/native-chat/composer/pickers/native-chat-slash-command-descriptions.ts:11-66` | **частично**: `/` дополняется списком команд CLI, только имена (SPEC:2066-2071; `client/src/proto.ts:1098-1100`) |
| Прошлый промпт стрелкой | ↑ в пустом поле поднимает свои прошлые сообщения из ленты | `src/renderer/src/components/native-chat/drafts/native-chat-sent-prompt-history.ts:1`; `src/renderer/src/components/native-chat/drafts/use-native-chat-composer-recall.ts` | **нет**: `ArrowUp` — только в списках (`client/src/ui/SessionList.tsx:333`, `client/src/ui/ProjectRail.tsx:178`) |
| Выделенное — в поле ввода | «Add to chat» у выделения в ленте | `src/renderer/src/components/native-chat/selection-quotes/AddToChatMenu.tsx:46` | **есть**: «Quote» вставляет выделение строками `> ` (SPEC:1963-1965) |
| Side chat `/side`, `/btw` | вопрос в копии рядом с чатом, до агента не доходит | `src/renderer/src/components/native-chat/composer/pickers/use-native-chat-side-chat-command.ts:26, 48-50` | **нет**: `side chat\|/btw\|side question` — 0 |
| Цель `/goal` | баннер цели: «Pursuing goal», пауза, продолжить, сбросить | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerGoalChip.tsx:11-12`; `src/renderer/src/components/native-chat/session/goals/NativeChatThreadGoalBanner.tsx:86-103` | **нет**: `/goal\|goal mode\|thread goal` — 0. В Orca `/goal` описан среди команд Codex (`src/renderer/src/components/native-chat/composer/pickers/native-chat-codex-slash-command-descriptions.ts:131` в Orca); работает ли с Claude — не проверено |
| Режим плана | чип «Plan» и «Exit plan mode», карточка с файлом плана | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerPlanChip.tsx:50-51`; `src/renderer/src/components/native-chat/prompts/NativeChatApprovalCard.tsx:179` | **частично**: переключателя нет; `ExitPlanMode` приходит запросом разрешения (SPEC:3205); воркер знает режим `plan` (`crates/hqc-worker/src/capabilities.rs:179-187`) |
| Модель, effort, Fast | выбор модели с поиском и описаниями (список берётся из установленного CLI), уровень рассуждений, Fast mode | `src/renderer/src/components/native-chat/session/options/controls/NativeChatModelCombobox.tsx:49`; `src/renderer/src/components/native-chat/session/options/controls/native-chat-session-option-labels-text.ts:11-27`; `docs/site/content/docs/agents/native-chat.mdx:49` | **частично**: модель меняется (SPEC:1896-1897); `effort\|thinking budget\|reasoning_effort\|ultrathink` — 0, `fast mode\|fast_mode` — 0 |
| Ветка git в поле ввода | переключить ветку рабочей папки, пока агент не работает | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerBranchPicker.tsx:53-69` | **нет**: ветка есть только у записи worktree или clone (SPEC:245-261); переключения из чата нет |
| Остановить ход | кнопка «Stop the agent» | `src/renderer/src/components/native-chat/composer/controls/NativeChatComposerActions.tsx:164-165` | **есть**: «Stop the agent» (`client/src/ui/Chat.tsx:537`) |

## Лента

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Поиск в открытом чате | Mod+F, строка поиска по ленте | `src/shared/keybindings/definitions-core-4.ts:35`; `src/renderer/src/components/native-chat/viewport/find/NativeChatFindBar.tsx` | **частично**: строки поиска в чате нет (`find in chat\|cmd\+f\|Mod\+F` — 0); страница поиска фильтрует по сессии (`session=`, SPEC:1663-1664) |
| Рейка «Your messages» | столбик своих сообщений сбоку ленты, переход к любому | `src/renderer/src/components/native-chat/viewport/rail/NativeChatMessageRail.tsx:115` | **нет**: `minimap\|outline\|jump to` — 0 |
| Метка новых сообщений | при возврате в чат линия «New» над пришедшим без тебя | `src/renderer/src/components/native-chat/viewport/new-rows/NativeChatNewRowsMarker.tsx:77`; `src/renderer/src/components/settings/chat/chat-behavior-search.ts:83` | **нет**: `new messages\|unread divider\|while you were away` — 0; есть счётчик непрочитанного у строки (`client/src/ui/SessionList.tsx:852`) |
| «Jump to latest» | кнопка вниз, когда лента прокручена вверх | `src/renderer/src/components/native-chat/viewport/scroll/NativeChatJumpToLatest.tsx:19` | **нет**: `to-bottom\|toBottom\|scrollToBottom\|jump-latest` — 0; лента держит низ сама (SPEC:1970-1971) |
| Старая история по частям | подгрузка ранних сообщений при прокрутке вверх | `src/renderer/src/components/native-chat/viewport/history/NativeChatOlderHistoryRow.tsx:17` | **есть**: старые блоки по 60 (SPEC:1987-1989) |
| Время и копирование сообщения | время у сообщения, «Copy message», у блока кода — копировать и перенос строк | `src/renderer/src/components/native-chat/messages/rows/NativeChatMessageTimestamp.tsx`; `src/renderer/src/components/native-chat/messages/actions/NativeChatCopyButton.tsx:47`; `src/renderer/src/components/native-chat/markdown/code/NativeChatCodeBlock.tsx:37, 91` | **есть**: время в первой строке, копирование (SPEC:1914-1919; `client/src/ui/cards.tsx:631`), копирование блока кода (`client/src/ui/Markdown.tsx:344-363`); переключателя переноса строк — не проверено |
| Размышление | строка «Thought for N s», раскрывается | `src/renderer/src/components/native-chat/messages/reasoning/NativeChatReasoningRow.tsx:22-28` | **есть**: строка Thinking (`client/src/ui/feed.ts:189`; SPEC:1922-1923); живого размышления по ходу нет (готовится отдельный PR) |
| Группы вызовов | «Read 3 files · Ran 2 commands», раскрытие | `src/renderer/src/components/native-chat/tools/runs/native-chat-tool-run-label.ts:17-143` | **есть**: «N tool calls · время» и список инструментов (SPEC:1919-1922) |
| Дифф правки | карточка диффа, «Copy diff» | `src/renderer/src/components/native-chat/tools/diffs/NativeChatDiffCard.tsx:188-202` | **есть**: карточка и раскрытая строка Edit, MultiEdit, Write — диффом (SPEC:3871-3876) |
| Сводка хода: изменённые файлы | под ответом — файлы хода с числом строк | `src/renderer/src/components/settings/chat/chat-turn-activity-search.ts:17`; `src/renderer/src/components/native-chat/tools/diffs/NativeChatTurnDiffRollup.tsx:86-110` | **нет**: сводного диффа хода нет (против «diffs only as artifacts», SPEC:39) |
| Открытые агентом PR и строки CI | карточка «Opened N pull requests», строка «CI failing on / CI green on» | `src/renderer/src/components/settings/chat/chat-turn-activity-search.ts:29-47`; `src/renderer/src/components/native-chat/tools/pull-requests/NativeChatOpenedPullRequestCard.tsx:232`; `src/renderer/src/components/native-chat/messages/notices/NativeChatPullRequestEventLine.tsx:68-69` | **нет**: `pull request\|git push\|commit message` — только правила deny (SPEC:4394) |
| Список задач агента (TodoWrite) | «3 of 7 tasks completed», закреплённый список | `src/renderer/src/components/native-chat/tools/tasks/NativeChatTaskList.tsx:86`; `src/renderer/src/components/native-chat/tools/tasks/NativeChatDockedTaskList.tsx` | **нет**: `TodoWrite\|todo\|task list` в client — 0 |
| Фоновые задачи со «Stop» | строка фоновых команд и агентов, кнопка остановки у каждой и «Stop background tasks» | `src/renderer/src/components/native-chat/tools/tasks/NativeChatBackgroundTasksStatus.tsx:136`; `src/shared/background-task-copy.ts:6-14` | **частично**: фоновые команды и субагенты списком над вводом (SPEC:2019-2030), «Run in background» (SPEC:4728-4730); остановки нет (готовится отдельный PR) |
| Субагент своей лентой | задание, шаги, ответ субагента в отдельном виде, «Back to main agent»; у каждого модель и текущий инструмент | `src/renderer/src/components/native-chat/subagents/NativeChatSubagentStatusBar.tsx:40`; `src/renderer/src/components/native-chat/subagents/NativeChatSubagentPrompt.tsx:18`; GitHub v1.4.259, PR #41766, #42066 | **частично**: карточки запуска и конца со статусом (SPEC:1923-1961); ленты субагента нет (готовится отдельный PR) |
| Встроенные визуализации | графики, диаграммы, макеты внутри ответа; «Open in new tab» | `src/renderer/src/components/settings/chat/chat-inline-visuals-search.ts:7`; `src/renderer/src/components/native-chat/media/visuals/NativeChatInlineVisual.tsx:31, 117` | **частично**: HTML через `hqc_attach` в iframe-песочнице (SPEC:4212; `client/src/ui/attachments.tsx:346`) |
| Mermaid | диаграммы в Markdown, в том числе на телефоне | `docs/site/content/docs/editing/viewers.mdx:17-19`; `docs/site/content/docs/mobile.mdx:21` | **нет**: `mermaid\|diagram` — 0 |
| Превью ссылки | карточка веб-страницы или GitHub issue и PR при наведении | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:95`; `src/renderer/src/components/native-chat/links/previews/NativeChatGitHubLinkPeek.tsx:57-74` | **нет**: `link preview\|hover card\|og:\|unfurl` — по делу 0 |
| Ссылки на файлы открывают файл | путь в ответе открывает файл во вкладке редактора | `src/renderer/src/components/native-chat/links/actions/native-chat-file-link-toasts.ts:7-41` | **нет**: не IDE (SPEC:39) |
| Сообщения от других агентов | «From <агент>», «Go to agent» | `src/renderer/src/components/native-chat/messages/agent-senders/NativeChatAgentMessageSenders.tsx:43, 112` | **есть**: фиолетовые блоки «↙ Message from X» со ссылкой на сессию (SPEC:1991-2007) |
| Копия от ответа | «Fork from this response», `/fork` | `src/renderer/src/components/native-chat/messages/actions/NativeChatForkAction.tsx:14, 46`; `src/renderer/src/components/native-chat/composer/pickers/use-native-chat-resume-command.ts:57` | **есть**: «Fork from here» (`client/src/ui/cards.tsx:582-583`; SPEC 0.80) |
| Rewind to here | разговор в той же сессии откатывается к сообщению, оно возвращается в поле; файлы на диске не трогаются | `src/renderer/src/components/native-chat/messages/actions/use-native-chat-rewind.ts:62-67` | **частично**: только копия до сообщения (Fork); в той же сессии — `rewind\|revert to\|discard this message` в client 0 |
| Вид чата | шрифт, размер текста и кода, ширина, контраст, «Match terminal interface» | `src/renderer/src/components/settings/appearance/chat/chat-appearance-search.ts:23-93` | **частично**: виды Compact и Spacious (SPEC:1705-1715); размера шрифта, ширины и контраста нет |

## Сессии

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Чат или голый CLI на выбор | агент открывается чатом или терминалом, Cmd+Shift+J переключает; «Continue with Terminal Agent», «Continue in Chat UI» | `docs/site/content/docs/agents/native-chat.mdx:12-43`; `src/shared/keybindings/definitions-core-4.ts:43`; `src/renderer/src/components/native-chat/pane/actions/NativeChatResumeInCliButton.tsx:80` | **частично**: только чат; терминала нет (SPEC:39); сессию из VS Code или `claude` в терминале hqc показывает зеркалом и перехватывает (SPEC §9.2–9.3, 1909-1914) |
| «Hand Off to Another Agent» [мульти] | передать разговор другому агенту в той же рабочей папке | `src/renderer/src/components/native-chat/pane/actions/NativeChatPaneHeader.tsx:46`; `src/renderer/src/components/native-chat/pane/actions/use-native-chat-hand-off.tsx` | **частично**: агент сам заводит сессию с промптом `hqc_spawn` (SPEC:2270-2293); из меню чата — нет |
| Названия чатов | агент называет чат по первому сообщению, можно выбрать другого агента для названий | `src/renderer/src/components/settings/chat/chat-naming-search.ts:7` | **есть**: название пишет Claude Code (`ai-title`, SPEC:342-356), Rename, `hqc_rename` (SPEC:2439) |
| Перезапуск агента | чип Restart после выхода агента | `docs/site/content/docs/model/agents-sessions.mdx:67-69` | **есть**: Restart session (SPEC:1817, 4794-4798) |
| Остановка простаивающих агентов | «Stop idle agents after», «Agent sleep»: процесс снимается, следующее сообщение поднимает его с той же историей | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:44`; `docs/site/content/docs/agents/hibernation.mdx:8-40` | **частично**: `idle_timeout` воркера (SPEC:4787-4793) — в `worker.toml`, по умолчанию выключен, в интерфейсе нет |
| Продолжить после перезапуска | работавшие чаты сами продолжаются после выхода или обновления Orca; «Continue» после обрыва — агент сначала проверяет свой последний шаг | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:14`; `src/renderer/src/components/native-chat/usage/NativeChatInterruptedContinue.tsx:247` | **частично**: ввод не теряется, прерванный ход помечается `turn_lost` и сам не продолжается (SPEC:4802-4806); Retry у сессии в ошибке (SPEC:1914) |
| Агенты живут без открытого приложения | демон держит процессы агентов после закрытия окна | `docs/site/content/docs/model/session-restore.mdx:14-42` | **есть**: агенты на воркерах, браузер не нужен (SPEC:43-57) |
| История сессий всех CLI на машине | список прошлых сессий с фильтрами, Resume, «Copy resume command» | `docs/site/content/docs/agents/session-history.mdx:8-52` | **есть**: для Claude Code — импорт (SPEC §9.1, 3524-3620), «Import from Claude Code» в проекте (SPEC:1753); других агентов hqc не ведёт |
| Полнотекстовый поиск по сессиям | поиск по тексту всех прошлых сессий всех машин, «Found on <машина>»; агенты ищут тоже | `src/renderer/src/i18n/locales/en.json` (`featureTips.sessionSearch.*`, `sessionSearch.panel.*`) | **есть**: страница поиска (SPEC §15, 1657-1680), `hqc_search_history` (SPEC:2322-2328); индекс один на хабе |
| Agent Dashboard | канбан Needs You / Working / Done / Idle по всем worktree с поиском и фильтрами, в окне или отдельным окном | `docs/site/content/docs/model/agents-sessions.mdx:23-58` | **частично**: группы Needs you / Running / Idle в проекте (SPEC:1833-1839), на телефоне «Sessions» по всем проектам (SPEC:1867-1870); колонки Done и превью последнего сообщения нет |
| Лента событий агентов (Activity) | хронологическая лента: кто закончил, кто ждёт, превью последнего ответа, по всем worktree | `docs/site/content/docs/activity.mdx:6-25` | **нет**: превью в строке нет (SPEC:1805-1808); «Requests» перечисляет только открытые запросы (SPEC:1608-1618) |
| Workspace Board | канбан рабочих мест по статусу todo / in-progress / in-review / completed | `src/shared/keybindings/definitions-core-1.ts:102-120`; `docs/site/content/docs/cli/worktree-checkpoints.mdx:13-24` | **нет**: SPEC:38: hqc «does not duplicate what Hermes offers (… kanban …)» |
| Статус от агента | агент пишет комментарий-статус рабочего места, человек видит его на карточке | `docs/site/content/docs/cli/worktree-checkpoints.mdx:5-51` | **нет**: ближайшее — агент переименовывает свою сессию (`hqc_rename`, SPEC:2439) |
| Значки состояния | working / waiting / done / blocked / idle у вкладки и строки | `docs/site/content/docs/model/agents-sessions.mdx:10-21` | **есть**: точка строки: ждёт, ошибка, работает, простаивает (SPEC:1803-1806) |
| Cmd-J: прыжок к сессии | недавние чаты и терминалы, цифры Cmd-1..6, поиск по worktree, вкладкам, PR | `docs/site/content/docs/model/quick-open.mdx:20-37` | **частично**: ⌘K ищет только проекты (SPEC:1789-1797) |
| Закреп, мультивыбор, порядок | закрепить worktree, Cmd/Shift-клик для массовых действий, перетаскивание проектов | `docs/site/content/docs/model/worktrees.mdx:113` | **частично**: избранное (SPEC:1620-1630), Reorder (SPEC:1840-1843), перетаскивание сессии на проект (SPEC:1819-1822); мультивыбора нет (`multi-?select\|shiftKey` в client — только сочетания клавиш) |
| Фильтры списка | скрыть спящие, созданные автоматизацией или CLI, чужих клиентов, detached HEAD | `docs/site/content/docs/model/worktrees.mdx:97-107` | **нет**: в списке есть порядки и архив (SPEC:1833-1853), фильтров нет |
| Скрыть проект, не останавливая | проект уходит из списка, его агенты и автоматизации работают | `src/renderer/src/components/settings/repository/repository-project-presence-search.ts:30` | **частично**: архив проекта убирает его с панели (SPEC:1784-1789); что происходит с работающими сессиями проекта — не проверено |
| ID сессии для обращения агентов друг к другу | «Copy Orca Session ID» | `src/renderer/src/components/native-chat/pane/actions/NativeChatCopyOrcaSessionIdMenuItem.tsx:72` | **есть**: адрес `<project>/<slug>` (SPEC:1611-1612), в подсказке ссылки (SPEC:1993-1994); кнопки «скопировать адрес» нет (`copy (the )?(link\|address)` — 0) |
| Таймер кэша промпта | обратный отсчёт до истечения кэша промпта у Claude | `src/renderer/src/components/settings/agents/behavior/agent-cache-timer-search.ts:7` | **нет**: `prompt cache\|cache (timer\|expir)` — 0; есть только `cache` в строке расхода (`client/src/ui/cards.tsx:675`) |
| Заполнение контекста | кольцо «Context X of Y tokens, N% used» | `src/renderer/src/components/native-chat/usage/NativeChatContextUsageRing.tsx:103-212` | **нет**: `context window\|context usage\|tokens left\|context ring` — 0 |

## Разрешения

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Режимы разрешений | Ask for approval / Accept edits / Approve for me / Full access; терминальные агенты по умолчанию запускаются с флагами полной автономии | `src/renderer/src/components/native-chat/session/options/controls/native-chat-permission-mode-labels.ts:31-83`; `docs/site/content/docs/model/agents-sessions.mdx:61-65`; `src/renderer/src/components/settings/agents/agents-search.ts:225` | **частично**: Ask / Auto / Allow all (SPEC:2038-2055); Accept edits нет |
| Карточка разрешения | причина, «Needs access to», файл плана, «Answer in terminal» | `src/renderer/src/components/native-chat/prompts/NativeChatApprovalCard.tsx:154-231` | **есть**: варианты из запроса и текстовый ответ вместо разрешения (SPEC:1590-1595, 3868-3876) |
| Вопросы агента (AskUserQuestion) | карточка по шагам: Next, Skip, свой ответ, Submit | `src/renderer/src/components/native-chat/prompts/NativeChatQuestionCard.tsx:51-89`; `docs/site/content/docs/agents/native-chat.mdx:51-53` | **есть**: карточка вопросов (SPEC §10.1); Answer ждёт ответа на каждый вопрос, Skip нет (#73) |
| Общий список «ждут тебя» | колонка Needs You, вопросы и разрешения всех агентов | `docs/site/content/docs/model/agents-sessions.mdx:31` | **есть**: «Requests» со всеми открытыми запросами и цепочкой ждущих сессий (SPEC:1608-1618) |
| Ввод удерживает одно устройство | «Input is held by another device», «Take back» | `src/renderer/src/components/native-chat/composer/controls/native-chat-composer-placeholder.ts:22`; `src/renderer/src/components/native-chat/host-access/use-native-chat-mobile-hold-notice.ts:49` | **не нужно при хабе**: у hqc одна очередь ввода на хабе, пишущих может быть несколько (SPEC:4781) |

## Оркестрация

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Координатор и рабочие | Run, Tasks с зависимостями, Dispatch; `worker-start` в текущем, новом дочернем worktree или на другой машине (`--on`), любой агент, модель и effort на рабочего | `docs/site/content/docs/cli/orchestration.mdx:8-58, 103-119`; `skills/orchestration/SKILL.md` | **частично**: `hqc_spawn` с `isolation: "worktree"` на воркере проекта (SPEC:2270-2314); задач с зависимостями и учёта попыток нет (`dependen\|task graph\|dag` — 0); runtime только `claude_code` (SPEC:2283) |
| Координатор просыпается от почты | координатор кончает ход; Orca будит его, когда рабочий отчитался (`worker_done`), спросил или встал | `docs/site/content/docs/cli/orchestration.mdx:53-72` | **есть**: `hqc_send` будит цель (`wake_on_send`, SPEC:307, 2235-2236), ответ `hqc_ask` возвращается вызвавшему (SPEC:2240-2244); но `hqc_ask` держит вызов до ответа или таймаута, а `ask` Orca возвращается сразу |
| Вопросы рабочего и шлюзы решений | `ask` с вариантами; gate блокирует задачу до решения | `docs/site/content/docs/cli/orchestration.mdx:146-174` | **частично**: `hqc_ask` (SPEC:2240-2244); шлюзов нет |
| Групповые адреса | `@all`, `@idle`, `@claude`, `@worktree:<id>` | `docs/site/content/docs/cli/orchestration.mdx:129-142` | **нет**: `broadcast\|@all\|fan.?out` — по делу 0 |
| Запросы разрешений рабочих у координатора | карточки разрешений рабочих в чате координатора; по желанию координатор сам разрешает с причиной | `docs/site/content/docs/cli/orchestration.mdx:89-99`; `src/renderer/src/components/native-chat/worker-approvals/NativeChatWorkerApprovals.tsx:96-164`; `src/renderer/src/components/native-chat/worker-approvals/worker-approval-card-model.ts:79-104` | **частично**: все открытые запросы и цепочка ждущих — в «Requests», push ведёт в сессию, где начата работа (SPEC:1608-1618, 4656-4662); агент на запрос разрешения ответить не может (SPEC:4131) |
| Панель рабочих у координатора | «N need you», задание и последнее сообщение рабочего, «Mention in message» | `src/renderer/src/components/orchestration-crew/OrchestrationCrewPanel.tsx:59-104`; `src/renderer/src/components/orchestration-crew/CrewRowMenu.tsx:52` | **частично**: «started by <address>» в шапке рабочего (SPEC:2056-2059), сообщения между сессиями в лентах (SPEC:1991-2007); сводки рабочих у координатора нет |
| Освобождение и чистка рабочих | закончивший рабочий освобождается, его worktree удаляется правилом | `src/renderer/src/components/settings/orchestration/OrchestrationWorkerAutoReleaseSetting.tsx`; `src/renderer/src/components/settings/workspace-cleanup/workspace-cleanup-settings-search.ts:99` | **частично**: worktree от `hqc_spawn` помечается к удалению, когда архивирована его последняя сессия (SPEC:4833-4834) |
| Предел вложенности рабочих | настройка глубины вложенных рабочих | `src/renderer/src/components/settings/orchestration/nested-worker-depth-copy.ts` | **есть**: лимиты цепочек, спавнов и сообщений (SPEC:118-122) |

## Git и PR

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Worktree на задачу | «+» у проекта: worktree создаётся в фоне, выбор машины (Run on), агента | `docs/site/content/docs/model/worktrees.mdx:25-77` | **частично**: worktree вручную в настройках проекта (SPEC §13.1, 4290-4311) и `hqc_spawn isolation: worktree`; в окне новой сессии опции нет |
| Старт от ветки, коммита, удалённой ветки; родитель и потомки | start-from picker, родительское рабочее место в Advanced | `docs/site/content/docs/model/worktrees.mdx:29-36, 83-91` | **частично**: новая ветка worktree (SPEC:245-261), у `hqc_spawn` — от HEAD исходной (SPEC:2300-2301) |
| `orca.yaml`: setup, общие папки, копии `.env`, вкладки | setup-скрипт, `sharedDirectories`, `.worktreeinclude`, вкладки по умолчанию | `docs/site/content/docs/model/orca-yaml.mdx:10-46`; `docs/site/content/docs/model/worktrees.mdx:38-65` | **нет**: `setup (script\|hook\|command)\|post.?create` — по делу 0 |
| Рабочее место из issue и PR | из GitHub, GitLab, Linear, Jira, со ссылкой на задачу | `docs/site/content/docs/model/worktrees.mdx:75`; `docs/site/content/docs/review/github.mdx:41`; `docs/site/content/docs/review/linear.mdx:19` | **нет**: `pull request` — только правила deny (SPEC:4394) |
| Tasks: задачи GitHub, Linear, Jira, GitLab | список, правка полей, GitHub Projects | `docs/site/content/docs/review/github.mdx:39-55`; `docs/site/content/docs/review/linear.mdx:15-27`; `docs/site/content/docs/settings.mdx:99-106` | **нет**: `issue\|linear\|jira` — по делу 0 (только «issued», `linear-gradient`) |
| Дифф рабочего места | против базы, по коммитам, картинки, три стороны конфликта | `docs/site/content/docs/review/diff-viewer.mdx:9-18` | **частично**: дифф каждой правки (SPEC:3871-3876), diff-вложения (SPEC §12); сводного нет (SPEC:39) |
| Заметки к строкам диффа | комментарии на строках, «Send to agent» пачкой | `docs/site/content/docs/review/annotate-ai-diff.mdx:14-37`; `src/shared/keybindings/definitions-core-3.ts:132-141` | **нет**: `review (note\|comment)\|line comment\|annotat` — 0 |
| Строки, написанные ИИ | отметка в полях диффа | `docs/site/content/docs/review/attribution.mdx:5-19` | **нет**: в Orca это только документация: в коде и строках интерфейса не найдено (`aiAuthored\|lineAttribution\|ai-attribution` — 0) |
| Commit, push, «Fix with AI» | stage, commit, сообщение ИИ, push, force-with-lease, amend, «Resolve with AI» для конфликтов | `docs/site/content/docs/review/commit-push.mdx:7-57` | **нет**: `git push\|commit message` — только правила deny (SPEC:4394) |
| Pull request | создание с текстом ИИ, стеки, auto-merge, merge queue, ревью, ответы, реакции, «Fix broken checks» | `docs/site/content/docs/review/github.mdx:13-37`; `docs/site/content/docs/review/commit-push.mdx:23-29` | **нет**: `pull.?request\|merge request\|\bPR\b` — только записи о версиях SPEC и правила deny (SPEC:4394, 5093) |
| Провайдеры кода | GitHub, GitLab, Bitbucket, Azure DevOps, Gitea | `docs/site/content/docs/review/github.mdx:11`; `src/renderer/src/components/settings/integrations/integrations-search.ts:23-151` | **нет**: `github\|gitlab\|bitbucket\|gitea` — только `GITHUB_TOKEN` в `env` проекта (SPEC:4443, 4468) и адрес клона |
| Ветка по смыслу работы | авто-переименование ветки, когда агент начал | `src/renderer/src/components/settings/source-control/auto-rename-branch-search.ts:8` | **нет**: `rename (the )?branch\|branch name` — только проверка имени ветки (`crates/hqc-proto/src/frame.rs:713`) |
| Свежая main, fork от upstream | fast-forward базы при создании, синхронизация форка | `src/renderer/src/components/settings/source-control/git-search.ts:34`; `src/renderer/src/components/settings/repository/repository-search.ts:132` | **нет**: `fast.?forward\|upstream\|fork sync` — по делу 0 |
| Чистка рабочих мест по правилам | слитые, неизменённые, от автоматизаций и рабочих; предупреждение о `.env` | `src/renderer/src/components/settings/workspace-cleanup/workspace-cleanup-settings-search.ts:20-134` | **частично**: удаление того, что создал hqc (SPEC §13.2), эфемерные worktree (SPEC:4833-4834) |
| Папка с несколькими репозиториями | один проект над несколькими репо, чат в родительской папке | `docs/site/content/docs/model/worktrees.mdx:129-135` | **есть**: рабочая папка — любая папка (`dir`, SPEC:245) |

## Терминал, браузер, редактор, устройства

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Терминал | xterm.js с WebGL, сплиты, поиск, темы, импорт Ghostty, Kitty, Warp | `docs/site/content/docs/terminal.mdx`; `src/renderer/src/components/settings/terminal/appearance/terminal-typography-search.ts:113` | **нет**: `pty\|xterm\|terminal panel` — только про «claude в терминале» (SPEC:364) |
| Плавающий терминал [десктоп] | терминал поверх всего по Mod+Alt+A | `docs/site/content/docs/terminal.mdx:138-145` | **нет**: `terminal\|xterm\|pty` — только про `claude` в терминале и зеркало |
| Quick Commands | сохранённые команды и промпты агентам, общие с телефоном | `docs/site/content/docs/terminal.mdx:147-153`; `docs/site/content/docs/mobile.mdx:71-80` | **нет**: `template\|snippet\|saved prompt\|prompt preset` — только шаблон сессии проекта в бэклоге (SPEC:476-480, §13.3) |
| Вкладки и сплиты [десктоп] | агент, браузер, дифф рядом; раскладка своя у каждого рабочего места | `docs/site/content/docs/model/tabs-panes-splits.mdx:15-53` | **частично**: «Open in new tab» у сессии и проекта (SPEC:1782, 1816) |
| Браузер в рабочем месте [десктоп] | Chromium, профили, cookies, расширения Chrome, размер экрана | `docs/site/content/docs/browser/overview.mdx:7-62`; `src/renderer/src/components/settings/browser/browser-search.ts:181` | **нет**: `browser pane\|chromium\|playwright\|devtools` — только имена MCP-инструментов в ленте (SPEC:1921) |
| Design Mode [десктоп] | клик по элементу страницы — в чат с HTML, стилями, скриншотом | `docs/site/content/docs/browser/design-mode.mdx:7-27` | **нет**: то же, браузера нет |
| Агент водит браузер | `orca snapshot`, `click`, `fill`, `screenshot` | `docs/site/content/docs/cli/overview.mdx:88-105` | **нет**: у агента — только его собственные MCP |
| Редактор и Cmd+P | Monaco, Markdown-редактор, поиск файлов | `docs/site/content/docs/editing/monaco.mdx`; `docs/site/content/docs/editing/markdown.mdx`; `docs/site/content/docs/model/quick-open.mdx:10-12` | **нет**: «not an IDE» (SPEC:39) |
| Просмотрщики | PDF, CSV, ноутбуки, Mermaid, HTML, картинки | `docs/site/content/docs/editing/viewers.mdx:9-35` | **частично**: картинки и Markdown во вьювере, HTML в iframe (SPEC:4212, 4246-4252); PDF, CSV, ipynb — нет |
| Дерево файлов, файлы — агенту | проводник с git-статусом, перетащить файл в промпт | `docs/site/content/docs/editing/file-explorer.mdx:5-26` | **частично**: файлы перетаскиваются в поле ввода; дерева нет |
| Эмулятор iOS и Android [десктоп] | агент тапает и печатает в симуляторе | `docs/site/content/docs/cli/overview.mdx:107-121`; `docs/site/content/docs/cli/skills.mdx:28-29` | **нет**: `simulator\|emulator\|computer.?use` — 0 |
| Computer use [десктоп] | агент видит и водит приложения машины | `README.md:211`; `docs/site/content/docs/cli/skills.mdx:26` | **нет**: `simulator\|emulator\|computer.?use` — 0 |
| Порты рабочего места | живые порты в строке состояния, адреса localhost по рабочим местам | `src/renderer/src/components/settings/appearance/appearance-status-bar-search.ts:243`; `src/renderer/src/components/settings/browser/browser-search.ts:162` | **нет**: `port(s)? (list\|forward)\|localhost url` — 0 |
| Resource Manager | CPU, память, терминалы, диск рабочих мест; End терминала | `docs/site/content/docs/settings.mdx:29`; `docs/site/content/docs/terminal.mdx:13-21` | **нет**: `cpu\|memory usage\|disk usage` — 0 |
| Публичная ссылка на HTML и Markdown | Share as artifact через аккаунт Orca | `docs/site/content/docs/settings.mdx:91-97`; `docs/site/content/docs/cli/overview.mdx:84-86` | **нет**: `hqc_publish\|public link` — 0; есть подписанная ссылка на 10 минут (SPEC:1602-1606); публикация по ссылке готовится отдельным PR |

## Машины и доступ

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Один клиент ко многим машинам | агенты работают, когда ноутбук спит | `docs/site/content/docs/ways-to-run.mdx:43-53`; `docs/site/content/docs/remote-servers.mdx:99-111` | **есть**: хаб и воркеры (SPEC:43-57) |
| Remote Orca Server | ссылка доступа по Tailscale или LAN; `orca serve` без окна; сервер для Linux отдельным пакетом | `docs/site/content/docs/remote-servers.mdx:47-208` | **есть**: воркер — один бинарник, сам подключается к хабу (SPEC:48-49, 4865-4866) |
| Orca Relay | телефон и компьютер без проброса портов, через облако Orca, нужен вход в аккаунт | `docs/site/content/docs/mobile.mdx:39-44, 122-133`; `cloud/apps/relay` | **есть**: хаб за Cloudflare Tunnel и Access (SPEC:55-56) |
| Сквозное шифрование | «Everything is encrypted end-to-end» у пары телефон–компьютер и веб-клиента | `mobile/src/i18n/locales/en.json:619`; `src/renderer/src/web/web-e2ee.ts` | **нет**: `end-to-end\|e2ee` — 0; хаб видит всю историю |
| SSH-цели | агенты на удалённой машине, интерфейс — на ноутбуке | `docs/site/content/docs/ways-to-run.mdx:29-41` | **не нужно при хабе**: воркер ставится на саму машину |
| Cloud VM на рабочее место | по рецепту из `orca.yaml`: Vercel Sandbox, Fly, Modal, Docker | `docs/site/content/docs/ways-to-run.mdx:72-111` | **нет**: `isolation` — только `worktree` (SPEC:2295) |
| Пары, отзыв устройств | код или QR, отдельный токен на клиента, отзыв в списке | `docs/site/content/docs/mobile.mdx:37-59`; `docs/site/content/docs/remote-servers.mdx:113-121` | **частично**: воркер подключается кодом и отзывается (SPEC:1637-1645); браузеры входят через Access или OIDC, списка устройств нет |
| Веб-клиент | браузер подключается к Orca по коду | `src/renderer/src/components/settings/runtime-environments/catalog/runtime-environments-search.ts:105`; `src/renderer/src/web/WebConnect.tsx:135-166` | **есть**: hqc и есть веб-клиент (SPEC:53) |
| Имя машины | имя, под которым компьютер виден другим | `src/renderer/src/components/settings/machine-identity/machine-name-search.ts:19` | **есть**: имя воркера (SPEC:1641) |
| Не давать машине уснуть [десктоп] | Caffeinate: всегда или пока работает агент | `docs/site/content/docs/settings.mdx:65` | **нет**: `caffeinate\|keep (the )?(machine\|computer) awake\|prevent sleep` — 0 |
| Предел памяти терминала на сервере | cgroup `MemoryMax` на терминал | `docs/site/content/docs/remote-servers.mdx:210-225` | **нет**: `memorymax\|cgroup` — 0 |
| WSL-дистрибутив как хост [десктоп] | Orca запускает свой сервер внутри дистрибутива | `src/renderer/src/components/settings/wsl/hosts/wsl-hosts-experimental-search.ts:8` | **не проверено**: воркер внутри WSL как на Linux — не проверялось |

## Телефон

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Нативные приложения [моб] | iOS (App Store) и Android (Google Play, с mobile-android-v0.0.65) | `docs/site/content/docs/mobile.mdx:8-14`; релиз mobile-android-v0.0.65 | **частично**: PWA, иконка на экран «Домой» (SPEC:1863-1887) |
| Вкладка Agents | все агенты всех компьютеров, группировка по статусу, рабочему месту, проекту, агенту, компьютеру; Mark as unread, Clear finished | `mobile/src/i18n/locales/en.json` (`agents.*`, около 4978-4994); релиз mobile-android-v0.0.65 | **частично**: «Sessions»: живые сессии всех проектов по состояниям с непрочитанным (SPEC:1867-1872); пометить непрочитанным и убрать законченные — нет |
| Лента и расход на главной | вкладки Feed и Usage; счётчики Agents spawned, Agent time, PRs created | `mobile/src/i18n/locales/en.json:651, 674, 676` | **нет**: на первом экране — проекты или сессии (SPEC:1863-1867) |
| Чат на телефоне | `@` файлы, `/` команды, выбор модели, фото, диктовка | `docs/site/content/docs/mobile.mdx:61-69` | **частично**: чат, вложения, `/` (SPEC:1863-1887); `@`, диктовки нет |
| Терминал на телефоне [моб] | живой терминал, режим ввода без Return, Quick Commands, настройки шрифта | `docs/site/content/docs/mobile.mdx:24, 71-80, 113-118` | **нет**: `terminal\|xterm\|pty` — терминала нет |
| Файлы на телефоне [моб] | дерево и просмотр файлов рабочего места | `mobile/app/h/[hostId]/files` | **нет**: дерева файлов нет и на компьютере (SPEC:39) |
| Source control и PR на телефоне [моб] | stage, commit, push; PR: merge, ревьюеры, треды, «Fix with AI»; ревью диффа | `docs/site/content/docs/mobile.mdx:105-111`; `mobile/app/h/[hostId]/{source-control,pr,review}`; `mobile/src/i18n/locales/en.json` (`pullRequest.*`) | **нет**: `pull.?request\|\bPR\b`, `git push\|commit message` — только правила deny (SPEC:4394) |
| Браузер на телефоне [моб] | страница рабочего места в виде Web или Mobile | `docs/site/content/docs/mobile.mdx:105-111` | **нет**: браузера нет и на компьютере |
| Аккаунты и лимиты | аккаунты агентов, сброс Codex, лимиты | `docs/site/content/docs/mobile.mdx:86-90`; `mobile/app/h/[hostId]/accounts.tsx` | **частично**: лимиты воркера сессии в меню More (SPEC:1634-1636) |
| Новое рабочее место с телефона | по issue, ветке, агенту | `docs/site/content/docs/mobile.mdx:92-94`; `mobile/app/h/[hostId]/new-workspace.tsx` | **частично**: новая сессия с чипами проекта и папки (SPEC:1889-1907) |
| Автоматизации на телефоне | список, запуск, правка | `mobile/app/h/[hostId]/automations.tsx` | **частично**: Schedules в меню More: список, пауза, удаление; создаёт только агент (`client/src/ui/Schedules.tsx:1-2`; SPEC:2338) |
| Диктовка на телефоне [моб] | телефон пишет звук и шлёт кусками на свой компьютер, там он распознаётся | `mobile/src/dictation/mobile-dictation-operations.ts:52-66`; `mobile/src/hooks/mobile-dictation-audio-chunk.ts:7` | **нет**: `dictat\|microphone\|getUserMedia\|speech` в SPEC и client — 0 |
| Push с фильтрами | только когда отошёл от компьютера (3 минуты без клавиатуры или заблокирован), только когда агент ждёт, звук; пауза через 7 дней без входа | `mobile/src/i18n/locales/en.json:878, 884` (`notifications.*`) | **частично**: Web Push запросов и ответов, без push, пока сессия открыта (SPEC:4637-4642, 4660-4661); на айфоне — только из PWA (SPEC:4642-4643); фильтров нет |
| Несколько хостов, правка адреса хоста [моб] | телефон хранит пары с каждым компьютером | `docs/site/content/docs/mobile.mdx:37-59` | **не нужно при хабе**: один хаб на все воркеры (SPEC:43-57) |

## Уведомления

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Категории уведомлений | агент закончил, ждёт ввода, звонок терминала; выбор машин; тест | `src/renderer/src/components/settings/notifications/notifications-search.ts:7-217`; `docs/site/content/docs/notifications.mdx:7-30` | **частично**: один переключатель на устройство (`client/src/ui/Settings.tsx:25-31`); события — запросы, финальный ответ, сообщение сессии, недоставленный ввод (SPEC:4637-4641) |
| Колокольчик и «Mark unread» | список непрочитанных в шапке, вернуть непрочитанным | `docs/site/content/docs/notifications.mdx:32-41`; `docs/site/content/docs/activity.mdx:6-25` | **частично**: непрочитанное у строк и проектов, точка на вкладке браузера (SPEC:1715-1717); вернуть непрочитанным нельзя (`mark (as )?(un)?read` — 0) |
| Значок в Dock [десктоп] | число ждущих на значке | `docs/site/content/docs/notifications.mdx:43` | **нет**: `setAppBadge\|badge` — только значки в списках; оболочка для macOS со значком готовится отдельным PR |
| Звуки | свой звук MP3, WAV, OGG, M4A, AAC, FLAC, громкость | `docs/site/content/docs/notifications.mdx:45-52`; `src/renderer/src/components/settings/notifications/notifications-search.ts` | **нет**: `sound\|audio` в SPEC и client — 0 |
| Не уведомлять, когда смотришь | «Suppress while focused» | `src/renderer/src/components/settings/notifications/notifications-search.ts` | **есть**: нет push, пока сессия открыта или открыта ждущая её (SPEC:4641-4642, 4660-4661) |
| PR и обновления | упала проверка PR, вышло обновление | `docs/site/content/docs/settings.mdx:110-113` | **нет**: `check(s)? fail\|ci (failing\|green)\|update available` — по делу 0 |

## Модели, аккаунты, расход

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| 34 агента [мульти] | Claude Code, Codex, Gemini, OpenCode, Cursor CLI, Aider и др. | `README.md:216-256` | **частично**: только Claude Code (SPEC:2283; `crates/hqc-worker/src/adapter.rs`) |
| Несколько аккаунтов | несколько аккаунтов Claude и Codex, переключение на ходу | `docs/site/content/docs/settings.mdx:68-72`; `src/renderer/src/components/settings/accounts/accounts-claude-search.ts:7-26`; `docs/site/content/docs/agents/claude-code.mdx:21-23` | **частично**: аккаунт — тот, под которым залогинен воркер (`crates/hqc-worker/src/adapter.rs:73-74`); разные машины — разные аккаунты |
| Лимиты провайдеров | 5 часов, день, неделя всех провайдеров; предупреждение на 80%; формат в строке состояния | `docs/site/content/docs/agents/usage-tracking.mdx:5-40`; `src/renderer/src/components/settings/appearance/appearance-status-bar-usage-format-search.ts:8` | **частично**: пять часов и семь дней Claude у воркера сессии (`client/src/ui/limits.tsx:1-46`; SPEC:1634-1636) |
| Статистика расхода | Stats: оценка стоимости, кэш, топ проектов, 7 дней, «Share on X» | `docs/site/content/docs/agents/usage-tracking.mdx:41-55`; строки `ClaudeUsagePane` в `src/renderer/src/i18n/locales/en.json` | **частично**: расход хода и итог сессии (SPEC:513; `client/src/ui/cards.tsx:672-676`); сводки по дням и проектам нет |
| Продолжение после сброса лимита | «Continue where you left off» само, когда лимит сбросился; «Continue Now» | `src/renderer/src/components/settings/chat/chat-behavior-search.ts:29`; `src/renderer/src/components/native-chat/usage/NativeChatUsageLimitAutoContinue.tsx:37-57`; `src/renderer/src/components/native-chat/messages/notices/NativeChatNoticeRow.tsx:361-364` | **нет**: `auto.?continue\|continue where you left` — 0 |
| Продолжить на другом аккаунте | упёрся в лимит — продолжить разговор под другим аккаунтом | релиз v1.4.259, строка 77; `src/renderer/src/components/native-chat/usage/NativeChatUsageLimitActions.tsx:254` | **нет**: `another account\|switch account` — 0; аккаунт задан входом на воркере |
| Опрос лимитов | OAuth-запрос использования или статус-строка CLI | `docs/site/content/docs/agents/usage-tracking.mdx:24-40` | **есть**: из `rate_limit_event` Claude Code (`crates/hqc-proto/src/frame.rs:1102-1103`; SPEC:1235-1236) |

## Автоматизация, CLI, навыки, плагины

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Автоматизации | расписание пресетом, cron или RRULE; новая сессия в репо или в рабочем месте; precheck; пропущенные запуски; Run now, Rerun, история | `docs/site/content/docs/cli/automations.mdx:5-155` | **частично**: `hqc_schedule`: сообщение в существующую сессию по `at` или `cron`, `catch_up` (SPEC:2329-2338, 4664-4699); создание сессии на запуск описано (SPEC:4665-4666), но в `crates/hqc-hub/src/schedules.rs` слова `workspace` нет; precheck, истории запусков нет |
| CLI `orca` | рабочие места, терминалы, браузер, оркестрация из любой оболочки и агентом | `docs/site/content/docs/cli/overview.mdx:15-127` | **частично**: агенту — MCP-инструменты hqc (SPEC §7, 2213-2456); человеку CLI нет |
| Навыки | установка, обновление, обмен по ссылке | `docs/site/content/docs/cli/skills.mdx:16-140` | **нет**: навык виден только именем в строке вызова (SPEC:1922) |
| Реестр MCP-серверов | поиск и установка MCP-серверов, настройка | `docs/site/content/docs/cli/skills.mdx:141-181`; `src/renderer/src/components/settings/repository/mcp/mcp-config-search.ts:14` | **нет**: `mcp server\|mcp registry\|install (a )?skill\|plugin` — MCP-серверы машины задаются только локальным конфигом воркера (SPEC:87) |
| Плагины | плагины Orca | `docs/site/content/docs/settings.mdx:170-175`; `docs/reference/plugin-*.md` | **нет**: тот же поиск — плагины видны только как префикс имени MCP-сервера в ленте (`client/src/ui/feed.ts:431`) |
| Коннекторы Slack, Discord, Linear | — | `src/main/connector-cloud/connector-hello.ts:13`; `cloud/packages/connector-contract/src/connector-wire-scalars.ts:16` | **не проверено**: в документации и строках интерфейса Orca не найдено; у hqc Telegram — через Hermes (SPEC:38, 4643-4644) |

## Вид, клавиатура, настройки

| Функция | Что получает пользователь | Где в Orca | hqc |
| --- | --- | --- | --- |
| Темы, акцент, плотность | тема, цвет акцента, плотность интерфейса | `docs/site/content/docs/settings.mdx:24-32`; `src/renderer/src/components/settings/appearance/appearance-search.ts:18-126` | **частично**: шесть палитр (три тёмные и три светлые), Compact и Spacious, цвет проекта (SPEC:159-163, 1705-1741); своего акцента нет |
| Язык интерфейса | 12 языков, в том числе русский | `docs/site/content/docs/settings.mdx:24-32` | **нет**: интерфейс английский, даты `toLocaleString('en-US')` (`client/src/ui/labels.ts:126-145`) |
| Иконка приложения [десктоп] | выбор иконки | `src/renderer/src/components/settings/appearance/appearance-app-icon-search.ts:8` | **нет**: `app icon\|pet\|mascot` — по делу 0 |
| Сочетания клавиш | 100 действий, переназначаются, `~/.orca/keybindings.json` | `docs/site/content/docs/settings.mdx:143-154`; `src/shared/keybindings/definitions-core-{1,2,3,4}.ts` | **частично**: ⌘K — поиск проекта (SPEC:1790), ⌘\ — колонка сессий (SPEC:1857); переназначить нельзя |
| Поиск по настройкам | Cmd-, и поиск | `docs/site/content/docs/settings.mdx:5`; `src/shared/keybindings/definitions-core-3.ts:214` | **нет**: `settings search` — 0 |
| Питомец | анимированный питомец | `src/renderer/src/components/settings/advanced/experimental-search.ts:13` | **нет**: `app icon\|pet\|mascot` — по делу 0 |
| Подтверждения | настройки «спрашивать перед…» | `src/renderer/src/components/settings/general/general-confirmations-search.ts:7-94` | **есть**: окна удаления и архив с Undo (SPEC:1818, 1843-1847) |
| Телеметрия | отправка статистики, выключается | `src/renderer/src/components/settings/privacy/privacy-search.ts:14-67` | **не нужно при хабе**: у hqc телеметрии нет (`telemetry\|analytics` — 0) |
| Трей и строка меню [десктоп] | иконка в трее, окно в фоне | `src/renderer/src/components/settings/appearance/appearance-system-presence-search.ts:10-52` | **нет**: у воркера трей описан как необязательный (SPEC:4866), в коде его нет (`crates/hqc-worker/src/connection.rs:74`: «later the tray») |

## Не проверено

- 23 файла документации Orca не открывались: `agents/antigravity-accounts.mdx`,
  `agents/antigravity.mdx`, `agents/codex-hot-swap.mdx`, `agents/codex.mdx`, `agents/cursor-cli.mdx`,
  `agents/glm-agent.mdx`, `agents/supported.mdx`, `android-apk.mdx`, `browser/profiles.mdx`,
  `cli/computer-use.mdx`, `cli/reference.mdx`, `github-errors.mdx`, `index.mdx`, `install.mdx`,
  `recipes/design-mode-fix.mdx`, `recipes/jump-worktrees.mdx`, `recipes/parallel-agents.mdx`,
  `recipes/remote-worktrees.mdx`, `recipes/review-ai-diff.mdx`, `review/jira.mdx`, `ssh.mdx`,
  `telemetry.mdx`, `troubleshooting.mdx`. Заметки к v1.4.259 прочитаны на 340 строк из 636.
- Orca не запускалась: всё о ней — по документации, строкам интерфейса и коду.
- Отметка строк, написанных ИИ, есть только в документации Orca, в коде не найдена.
- Работает ли `/goal` Orca с Claude, а не только с Codex.
- Коннекторы Slack, Discord, Linear — найдены только код протокола и облака, без документации и
  интерфейса.
- Получает ли push веб-клиент Orca (не приложение).
- hqc: воркер внутри WSL; что архив проекта делает с работающими сессиями; переживают ли вложения
  переподключение в черновике; переключатель переноса строк в блоке кода.
