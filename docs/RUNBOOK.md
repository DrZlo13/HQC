# hqc — как поднять у себя (итерация 1)

Для человека, который ставит hqc на свои машины: хаб в домашней DMZ за Cloudflare Tunnel,
воркер на маке, клиент в браузере телефона. Архитектура и протокол — `docs/SPEC.md`
(§2, §5, §6, §20), здесь только эксплуатация. Все команды, переменные и поля конфига ниже
взяты из кода (`crates/hqc-hub/src/config.rs`, `crates/hqc-worker/src/config.rs`,
`crates/hqc-worker/src/main.rs`); чего здесь нет — того в итерации 1 нет.

Схема:

```
телефон / браузер ──HTTPS──> Cloudflare (Access) ──Tunnel──> cloudflared ──> hqc-hub :8080 ──> Postgres
                                                                               ^
home-mac: hqc-worker run ──HTTPS/WSS /worker/* (без Access, токен воркера)─────┘
          └─ claude -p (подписка, залогиненная на этой машине)
```

## 0. Сборка

Нужны Rust stable (версия зафиксирована в `rust-toolchain.toml`, `rustup` поставит её сам)
и Node.js 20.19+ для сборки клиента (для его тестов — 22+). Хаб для DMZ собирать не нужно:
есть готовый Docker-образ с клиентом внутри (§1.8).

```sh
git clone <repo> hqc && cd hqc
cargo build --release -p hqc-hub -p hqc-worker   # target/release/hqc-hub, target/release/hqc-worker
target/release/hqc-hub --version                  # hqc-hub 0.1.0
target/release/hqc-worker --version               # hqc-worker 0.1.0
cd client && npm ci && npm run build              # client/dist
```

Хаб собирайте на той ОС и архитектуре, где он будет работать (на Linux-хосте DMZ — там
же или в CI; Docker-образ собирает CI, §1.8), воркер — на маке. Бинарники самодостаточны: Postgres-клиент встроен,
SQLite outbox воркера тоже.

## 0.1 Локальная разработка без Cloudflare

`tools/dev.sh up` собирает хаб и воркер в dev-профиле, поднимает хаб в dev-режиме на
`127.0.0.1:8080` (база `hqc_dev`, пользователь `dev@local`, клиент из `client/dist`),
при первом запуске регистрирует воркер с отдельным `HQC_HOME` в `tmp/dev/home` и запускает
его, а когда воркер online — добавляет через REST workspace с путём репозитория
(`DEV_WORKSPACE`, внутри `allowed_roots` = `DEV_ROOTS`) в личный проект `dev@local`, если его
там ещё нет; остальные workspace добавляются в клиенте. `down`, `restart`, `status`,
`logs`, `reset` — по именам. Состояние живёт между запусками. С другой машины хаб открывается через ssh-проброс:
`ssh -N -L 8080:127.0.0.1:8080 <user>@<host>`, затем `http://127.0.0.1:8080`. Хаб слушает
только loopback, и в dev-режиме это обязательно (§6). Воркер запускает `claude`, поэтому
`up` надо выполнять из сессии с разблокированной связкой Keychain (§2.3).

## 1. Хаб в DMZ

### 1.1 Postgres

Хаб работает с Postgres 17 (проверено; расширения в итерации 1 не нужны). Подключение
без TLS — sqlx собран без TLS, поэтому база должна быть на том же хосте, что и хаб
(или в доверенной сети без `sslmode=require` в строке подключения).

```sh
# Linux (Debian/Ubuntu), от администратора:
sudo -u postgres createuser --pwprompt hqc
sudo -u postgres createdb -O hqc hqc
# macOS + Homebrew:
brew install postgresql@17 && brew services start postgresql@17
createdb hqc
```

**Миграции** применяются автоматически при каждом старте хаба (встроены в бинарник из
`crates/hqc-hub/migrations`, таблица `_sqlx_migrations`). Отдельной команды нет;
`sqlx-cli` не нужен. Перед обновлением хаба — бэкап: `pg_dump -Fc hqc > hqc-$(date +%F).dump`.

### 1.2 Переменные окружения хаба

Хаб настраивается только переменными окружения; аргумент командной строки один —
`--version`. Пустое значение считается незаданным.

