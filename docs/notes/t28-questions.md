# T28: translation to English — glossary and open questions

From T28 on, everything in the repository is written in English: the spec, docs, notes, the
client UI and the texts the hub and the worker show to the user. The terms below are used
everywhere, so the spec, the docs and the UI say the same thing.

## Glossary

### Spec terms

| Russian | English |
|---|---|
| ход | turn |
| голая модель | bare model |
| подхват, подхватить | takeover, take over |
| вернуть (сессию в VS Code) | release (back to VS Code) |
| владелец воркера / сессии | worker owner / session owner |
| воркер, хаб, клиент | worker, hub, client |
| привязка | binding |
| окружение | environment |
| зеркало, зеркалирование | mirror, mirroring |
| рантайм | runtime |
| ввод | input |
| запрос разрешения | permission request |
| одобрение, политика одобрений | approval, approval policy |
| вложение | attachment |
| артефакт | artifact |
| шаринг, поделиться | sharing, share |
| служебная сессия воркера | the worker's service session |
| развилка | fork |
| регистрация воркера (enroll) | enrollment |
| перерегистрация / замена воркера | re-enrollment / worker replacement |
| отзыв токена | token revocation |
| заголовок сессии | session title |
| разрешённые корни | allowed roots (`allowed_roots`) |
| сторожевой таймер | watchdog |
| фикстура | fixture |
| ревью | review |
| **[решение]** | **[decision]** |
| **[проверить]** | **[verify]** |

### UI labels (client and the texts the hub and the worker write)

| Russian | English |
|---|---|
| Воркеры (group of service sessions) | Workers |
| Архив / В архив / Вернуть | Archive / Archive / Restore |
| Личный (personal project) | Personal |
| + проект / Новый проект / Новая сессия | + project / New project / New session |
| Сессия N (default title) | Session N |
| Сессии VS Code (switch) | VS Code sessions |
| Подхватить / Забрать сюда | Take over / Take over here |
| ждёт вас | needs you |
| Разрешить / Всегда в этой сессии / Разрешать всегда / Отклонить / Запрещать всегда | Allow / Always in this session / Always allow / Deny / Always deny |
| Ответить текстом вместо этого | Reply with text instead |
| Подхвачена из VS Code / Возвращена в VS Code / Развилка | Taken over from VS Code / Returned to VS Code / Fork |
| Запрещено политикой воркера | Blocked by worker policy |
| Работает / Свободна / Ждёт ответа / В очереди / Ведётся в VS Code / Воркер офлайн / В архиве / Ошибка | Running / Idle / Waiting for reply / Queued / In VS Code / Worker offline / Archived / Error |
| пропущено N (worker log) | skipped N |
| Вложения: (block appended to the agent's input) | Attachments: |
| только запросы | requests only |
| вне рабочей папки | outside the working folder |

Russian «quotes» around a UI label become "double quotes" in the docs and “curly quotes” in
user-visible strings in code.

## Notes

- `docs/design/redesign-2026-10-24` (the mockups of T26) is left in Russian: it is a design
  input, not a document the code follows. Its labels map to the English UI through the table
  above.
- Fixtures recorded from real runs (`crates/hqc-claude-jsonl/fixtures`) and conversation text
  that tests use as data (Cyrillic file names, titles in the user's language, slug and
  percent-encoding checks) stay as they are.
- The worker's title prompt is now in English and asks for a title in the language of the
  conversation, so a Russian conversation still gets a Russian title.
- The block heading that lists attachments in the agent's input is now `Attachments:`.

## Questions
Places where the Russian was ambiguous; the translation keeps the reading below. Worth a look
when the section is next touched.

1. SPEC header, status line: "the architecture agreed after six reviews and a survey of similar
   systems (§22), before implementation started" — translated as written, though implementation
   is well under way. The legend says **[decision]** is "decided, needs confirmation", while
   `CLAUDE.md` says "decided, ask before changing"; both kept as they were.
2. SPEC §3 Session: "по нему считается тишина для подхвата" → "it is how quiet is measured for a
   takeover" (quiet = time since the last write to the transcript).
3. SPEC §5.2: "при таком опоздании `ack` остаётся «идущим»" → the *turn* stays "in progress"
   when its `ack` is late, not the `ack` itself.
4. SPEC §6: `<режим "спрашивать" | auto>` → `<"ask" mode | auto>`: a description of the mode, not
   a literal CLI value (that is `manual` or `default`). "приём из Agent Host Protocol" → "a
   technique from Agent Host Protocol".
5. SPEC §8.1, permission policy: "второй линии" → "a second line of defence" behind the CLI.
6. SPEC §9: "base36 от модуля 32-битного хэша" → "base36 of the absolute value of a 32-bit
   hash" (модуль read as absolute value, not modulo).
7. SPEC §9.2: "Так своими становятся запись команды `/compact` … и промпт" → this makes the
   `/compact` record and the prompt (when its echo has another uuid) count as own records.
8. SPEC §11.1: "долгосрочную память владельца (Hindsight) на чтение и запись" → "read and write
   access to the owner's long-term memory (Hindsight)".
9. SPEC §13.1: "кроме случая, когда он сам инициатор" → "unless the owner is the one who
   initiated it" (он read as the worker owner).
10. SPEC §18: "**[проверить]** актуальность" → "**[verify]** that it is current".
11. SPEC §23, 0.10 → 0.11: "снято прежнее «не отличает»" → the earlier rule that the hub does not
    tell a reconnect from a restart is dropped.
12. ITERATION-2 T17: "Закрывает A2 аудита частично" → "Partly closes audit item A2"; T20
    "плагин" (the VS Code side) → "the extension".
13. t5-questions item 16: the old button "Разрешить в сессии" → "Allow for this session" (the
    wording of that time), not today's "Always in this session". Item 22 ("the interface
    language is Russian, no i18n") is kept as a fact about that time; T28 supersedes it.
14. `docs/notes/claude-control-protocol.md`, T24: the command line now shows the English title
    prompt; the recorded live runs used its Russian predecessor and their Russian titles stay as
    recorded.
15. t17-questions, migration `0006`: the garbled "служебные сессии и отозванным воркерам" is read
    as "service sessions also for revoked workers (the history stays)".

Not translated on purpose:

- `crates/hqc-hub/migrations/0004_title_source.sql` and `0009_worker_replace.sql` keep their
  Russian comments: sqlx stores a checksum of every applied migration, so editing a comment
  would make a deployed hub refuse to start ("migration was previously applied but has been
  modified").
- `docs/notes/t26-questions.md`: the `message` recorded in a real `deny` run stays in Russian
  (with an English gloss): the point of that run is that the model answered in the user's
  language.
