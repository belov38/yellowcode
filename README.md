# Yellowcode

Просмотрщик конфигураций и файловых баз 1С для macOS, Windows и Linux. Конфигурации и базы открываются только для чтения; данные меняют только `yellowcode ib users create|edit|delete|clear-password` (файл базы), `bak convert --server` (временная база на SQL Server) и пишущие команды `yellowcode remote` (сервер 1С), см. `YELLOWCODE_READONLY`. Ранняя версия.

Скачать — [Releases](https://github.com/belov38/yellowcode/releases).

## Совместимость

Проверенные версии БП, УНФ, ЗУП, ERP, Документооборота и БСП, результаты открытия и известные проблемы — [матрица совместимости](https://yellowcode.dev/compatibility/).

## Установка

**macOS:**

```sh
brew install --cask belov38/yellowcode/yellowcode
```

или `.dmg` со страницы выпуска. Обновление: `brew update && brew upgrade --cask belov38/yellowcode/yellowcode`.

**Windows:** установщик `Yellowcode-<версия>-windows-x64-setup.exe` или без установки — `Yellowcode-<версия>-windows-x64-portable.zip` (распакуйте и запустите `yellowcode-desktop.exe`; консоль — `yellowcode.exe` в том же каталоге).

**Linux** (x86_64; Debian 12, Ubuntu 24.04 и новее): пакет `.deb` или `.rpm`, либо `.AppImage`.

**Linux ARM64**, в том числе Raspberry Pi 4/5 с 64-битной Raspberry Pi OS (Bookworm): пакет `.deb` или `.rpm`, либо `.AppImage`.

**Командная строка.** В каждом пакете рядом с окном приложения `yellowcode-desktop` лежит консольная программа `yellowcode` — команды для терминала, скриптов и ИИ-агентов (`yellowcode --help`, [описание команд](https://yellowcode.dev/cli/)). Окна она сама не открывает; открыть источник в окне — `yellowcode open <файл>`: команда сразу возвращается и ошибок запуска окна не показывает, их видно с `--smoke` (ждёт окно и возвращает его код).

- macOS — `/Applications/Yellowcode.app/Contents/MacOS/yellowcode`. Ни DMG, ни Homebrew не добавляют его в `PATH`; можно сделать ссылку, например `ln -s /Applications/Yellowcode.app/Contents/MacOS/yellowcode /opt/homebrew/bin/yellowcode`. Через ссылку `--platform-build` с версией не по умолчанию может не найти ресурсы платформы из пакета; для неё вызывайте программу по настоящему пути.
- Windows — `%LOCALAPPDATA%\Yellowcode\yellowcode.exe` после установщика или `yellowcode.exe` в распакованном ZIP. Установщик не меняет `PATH`: вызывайте по полному пути (`& "$env:LOCALAPPDATA\Yellowcode\yellowcode.exe" --help` в PowerShell, `"%LOCALAPPDATA%\Yellowcode\yellowcode.exe" --help` в cmd) или добавьте этот каталог в `PATH` сами.
- Linux `.deb` и `.rpm` — `/usr/bin/yellowcode` и `/usr/bin/yellowcode-desktop`, оба уже в `PATH`.
- `.AppImage` открывает окно; консольные команды — после слова `cli`: `./Yellowcode-<версия>-linux-amd64.AppImage cli --help`. Без слова `cli` образ работает как консоль, если запущен через ссылку с именем `yellowcode`: `ln -s "$PWD/Yellowcode-<версия>-linux-amd64.AppImage" ~/.local/bin/yellowcode`. Без FUSE (`--appimage-extract-and-run`) образ распаковывается во временный каталог, который удаляется при выходе консоли, поэтому вместо `cli open <файл>` используйте `cli open <файл> --smoke` (ждёт окно) или запускайте окно напрямую (образ без `cli`).

`YELLOWCODE_READONLY=1` запрещает `yellowcode` команды, которые меняют данные (пользователи базы, запись объектов и вызовы процедур на сервере 1С, выбор варианта отчёта, временная база на SQL Server): они завершаются с кодом 4 (код 3 у `remote create`, `write` и `form` значит другое: запрос отправлен, итог неизвестен). Это защита от ошибок, а не граница безопасности.

## Как начать

Откройте `.cf`, `.dt` или файловую базу (`1Cv8.1CD` или папку базы).

- **Просмотр данных:** правый клик по справочнику, документу или регистру → «Открыть данные».
- **Консоль запросов:** «Сервис» → «Консоль запросов».