| переменная | обязательна | что это |
|---|---|---|
| `HQC_DATABASE_URL` | да | строка подключения Postgres, например `postgresql://hqc:<пароль>@127.0.0.1/hqc` |
| `HQC_HUB_LISTEN` | нет, `127.0.0.1:8080` | адрес и порт HTTP. Хаб слушает только интерфейс, на который ходит `cloudflared` (§20); с туннелем на том же хосте — loopback. В Docker-образе — `0.0.0.0:8080` внутри контейнера, порт наружу не публикуется (§1.8) |
| `HQC_ACCESS_TEAM_DOMAIN` | да (без `HQC_DEV`) | домен команды Cloudflare Zero Trust, `<team>.cloudflareaccess.com` (можно с `https://` и `/` — хаб обрежет). Из него хаб берёт issuer `https://<team>.cloudflareaccess.com` и JWKS `https://<team>.cloudflareaccess.com/cdn-cgi/access/certs` |
| `HQC_ACCESS_AUD` | да (без `HQC_DEV`) | Application Audience (AUD) tag Access-приложения хаба; хаб проверяет `aud` в JWT `Cf-Access-Jwt-Assertion` |
| `HQC_VAPID_PRIVATE` | нет | закрытый ключ VAPID для Web Push: 32 байта скаляра P-256, base64url без `=` (§1.5). Задаётся вместе с `HQC_VAPID_PUBLIC` или не задаётся вовсе — иначе хаб не стартует |
| `HQC_VAPID_PUBLIC` | нет | открытый ключ VAPID: несжатая точка P-256 (65 байт), base64url без `=`. Его отдаёт `GET /api/push/vapid`. Без пары ключей push выключен, клиент пишет «На хабе не настроены ключи VAPID» |
| `HQC_VAPID_SUBJECT` | нет, `mailto:hqc@localhost` | `sub` в подписи VAPID: `mailto:` или `https:`. Задайте настоящий адрес — push-сервисы (особенно Apple) могут отвергать подписи с выдуманным |
| `HQC_CLIENT_DIR` | нет | каталог собранного клиента (`client/dist`). Если задан, хаб отдаёт клиент на `/` того же домена; без `index.html` в нём хаб не стартует. Не задан — хаб отдаёт только `/api/*`, `/worker/*` и `/health` |
| `HQC_DEV` | нет | `1` — dev-режим **без Access**: все запросы `/api/*` идут от `HQC_DEV_USER`. Только для локальной разработки: хаб откажется стартовать, если при этом задан любой `HQC_ACCESS_*` или `HQC_HUB_LISTEN` не loopback (`127.0.0.0/8`, `[::1]`, `localhost`). В DMZ не использовать |
| `HQC_DEV_USER` | с `HQC_DEV=1` | email пользователя dev-режима; без `HQC_DEV=1` игнорируется с предупреждением |
| `HQC_ARTIFACT_DIR` | нет, `artifacts` (в Docker-образе `/var/lib/hqc/artifacts`) | каталог хранилища артефактов (SPEC §12): файлы по sha256, `<dir>/<первые 2 знака>/<sha256>`, и `tmp/` для загрузок. Хаб создаёт его при старте и не стартует, если не может. Бэкапить вместе с базой (SPEC §20). Удаления артефактов пока нет |
| `HQC_ARTIFACT_PUBLIC_BASE` | нет | origin отдельного домена артефактов, `https://artifacts.<домен хаба>` без пути (`http://` — только loopback). Подписанные ссылки ведут туда, HTML рендерится в iframe только оттуда; хаб добавляет этот origin в `img-src`/`frame-src`/`connect-src` своей CSP, а на этом хосте отдаёт только `/artifacts/*` (остальное — `404`). Домен — тот же туннель на тот же хаб, **вне** Access (§1.6). Не задан — ссылки относительные, на домене хаба (за Access), HTML только скачивается |
| `HQC_ARTIFACT_KEY` | нет | ключ HMAC подписанных ссылок, не короче 32 символов (`openssl rand -hex 32`). Не задан — случайный на каждый запуск: ссылки прошлого запуска перестают работать, клиент просто запрашивает новые. Нужен, только если ссылки должны переживать перезапуск |
| `HQC_PUBLIC_ORIGINS` | нет | origin'ы, по которым браузеры открывают хаб, через запятую: `https://hqc.example.com`. Когда задано, `Origin` на `/api/*` (WebSocket и запись) сверяется с этим списком, а не с `Host` запроса. Нужно, если перед хабом стоит прокси, который переписывает `Host` в имя контейнера (Caddy с `header_up Host`, cloudflared с `httpHostHeader`) и не ставит `X-Forwarded-Host`; симптом — `WebSocket connection to wss://…/api/ws failed` в браузере и `Origin does not match the hub's host` в логе хаба |
| `HQC_TURN_WATCHDOG_S` | нет, `900` | сторожевой таймер хода (SPEC §17.3), целые секунды: если сессия в `running`, а от воркера по ней нет ни одного события (включая `agent.delta`) дольше этого, хаб закрывает ход с `system turn_lost` «no events for N s», и сессия в `error` до следующего ввода. Ожидание ответа на approval/clarify и офлайн воркера не считаются. `0` — выключить |
| `RUST_LOG` | нет, `info` | уровень логов (`tracing`), например `hqc_hub=debug` |

Мастер-ключа для `api_keys` (§3, §20) в итерации 1 нет — ключи пользователей не хранятся.

Хабу нужен исходящий HTTPS: к `<team>.cloudflareaccess.com` (JWKS) и к push-сервисам
браузеров (`fcm.googleapis.com`, `web.push.apple.com`, `updates.push.services.mozilla.com`
и т.п.).

Проверка после старта: `curl http://127.0.0.1:8080/health` → `ok`; в логе
`hqc-hub listening addr=… protocol=1`.

### 1.3 systemd (Linux)

Без Docker; с Docker — §1.8.

`/etc/hqc/hub.env` (права `0600`, владелец `root`; в файле пароль базы и ключ VAPID):

```sh
HQC_DATABASE_URL=postgresql://hqc:<пароль>@127.0.0.1/hqc
HQC_HUB_LISTEN=127.0.0.1:8080
HQC_ACCESS_TEAM_DOMAIN=<team>.cloudflareaccess.com
HQC_ACCESS_AUD=<AUD tag>
HQC_VAPID_PRIVATE=<base64url>
HQC_VAPID_PUBLIC=<base64url>
HQC_VAPID_SUBJECT=mailto:<you>@<domain>
HQC_CLIENT_DIR=/opt/hqc/client
RUST_LOG=info
```

`/etc/systemd/system/hqc-hub.service`:

```ini
[Unit]
Description=hqc hub
After=network-online.target postgresql.service
Wants=network-online.target
Requires=postgresql.service

[Service]
User=hqc
Group=hqc
EnvironmentFile=/etc/hqc/hub.env
ExecStart=/usr/local/bin/hqc-hub
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --no-create-home hqc
sudo install -m 0755 target/release/hqc-hub /usr/local/bin/
sudo mkdir -p /opt/hqc && sudo cp -r client/dist /opt/hqc/client
sudo systemctl daemon-reload && sudo systemctl enable --now hqc-hub
journalctl -u hqc-hub -f            # логи (хаб пишет в stdout)
```

Хаб читает файлы клиента при каждом запросе: новая сборка `client/dist` в
`HQC_CLIENT_DIR` подхватывается без перезапуска.

### 1.4 launchd (если хаб на маке)

`/Library/LaunchDaemons/local.hqc.hub.plist` (владелец `root:wheel`, права `0600` — в нём
секреты); `UserName` — непривилегированный пользователь, под которым работает хаб:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.hqc.hub</string>
  <key>UserName</key><string>hqc</string>
  <key>ProgramArguments</key>
  <array><string>/usr/local/bin/hqc-hub</string></array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HQC_DATABASE_URL</key><string>postgresql://hqc:PASSWORD@127.0.0.1/hqc</string>
    <key>HQC_HUB_LISTEN</key><string>127.0.0.1:8080</string>
    <key>HQC_ACCESS_TEAM_DOMAIN</key><string>TEAM.cloudflareaccess.com</string>
    <key>HQC_ACCESS_AUD</key><string>AUD_TAG</string>
    <key>HQC_VAPID_PRIVATE</key><string>BASE64URL</string>
    <key>HQC_VAPID_PUBLIC</key><string>BASE64URL</string>
    <key>HQC_VAPID_SUBJECT</key><string>mailto:you@example.com</string>
    <key>HQC_CLIENT_DIR</key><string>/usr/local/share/hqc/client</string>
    <key>RUST_LOG</key><string>info</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>/usr/local/var/log/hqc-hub.log</string>
  <key>StandardErrorPath</key><string>/usr/local/var/log/hqc-hub.log</string>
</dict>
</plist>
```

```sh
sudo launchctl bootstrap system /Library/LaunchDaemons/local.hqc.hub.plist
sudo launchctl kickstart -k system/local.hqc.hub     # перезапуск
sudo launchctl bootout system/local.hqc.hub          # остановка
```

Каталог лога должен существовать и быть доступен пользователю `hqc` на запись.

### 1.5 Ключи VAPID

Нужна пара P-256 в «сыром» виде, base64url без `=`. Через `openssl` (так же их делает
`tools/e2e-local.sh`, хаб такие ключи принимает):

```sh
openssl ecparam -name prime256v1 -genkey -noout -out vapid.pem
b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
echo "HQC_VAPID_PRIVATE=$(openssl ec -in vapid.pem -outform DER 2>/dev/null | tail -c +8 | head -c 32 | b64url)"
echo "HQC_VAPID_PUBLIC=$(openssl ec -in vapid.pem -pubout -outform DER 2>/dev/null | tail -c 65 | b64url)"
```

Ключи генерируются один раз. Смена ключей делает все существующие подписки
недействительными: на каждом устройстве придётся заново нажать «Подписаться на
уведомления». `vapid.pem` после переноса значений в `hub.env` можно удалить.

### 1.6 Cloudflare Tunnel и Access

Хаб публикуется одним именем, например `hqc.example.com`. Людям (`/`, `/api/*`,
в том числе WebSocket `/api/ws`) — только через Access; воркерам (`/worker/*`) — без Access,
их пускает токен воркера (§6, §20).

**Туннель.** `cloudflared tunnel login`, `cloudflared tunnel create hqc`,
`cloudflared tunnel route dns hqc hqc.example.com`. Конфиг `/etc/cloudflared/config.yml`
(два маршрута на один и тот же хаб; разделение на «с Access» и «без Access» делается
приложениями Access ниже, маршрут `/worker/` выделен явно, чтобы его было видно и чтобы
при желании направить его на отдельный порт):

```yaml
tunnel: <UUID туннеля>
credentials-file: /etc/cloudflared/<UUID туннеля>.json
ingress:
  - hostname: hqc.example.com
    path: ^/worker/
    service: http://127.0.0.1:8080
  - hostname: hqc.example.com
    service: http://127.0.0.1:8080
  - service: http_status:404
```

`sudo cloudflared service install` ставит его службой (systemd или launchd). Хаб в Docker
(§1.8): `service: http://hqc-hub:8080` у cloudflared в той же Docker-сети. WebSocket
туннель пропускает без настроек; долгие соединения держатся heartbeat'ом хаба и
воркера (20 с) и переподключением.

**Access** (Zero Trust → Access → Applications), два self-hosted приложения на тот же
домен:

1. `hqc` — домен `hqc.example.com`, путь пустой (весь сайт). Политика **Allow** для ваших
   email (или группы). Session Duration — длинный (например, 1 месяц): установленное на
   телефон PWA иначе часто будет открываться на странице входа.
2. `hqc-worker` — домен `hqc.example.com`, путь `worker`. Политика **Bypass**, Include:
   Everyone. Более конкретный путь побеждает, поэтому `/worker/*` идёт мимо Access.

**Cookie приложения `hqc`** (Applications → `hqc` → Configure → Settings → Cookie
settings): **SameSite = Lax**, **HttpOnly** включён, **Binding Cookie** выключен: с ней Cloudflare не пропускает WebSocket через Access (проверено 2026-10-23). По
умолчанию cookie `CF_Authorization` уходит с `SameSite=None`, и чужая страница в браузере
того, кто вошёл в hqc, может открыть `wss://hqc.example.com/api/ws` с этой cookie. Хаб
сам отклоняет WebSocket и запросы на запись с чужим `Origin` (SPEC §20), Lax — второй
рубеж. После смены проверить, что установленное на телефон PWA открывается без
повторного входа.

**Rate limiting на `/worker/*`** (Security → WAF → Rate limiting rules, на зону домена):
правило «URI Path starts with `/worker/`», характеристика — IP, например 60 запросов за
10 секунд → Block на 1 минуту; отдельное, строже, для `/worker/enroll` (например, 10 в
минуту). Хаб и сам ограничивает `/worker/*` по `CF-Connecting-IP` (§20 SPEC), правило
Cloudflare отсекает флуд раньше, чем он дойдёт до туннеля и базы. Нормализацию URL
(Rules → Settings → Normalize incoming URLs) оставить включённой: Bypass для `/worker/`
не должен обходиться путями вида `/worker/../api/me`.

**Домен артефактов** (необязательно, SPEC §12): отдельное имя, например
`artifacts.hqc.example.com`, — ещё один `hostname` в `ingress` туннеля на тот же
`http://127.0.0.1:8080` и `cloudflared tunnel route dns hqc artifacts.hqc.example.com`.
Access на это имя **не** ставить (или Bypass, Everyone): доступ даёт подпись ссылки с TTL
10 минут. В хабе — `HQC_ARTIFACT_PUBLIC_BASE=https://artifacts.hqc.example.com` (§1.2):
только с ним HTML-вложения показываются в iframe-песочнице; на этом имени хаб отдаёт лишь
`/artifacts/*`. Без отдельного домена всё работает, ссылки идут на домен хаба за Access, а
HTML только скачивается.

Откуда взять значения для хаба:
- `HQC_ACCESS_AUD` — Applications → `hqc` → Configure (Overview) → **Application Audience
  (AUD) Tag**. Это тег именно приложения `hqc` (не `hqc-worker`).
- `HQC_ACCESS_TEAM_DOMAIN` — Settings → Custom Pages (или Team domain в общих настройках
  Zero Trust): `<team>.cloudflareaccess.com`.

Проверка снаружи: `https://hqc.example.com/` — экран входа Access, после входа — клиент;
`curl -i https://hqc.example.com/worker/enroll -X POST -H 'content-type: application/json'
-d '{}'` — ответ хаба (ошибка разбора тела `4xx`), а не редирект на вход Access.
Запрос к `/api/me` мимо Access (без заголовка `Cf-Access-Jwt-Assertion`) хаб отклоняет
сам. Лимит тела запроса через туннель ~100 МБ (§20).

**HTTP/3 в зоне выключить** (Network → HTTP/3 with QUIC → Off). Проверено 2026-10-23:
Chrome, открыв сайт по HTTP/3, поднимает WebSocket внутри той же QUIC-сессии (extended
CONNECT, RFC 9220), а край Cloudflare такой поток до origin не доносит и сбрасывает без
ответа. Симптомы: в консоли `WebSocket connection to wss://…/api/ws failed:` без кода, в
Network у `ws` пустой Protocol и статус «Finished», в журнале Caddy и хаба запросов к
`/api/ws` нет вовсе, при этом Safari и телефон работают, а воркер по `/worker/ws`
подключён. Воспроизведение из консоли Chrome: `new WebSocket('wss://<хаб>/worker/ws')`
должен дать «Unexpected response code: 401» от хаба; по HTTP/3 он тоже падает без кода,
значит Access ни при чём. Временная проверка на одном компьютере:
`chrome://flags/#enable-quic` → Disabled. После выключения HTTP/3 в зоне Chrome может ещё
несколько минут ходить по QUIC из кэша Alt-Svc.

### 1.7 Раздача клиента

Клиент — статические файлы `client/dist` после `npm run build`. Он обращается к `/api`
относительным путём и маршрутизирует в `#`, поэтому должен жить на том же домене, что и
API, за тем же приложением Access. Проще всего — отдать его самим хабом:
`HQC_CLIENT_DIR=/opt/hqc/client` (скопировать туда содержимое `client/dist`). `index.html`,
`sw.js` и манифест отдаются с `Cache-Control: no-cache`, файлы `assets/*` (имена с хэшем) —
как неизменяемые. В Docker-образе (§1.8) клиент уже лежит в `/opt/hqc/client` и включён.

### 1.8 Хаб в Docker

Вместо §1.3: готовый образ `ghcr.io/drzlo13/hqc-hub` (linux/amd64; Dockerfile в корне
репозитория). На сервере ничего не собирается — только `docker compose pull`. Образ
собирает workflow `.github/workflows/image.yml`, только после зелёного `ci` на том же коммите.

**Теги.**

| тег | когда появляется | для чего |
|---|---|---|
| `sha-<7 знаков коммита>` | каждая сборка | развёртывание: неизменяемый, откат — предыдущий `sha-…` |
| `master` | каждый зелёный push в `master` | плавающий, «последний master»; в боевом compose не использовать |
| `X.Y.Z`, `X.Y`, `latest` | тег `vX.Y.Z` (версия обязана совпасть с `workspace.package.version` в `Cargo.toml`) | релизы; `vX.Y.Z-rc.N` даёт только `X.Y.Z-rc.N` и `sha-…` |

В compose пишите конкретный `sha-…` или `X.Y.Z`. Версию в образе показывает
`docker run --rm ghcr.io/drzlo13/hqc-hub:<тег> --version` (`hqc-hub 0.1.0`), коммит — метка
`org.opencontainers.image.revision` (`docker image inspect`). Пакет публичный: `pull` без
логина. Если пакет закрыт — `docker login ghcr.io -u <github-логин>` с classic PAT
только с `read:packages`.

**Что в образе.** `/usr/local/bin/hqc-hub` (`ENTRYPOINT`, аргументов не нужно), клиент
в `/opt/hqc/client`, пользователь `hqc` (uid/gid 10001), порт 8080, из пакетов —
`ca-certificates` и `libssl3t64`. Заданы по умолчанию (§1.2; переопределяются в env):

| переменная | значение в образе |
|---|---|
| `HQC_HUB_LISTEN` | `0.0.0.0:8080` |
| `HQC_CLIENT_DIR` | `/opt/hqc/client` |
| `RUST_LOG` | `info` |
| `NO_COLOR` | `1` — логи без ANSI-цветов |

Задать нужно: `HQC_DATABASE_URL`, `HQC_ACCESS_TEAM_DOMAIN`, `HQC_ACCESS_AUD`; для push —
`HQC_VAPID_PRIVATE`, `HQC_VAPID_PUBLIC`, `HQC_VAPID_SUBJECT` (§1.5). `HQC_DEV` в
контейнере не работает и не нужен: с `0.0.0.0` хаб в dev-режиме откажется стартовать.

**Состояние.** Хаб хранит всё в Postgres; на диске контейнера он ничего не пишет (корень
образа можно монтировать read-only, `/tmp` не нужен), логи — в stdout. Volume нужен только
базе.

**`0.0.0.0:8080` безопасен, только пока порт не опубликован наружу.** Внутри контейнера
это интерфейс compose-сети, а не хоста; Access (§1.6) защищает только то, что приходит
через туннель. Поэтому у `hqc-hub` нет `ports:` — `cloudflared` или Caddy ходят к нему по
имени `hqc-hub:8080` в той же сети. Если они на хосте, а не в контейнере, публикуйте только
на loopback: `ports: ["127.0.0.1:8080:8080"]`, никогда `8080:8080` (Docker открывает такой
порт на всех интерфейсах в обход ufw/nftables хоста).

`/srv/hqc/compose.yml`:

```yaml
services:
  hqc-db:
    image: postgres:17
    restart: unless-stopped
    env_file: ./db.env
    volumes:
      - hqc-db:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD", "pg_isready", "-U", "hqc", "-d", "hqc"]
      interval: 5s
      timeout: 5s
      retries: 10

  hqc-hub:
    image: ghcr.io/drzlo13/hqc-hub:sha-<коммит>   # или X.Y.Z
    restart: unless-stopped
    depends_on:
      hqc-db:
        condition: service_healthy
    env_file: ./hub.env
    read_only: true
    volumes:
      # Хранилище артефактов (HQC_ARTIFACT_DIR образа); бэкапить вместе с базой.
      - hqc-artifacts:/var/lib/hqc/artifacts
    # без ports: наружу хаб виден только через cloudflared/Caddy в этой же сети
    networks: [default, edge]

volumes:
  hqc-db:
  hqc-artifacts:

networks:
  # Общая сеть с cloudflared/Caddy из их compose-проекта: docker network create edge.
  # Если они описаны в этом же compose.yml — уберите edge и оставьте default.
  edge:
    external: true
```

`/srv/hqc/db.env` и `/srv/hqc/hub.env` — права `0600`, в git не класть; пароль в обоих
один и тот же (`openssl rand -hex 24`; hex — чтобы не экранировать его в URL):

```sh
# db.env
POSTGRES_USER=hqc
POSTGRES_DB=hqc
POSTGRES_PASSWORD=<пароль>
```

```sh
# hub.env
HQC_DATABASE_URL=postgresql://hqc:<пароль>@hqc-db/hqc
HQC_ACCESS_TEAM_DOMAIN=<team>.cloudflareaccess.com
HQC_ACCESS_AUD=<AUD tag>
HQC_VAPID_PRIVATE=<base64url>
HQC_VAPID_PUBLIC=<base64url>
HQC_VAPID_SUBJECT=mailto:<you>@<domain>
```

**Проверьте, что у `hqc-hub` и `hqc-db` в `compose.yml` нет `ports:`** (или только
`127.0.0.1:…`, см. выше). Хаб доверяет `CF-Connecting-IP` и `Cf-Access-Jwt-Assertion`
только потому, что до него можно дойти лишь через туннель: открытый наружу порт даёт
`/worker/*` и статику без Cloudflare, а лимиты по адресу — подделываемыми.

База — в той же compose-сети и без опубликованного порта, поэтому подключение без TLS
(§1.1) здесь допустимо. `POSTGRES_*` действуют только при первом запуске на пустом volume:
сменить пароль потом — `ALTER ROLE hqc PASSWORD …` в базе и правка обоих файлов.

В туннеле (§1.6) вместо `http://127.0.0.1:8080` — `http://hqc-hub:8080` (cloudflared в
контейнере в сети `edge`); Caddy — `reverse_proxy hqc-hub:8080`.

```sh
cd /srv/hqc
docker compose pull && docker compose up -d
docker compose logs -f hqc-hub       # ждать: hqc-hub listening addr=0.0.0.0:8080 protocol=1
docker compose exec hqc-db psql -U hqc -d hqc -c 'select version, description from _sqlx_migrations'
```

`HEALTHCHECK` в образе нет (в нём нет `curl`); `GET /health` → `ok` проверяет прокси или
`curl` с хоста, где виден хаб. Если база недоступна, хаб завершается с ошибкой, и его
перезапускает `restart: unless-stopped`.

**Обновление.** Миграции применяются при старте нового контейнера (§1.1), отката миграций
нет — поэтому сначала дамп:

```sh
cd /srv/hqc
docker compose exec -T hqc-db pg_dump -U hqc -Fc hqc > backup/hqc-$(date +%F-%H%M).dump
# в compose.yml: image: ghcr.io/drzlo13/hqc-hub:<новый тег>
docker compose pull hqc-hub
docker compose up -d hqc-hub
docker compose logs --since 5m hqc-hub
```

Откат: прежний тег в `compose.yml` и `up -d`. Если новая версия успела применить
миграцию, старый хаб на такой базе может не стартовать — тогда восстановить дамп:
`docker compose stop hqc-hub`,
`docker compose exec -T hqc-db pg_restore -U hqc -d hqc --clean --if-exists < backup/<файл>.dump`,
затем `up -d`. Мажорная версия Postgres привязана к volume: `postgres:17` → `18` только
через `pg_dump`/`pg_restore` в новый volume, не сменой тега.

Собрать образ самому: `docker build -t hqc-hub .` в корне репозитория (на машине той же
архитектуры, что сервер, или с `--platform linux/amd64`).

## 2. Воркер на macOS

### 2.1 Установка

```sh
cargo build --release -p hqc-worker
sudo install -m 0755 target/release/hqc-worker /usr/local/bin/
hqc-worker --version
which claude          # путь к Claude Code CLI, понадобится для claude_bin
claude --version      # CLI установлен и залогинен подпиской этого пользователя
```

Воркер запускается от того пользователя macOS, чья подписка Claude Code и чьи проекты
используются. Подкоманды: `enroll <hub_url>`, `run`; `mcp` и `env` в итерации 1 печатают
«not implemented».

Переменные окружения воркера:

| переменная | по умолчанию | что это |
|---|---|---|
| `HQC_HOME` | `~/.hqc` | каталог воркера: `worker.toml`, outbox, токен в файловом режиме |
| `HQC_WORKER_CONFIG` | `$HQC_HOME/worker.toml` | путь к конфигу, если он лежит не в `HQC_HOME` |
| `HQC_TOKEN_STORE` | не задана | `file` — хранить токен в файле `$HQC_HOME/token` (0600) вместо Keychain; на маке нужно только для тестов и прогонов вроде `tools/e2e-local.sh` |
| `CLAUDE_CONFIG_DIR` | `~/.claude` | где Claude Code хранит транскрипты; воркер смотрит туда, чтобы выбрать `--resume` или `--session-id` |
| `RUST_LOG` | `info` | уровень логов; токен в логах не появляется даже на `trace` |

### 2.2 `worker.toml`

`~/.hqc/worker.toml`. Это единственный источник истины для ограничений воркера: хаб их
не может ни задать, ни расширить (§2.1, §14). Неизвестное поле — ошибка запуска (опечатка
не должна молча ослабить ограничения).

```toml
# URL хаба, ровно тот же, что в `hqc-worker enroll` (без завершающего /): по нему токен
# ищется в Keychain. Только https://; http:// — для localhost / 127.0.0.0/8 / [::1].
hub_url = "https://hqc.example.com"

# Разрешить http:// к хабу не на loopback (токен воркера пойдёт открытым текстом). Только
# для отладки в изолированной сети; для `enroll` то же самое — флаг --insecure-http.
# insecure_http = false

# Сколько процессов агентов может работать одновременно; следующий старт хаб получит как
# `agent_start_failed` (retryable) и повторит позже. По умолчанию 8.
# max_sessions = 8

# Корни, за пределы которых сессии не выходят. Абсолютные пути; симлинки раскрываются.
# Непустой список. Workspace (папки сессий) добавляются в веб-клиенте и обязаны лежать
# внутри: воркер проверяет каждый путь сам (fs.stat, session.start), а дерево папок в
# клиенте показывает только то, что внутри этих корней.
allowed_roots = ["/Users/me/work"]

# Секунды простоя, после которых процесс агента завершается (следующий ввод поднимет его
# через --resume). 0 или отсутствие — простаивающий агент не трогается (§17.3).
idle_timeout = 0

# Пускать ли сессии чужих пользователей хаба. В итерации 1 пользователь один — false.
allow_foreign_sessions = false

# Путь к Claude Code CLI. По умолчанию — `claude` из PATH процесса воркера. Под launchd
# PATH короткий (~/.local/bin в него не входит), поэтому лучше указать явно.
claude_bin = "/Users/me/.local/bin/claude"

# Блока [workspace] больше нет (с T14): workspace — объекты хаба, их добавляют в клиенте
# (проект → Workspaces → «Добавить workspace»). Старый блок итерации 1 воркер игнорирует и
# пишет об этом предупреждение при старте — уберите его из файла.

# Политика разрешений Claude Code (SPEC §8.1, §14). Блок можно не писать: тогда mode = "ask",
# allow пуст, deny — набор по умолчанию (sudo, ssh, scp, rm -rf /*, curl | sh, dd, mkfs,
# git push --force / -f).
[permissions]
mode = "ask"
allow = [
  # Edit/Write без скобок — правка любого файла, куда может писать пользователь, не только
  # workspace; уберите их, если хотите подтверждать каждую правку.
  "Read", "Glob", "Grep", "Edit", "Write",
  "Bash(cargo build:*)", "Bash(cargo check:*)", "Bash(cargo test:*)",
  "Bash(cargo clippy:*)", "Bash(cargo fmt:*)", "Bash(cargo run:*)",
  "Bash(git status:*)", "Bash(git diff:*)", "Bash(git log:*)", "Bash(git show:*)",
  "Bash(git add:*)", "Bash(git commit:*)",
  "Bash(ls:*)", "Bash(mkdir:*)", "Bash(touch:*)",
  "mcp__hqc",
]
deny = [
  "Bash(sudo:*)", "Bash(ssh:*)", "Bash(scp:*)", "Bash(rm -rf /*)",
  "Bash(curl * | sh)", "Bash(curl * | bash)", "Bash(dd:*)", "Bash(mkfs*)",
  "Bash(git push:*)", "Bash(cargo publish:*)", "Bash(git reset --hard:*)",
  "WebFetch",
]
```

Три уровня `[permissions]`, от сильного к слабому:

1. **`deny`** — не выполняется и не спрашивается. Его не отменяют ни «Разрешать всегда»,
   ни `mode = "auto"`, ни `auto_approve` хаба; в чате вместо карточки появляется строка
   «Запрещено политикой воркера». Инструмент без скобок (`WebFetch`) пропадает из сессии
   целиком. Свой `deny` **заменяет** набор по умолчанию, поэтому перенесите в него те
   правила по умолчанию, которые нужны; `deny = []` отключает запреты совсем.
2. **`allow`** и правила «Разрешать всегда» (CLI пишет их в
   `<корень git-репозитория>/.claude/settings.local.json`) — выполняется без вопроса.
3. **Остальное** — при `mode = "ask"` приходит карточка разрешения; при `mode = "auto"`
   решает классификатор Claude Code, ничего не спрашивая (проверено: он пропускает и
   `rm -rf` вне папки, и `git push --force`, если пользователь об этом попросил), так что в
   этом режиме граница — только `deny`.

Синтаксис правил: `Bash(cargo test:*)` — команда `cargo test` с любыми аргументами (префикс
до границы слова: `Bash(mkfs*)` не ловит `mkfs.ext4`), `Bash(curl * | sh)` — `*` любые
символы, `Bash(git status)` — ровно эта команда; составные команды (`a && b`, `a | b`) CLI
проверяет по каждой части. `Read`, `Edit`, `WebFetch` — инструмент целиком,
`mcp__hqc` — все инструменты MCP-сервера. Неверное правило — ошибка запуска воркера: CLI
молча выбросил бы весь файл настроек вместе с `deny`. `Bash(rm -rf /*)` ловит любой
абсолютный путь, в том числе внутри workspace — относительные (`rm -rf target`) проходят.

### 2.3 Регистрация (`enroll`)

```sh
hqc-worker enroll https://hqc.example.com
# Enrollment code: K7QM-3XPW
# Confirm it in the hqc web app: https://hqc.example.com/#/enroll?code=K7QM-3XPW
# Waiting...
```

Открыть ссылку (с телефона или ноутбука, войдя через Access) и подтвердить код — либо
в клиенте «Настройки» → «Подтвердить код воркера» и ввести код руками (регистр и дефис
не важны). Перед кнопкой «Подтвердить» клиент показывает имя машины, ОС, время запроса и
страну (по Cloudflare): если это не ваша машина и не только что запущенный `enroll` — не
подтверждайте, ссылку могли прислать. Код живёт 10 минут. После подтверждения воркер печатает
`Enrolled as worker <id>.` и сохраняет токен:

- на macOS — Keychain, generic password, service `hqc-worker`, account = `hub_url`.
  Посмотреть: `security find-generic-password -s hqc-worker -a https://hqc.example.com`;
- с `HQC_TOKEN_STORE=file` (и на других ОС) — `$HQC_HOME/token`, права 0600.

Имя воркера в вебе — имя хоста без домена (`home-mac`, а не `home-mac.tail….ts.net`);
другое — флагом `--name`: `hqc-worker enroll --name home-mac https://hqc.example.com`.

По ssh связка ключей обычно заперта. `enroll` проверяет хранилище пробной записью до
запроса кода и при отказе Keychain сразу выходит с подсказкой: выполнить в той же
ssh-сессии `security unlock-keychain` и повторить, либо задать `HQC_TOKEN_STORE=file`
(тогда и `run` должен идти с ней же). Токен хаб выдаёт один раз, так что до хаба дело
не доходит и ничего не сгорает.

`enroll` не пишет `worker.toml`: если его нет, воркер подскажет путь.

**Перерегистрация той же машины** (SPEC §5.1, с 0.21). Если машину регистрируют заново —
после пересборки бинарника, смены хранилища токена (Keychain → `HQC_TOKEN_STORE=file`),
отзыва токена, — это должен остаться тот же воркер, иначе его сессии, workspace и служебная
сессия навсегда остаются на старом id «офлайн». Порядок:

1. Остановить старый `run` (`launchctl bootout gui/$(id -u)/local.hqc.worker`): воркер,
   который сейчас на связи, заменить нельзя (`409 worker_online`).
2. `hqc-worker enroll <hub_url>` (с `HQC_TOKEN_STORE=file`, если токен должен лежать в
   файле), открыть ссылку.
3. На экране подтверждения под данными машины — список ваших воркеров, которые офлайн или
   отозваны, с числом сессий. Выбрать «Это та же машина, заменить <имя>» и нажать
   «Подтвердить замену». «Новый воркер» (по умолчанию) — новая запись с новым id.
4. `enroll` печатает `Enrolled as worker <id> (replaced <имя>).` — `<id>` тот же, что был.
   Старый токен перестал работать в момент подтверждения; в служебной сессии воркера —
   строка «Воркер перерегистрирован: «старое» → «новое»». Запустить `run` снова.

Имя и ОС берутся у новой регистрации (`--name`, чтобы оставить прежнее). Процессы агентов
старого запуска считаются завершёнными (как при перезапуске воркера): ввод продолжится
через `--resume`. Если по ошибке подтверждён «Новый воркер» — отзовите его, затем
перерегистрируйте машину ещё раз с заменой старого; лишний воркер без сессий удаляется
кнопкой «Удалить» (§2.6).

### 2.4 launchd-агент для `run`

Агент (LaunchAgent), а не демон: воркеру нужен Keychain пользователя (его токен и логин
Claude Code), а он доступен только в сессии пользователя.

`~/Library/LaunchAgents/local.hqc.worker.plist` (пути — абсолютные, `~` launchd
не раскрывает):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>local.hqc.worker</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/hqc-worker</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>/Users/me/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>RUST_LOG</key><string>info</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>/Users/me/Library/Logs/hqc-worker.log</string>
  <key>StandardErrorPath</key><string>/Users/me/Library/Logs/hqc-worker.log</string>
</dict>
</plist>
```

`PATH` нужен и самому `claude` (он запускает `git`, оболочку и т.п.), даже если
`claude_bin` указан.

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.hqc.worker.plist
launchctl print gui/$(id -u)/local.hqc.worker | grep -E 'state|pid'
launchctl kickstart -k gui/$(id -u)/local.hqc.worker   # перезапуск (например, после обновления)
launchctl bootout gui/$(id -u)/local.hqc.worker        # остановка
```

Остановка шлёт SIGTERM: воркер завершает процессы агентов, сообщает хабу `stopped` и до
5 с досылает outbox; неотправленное остаётся на диске и уйдёт при следующем запуске.

Элемент Keychain привязан к подписи бинарника, который его создал. У неподписанной сборки (`cargo build`, ad-hoc подпись линковщика) подпись меняется при каждой пересборке, поэтому после замены `hqc-worker` воркер под launchd упирается в диалог Keychain на экране мака (рядом с ним в `ps` появляется `SecurityAgent`), а лог остаётся пустым. Проверено 2026-10-23 на 0.1.0. Для машины, где воркер пересобирают, используйте файловое хранилище: `HQC_TOKEN_STORE=file` при `enroll` и та же переменная в `EnvironmentVariables` plist; токен лежит в `$HQC_HOME/token` с правами 0600. Keychain имеет смысл только для подписанного релизного бинарника.

### 2.5 Где что лежит, логи

| что | где |
|---|---|
| конфиг | `~/.hqc/worker.toml` |
| токен | Keychain (`hqc-worker` / `hub_url`); в файловом режиме `~/.hqc/token` |
| outbox (события, ещё не подтверждённые хабом) | `~/.hqc/outbox/outbox.sqlite3` (SQLite, WAL); `sqlite3 ~/.hqc/outbox/outbox.sqlite3 'select count(*) from outbox'` — сколько ждёт отправки |
| логи воркера | stderr; под launchd — `~/Library/Logs/hqc-worker.log` (`tail -f`) |
| транскрипты Claude Code | `~/.claude/projects/<путь workspace, где всё кроме букв и цифр заменено на ->/<runtime_session_id>.jsonl` |

Что искать в логе: `connected to hub worker_id=…` — подключение есть; `hub rejected the
worker token` — токен отозван или не тот хаб; `worker update needed` — версия протокола не совпала
(воркер повторяет раз в час и больше ничего не делает); `agent start failed` — не
запускается `claude` (путь, логин).

### 2.6 Отзыв токена

В клиенте: «Настройки» → «Воркеры» → «Отозвать». Хаб помечает токен отозванным, рвёт
соединение и архивирует служебную сессию воркера (в ней — строка «Токен воркера … отозван»);
воркер пропадает из группы «Воркеры» в левой колонке, а в «Настройках» остаётся с пометкой
«отозван» и кнопкой «Удалить». Сам воркер будет переподключаться и писать в лог `hub
rejected the worker token; run hqc-worker enroll again`. На самой машине затем остановите
агент (`launchctl bootout …`) и удалите старый токен:
`security delete-generic-password -s hqc-worker -a https://hqc.example.com`
(в файловом режиме — `rm ~/.hqc/token`). Чтобы вернуть машину с её сессиями — `hqc-worker
enroll <hub_url>` и при подтверждении «Это та же машина, заменить <имя>» (§2.3): отзыв
снимается, id, сессии и workspace остаются.

**Удаление отозванного воркера.** «Настройки» → «Удалить» у отозванного воркера: хаб удаляет
запись воркера, его служебную сессию с историей и его workspace-записи (папки на машине не
трогаются). Пока на воркере есть неархивные сессии, хаб отказывает (`409
worker_has_sessions`), клиент пишет их число: либо перенесите их, перерегистрировав машину с
заменой этого воркера (§2.3), либо архивируйте. Архивные сессии удаление переживают — без
воркера и workspace, только как история (разархивировать их нельзя). Неотозванный воркер
сначала отзовите.

Если украден ноутбук или токен — отзывайте из клиента сразу: хаб хранит только хэш, новый
токен выдаётся только через подтверждение кода под вашим логином Access.

## 3. Клиент

Сборка: `cd client && npm ci && npm run build` → `client/dist`; раздача — §1.7. Для
разработки — `client/README.md` (`npm run dev`, `npm run dev:mock`).

С телефона:

1. Открыть `https://hqc.example.com/`, войти через Access.
2. Установить PWA. Android (Chrome): меню → «Установить приложение». iPhone (Safari):
   «Поделиться» → «На экран „Домой“». Установленное приложение на iOS хранит cookies
   отдельно от Safari — войти через Access придётся ещё раз уже внутри него.
3. Включить уведомления: в приложении «Настройки» → «Подписаться на уведомления» и
   разрешить. Push приходит на `approval.request`, `clarify.request`, финальный ответ агента
   и недоставленный ввод — если эта сессия сейчас не открыта на каком-либо устройстве
   (§16). Нажатие открывает сессию.

Ограничения push:
- **iOS / iPadOS**: Web Push работает только из установленного на экран «Домой» PWA
  (iOS 16.4+), не из вкладки Safari; подписываться нужно изнутри установленного приложения.
- Push требует HTTPS (через туннель — есть); по IP локальной сети не работает.
- Если сессия Access истекла, уведомление всё равно придёт (push идёт от хаба через
  сервис браузера), но по нажатию откроется страница входа.
- Сервис-воркер клиента ничего не кэширует: без связи с хабом клиент не открывается.

## 4. Проверка

Ручной вариант того, что делает `tools/e2e-local.sh` (его можно запустить на любой машине
разработчика: он поднимает хаб в dev-режиме на loopback, отдельную базу `hqc_e2e` и
временный воркер, проходит сценарий одним ходом `claude-haiku-5-5` и всё убирает; это
стоит одного короткого запроса подписки).

1. `https://hqc.example.com/health` (после входа в Access) — `ok`. Клиент открывается,
   в «Настройках» виден ваш email.
2. «Настройки» → «Уведомления»: не «На хабе не настроены ключи VAPID». Подписаться.
3. На маке: `hqc-worker enroll https://hqc.example.com`, подтвердить код по ссылке,
   `launchctl bootstrap …` (§2.4). В «Настройках» → «Воркеры» машина в статусе `online`.
   В левой колонке у проекта (личного или нового, «+ Проект») — «Workspaces» →
   «Добавить workspace»: выбрать воркер и папку внутри `allowed_roots` (по дереву или
   путём). Первый workspace проекта становится основным.
4. Создать сессию в проекте («Новая»: название по желанию, workspace — основной заранее
   выбран, папка внутри него — необязательно), отправить:
   `Run the shell command: touch hqc-e2e && echo hqc-e2e. Then reply with exactly: DONE`.
   Пометка «ждёт доставки» под сообщением пропадает, сессия «работает».
5. Появляется карточка `Bash` и запрос разрешения с кнопками; закрыть приложение на
   телефоне — должен прийти push о запросе. Открыть по нему, нажать «Разрешить».
6. Карточка инструмента получает результат `hqc-e2e`, приходит финальный ответ `DONE`,
   сессия «свободна».
7. На маке: файл `hqc-e2e` появился в папке workspace; транскрипт
   `~/.claude/projects/<закодированный путь workspace>/<uuid>.jsonl` существует.
8. «Стоп» во время следующего хода прерывает его; открытый запрос разрешения при этом
   закрывается как отменённый.

Если что-то не так: лог хаба (`journalctl -u hqc-hub`, в Docker — `docker compose logs hqc-hub`), лог воркера
(`~/Library/Logs/hqc-worker.log`), состояние сессии в клиенте (`ошибка` — нажать
«Повторить» после исправления причины).

## 5. Известные ограничения итерации 1

По §19.1 и заметкам `docs/notes/t2..t6-questions.md`:

- ~~Один проект, один workspace~~ — снято в T14: проекты создаются, переименовываются и
  удаляются в клиенте (личный — только описание), в проекте несколько workspace на любых
  воркерах. Workspace только `dir` (существующая папка); `clone`, `worktree`, `ephemeral` и
  шаблон сессии — итерация 5. Сессия без workspace (§8.4) через клиент не создаётся.
  Удаление workspace убирает только запись; пока в нём есть неархивные сессии, хаб его не
  удаляет.
- **Обновление с итерации 1** (миграция хаба `0005`): workspace из старого `[workspace]
  path` (хаб помнит его из последнего `hello`) переносится в личный проект, сессии
  привязываются к нему; сессии, для которых папка неизвестна, архивируются с системной
  записью «Archived on upgrade…». Блок `[workspace]` в `worker.toml` после обновления
  воркера можно удалить (он игнорируется).
- **Нет MCP hqc** (`hqc-worker mcp` не реализован): агенты не шлют сообщения друг другу,
  нет `ask`/`send`, вложений от агента, поиска, расписаний.
- **Нет зеркала и импорта сессий VS Code**, нет подхвата; сессия, начатая в hqc, видна в
  `~/.claude/projects`, но обратно изменения из VS Code в hqc не попадут.
- **Нет артефактов**: вложения в сообщения не передаются агенту (воркер пишет warn).
- **Окружения**: только `default`; `environments`, `env_bindings`, `ssh_keys`,
  `allow_local_mcp` не реализованы (`hqc-worker env` — «not implemented»). Процесс агента
  получает только `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `LANG`, `LC_*`, `TERM`,
  `TMPDIR`, `SSH_AUTH_SOCK`, `CLAUDE_*`, `ANTHROPIC_*` из окружения воркера; `HQC_*` и
  остальное — нет.
- **Один пользователь**: шаринга и грантов нет, `allow_foreign_sessions` держите `false`.
- **Адаптер Claude Code**: `system_prompt` и `disallowed_tools` передаются
  (`--append-system-prompt`, `--disallowedTools`), `restricted` — `--restricted`;
  `.claude/settings.json` репозитория не применяется (`--setting-sources user,local`), но
  `.claude/settings.local.json` — применяется (туда пишет `always`). `MCP_TOOL_TIMEOUT` не
  передаётся; ограничения числа ходов (`--max-turns`) нет. Команды только для чтения
  (например, `echo`) CLI разрешает сам, без запроса (§8.1). Формат ответа на вопрос с
  множественным выбором (`AskUserQuestion`) не проверен на живой сборке.
- **Состояние хода**: ход, закончившийся без финального текста, при позднем `ack` ввода
  может остаться «работает» до следующего хода (§5.2 **[проверить]**).
- **`session.kill`** очищает очередь воркера; ожидающие вводы досылаются только после
  следующего переподключения воркера (T3, п. 10).
- **Токен воркера**: Keychain только на macOS; на других ОС — файл 0600. Трея и
  `hqc-worker update` нет: о несовместимой версии сообщает только лог.
- **Хаб**: Postgres без TLS (база на том же хосте); лимиты `/worker/*` — в памяти процесса
  хаба, по адресу из `CF-Connecting-IP` (enroll 10/мин и 10 живых кодов на адрес, 1000
  всего; poll 60/мин; 20 неверных токенов `/worker/ws` в минуту; 16 проверок токена
  одновременно), после перезапуска счётчики обнуляются; мастер-ключа и `api_keys` нет.
- **Клиент**: проверен тестами и сборкой; ручной прогон на телефоне через Access и
  настоящий push на iOS/Android в рамках T6 не выполнялись (нет устройства в автономном
  прогоне) — это первое, что нужно сделать после установки (§4). История сессии
  загружается с начала целиком (без подгрузки хвоста), офлайн-режима нет.
