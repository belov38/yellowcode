# Yellowcode

Командная строка для конфигураций, файловых баз и серверов 1С: для разработчиков, скриптов и ИИ-агентов. Читает `.cf`, `.cfe`, `.dt`, `1Cv8.1CD` и резервные копии MS SQL Server без платформы 1С, выполняет запросы к файловой базе, работает с сервером 1С как тонкий клиент. macOS, Windows и Linux. Есть и приложение с окном для просмотра. Ранняя версия.

Чтение файлы не меняет. Команды, которые меняют данные, перечислены в `yellowcode --help`; `YELLOWCODE_READONLY=1` их запрещает.

Скачать — [Releases](https://github.com/belov38/yellowcode/releases). Документация — [yellowcode.dev](https://yellowcode.dev/).

## Совместимость

Проверенные версии БП, УНФ, ЗУП, ERP, Документооборота и БСП, результаты открытия и известные проблемы — [матрица совместимости](https://yellowcode.dev/compatibility/).

## Установка

**Только командная строка через npm или PyPI** (Linux x86_64 и ARM64 с glibc 2.36+, macOS Apple Silicon, Windows x64):

```sh
npx @belov.ai/yellowcode --help    # Node.js 18+; установить: npm install -g @belov.ai/yellowcode
uvx yellowcode --help              # или: pipx install yellowcode, pip install yellowcode
```

**Linux из репозитория** (x86_64 и ARM64) — обновления приходят вместе с системными. Пакет `yellowcode-cli` — только командная строка (для серверов и CI), `yellowcode` — командная строка и окно. Debian и Ubuntu:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://yellowcode.dev/apt/yellowcode.gpg -o /etc/apt/keyrings/yellowcode.gpg
sudo curl -fsSL https://yellowcode.dev/apt/yellowcode.sources -o /etc/apt/sources.list.d/yellowcode.sources
sudo apt update && sudo apt install yellowcode-cli
```

Fedora:

```sh
sudo curl -fsSL https://yellowcode.dev/rpm/yellowcode.repo -o /etc/yum.repos.d/yellowcode.repo
sudo dnf install yellowcode-cli
```

**Windows через Scoop:** `scoop bucket add yellowcode https://github.com/belov38/scoop-yellowcode`, затем `scoop install yellowcode`. Scoop добавляет `yellowcode` в `PATH`; окно — ярлык «Yellowcode» в меню «Пуск».

**macOS:**

```sh
brew install --cask belov38/yellowcode/yellowcode
```

или `.dmg` со страницы выпуска. Обновление: `brew update && brew upgrade --cask belov38/yellowcode/yellowcode`.

**Windows:** установщик `Yellowcode-<версия>-windows-x64-setup.exe` или без установки — `Yellowcode-<версия>-windows-x64-portable.zip` (распакуйте; консоль — `yellowcode.exe`, окно — `yellowcode-desktop.exe`).

**Linux** x86_64 (Debian 12, Ubuntu 24.04 и новее) и ARM64, в том числе Raspberry Pi 4/5 с 64-битной Raspberry Pi OS (Bookworm): пакет `.deb` или `.rpm`, либо `.AppImage`.

**Где лежит `yellowcode` в пакетах с окном.** Рядом с `yellowcode-desktop` лежит консольная программа `yellowcode` ([описание команд](https://yellowcode.dev/cli/)). Открыть источник в окне — `yellowcode open <файл>`: команда не ждёт окна и ошибок его запуска не показывает; с `--smoke` ждёт и возвращает код окна.

- macOS — `/Applications/Yellowcode.app/Contents/MacOS/yellowcode`. Ни DMG, ни Homebrew не добавляют его в `PATH`; можно сделать ссылку, например `ln -s /Applications/Yellowcode.app/Contents/MacOS/yellowcode /opt/homebrew/bin/yellowcode`. Через ссылку `--platform-build` с версией не по умолчанию может не найти ресурсы платформы из пакета; для неё вызывайте программу по настоящему пути.
- Windows — `%LOCALAPPDATA%\Yellowcode\yellowcode.exe` после установщика или `yellowcode.exe` в распакованном ZIP. Установщик не меняет `PATH` (Scoop меняет): вызывайте по полному пути (`& "$env:LOCALAPPDATA\Yellowcode\yellowcode.exe" --help` в PowerShell, `"%LOCALAPPDATA%\Yellowcode\yellowcode.exe" --help` в cmd) или добавьте этот каталог в `PATH` сами.
- Linux `.deb` и `.rpm` — `/usr/bin/yellowcode` и `/usr/bin/yellowcode-desktop`, оба уже в `PATH`.
- `.AppImage` открывает окно; консольные команды — после слова `cli`: `./Yellowcode-<версия>-linux-amd64.AppImage cli --help`. Через ссылку с именем `yellowcode` образ работает как консоль и без `cli`: `ln -s "$PWD/Yellowcode-<версия>-linux-amd64.AppImage" ~/.local/bin/yellowcode`. Без FUSE (`--appimage-extract-and-run`) временный каталог образа удаляется при выходе консоли, поэтому вместо `cli open <файл>` используйте `cli open <файл> --smoke` или запускайте окно напрямую (образ без `cli`).

## Как начать

Источник — файл `.cf`, `.dt`, файловая база (`1Cv8.1CD` или каталог базы); для расширений — `.cfe`.

```sh
# Справочники и документы конфигурации
yellowcode config metadata tree configuration.cf --kind Справочник,Документ --depth 2 --format text

# Процедуры модуля и текст одной из них
yellowcode config module outline configuration.cf ОбщийМодуль.Пользователи
yellowcode config module show configuration.cf Справочник.Пользователи.МодульОбъекта --procedure ПередЗаписью

# Где используется регистр, с контекстом
yellowcode config grep -F РегистрНакопления.Продажи configuration.cf -C 2

# Модули и макеты подсистемы в каталог, как в выгрузке конфигуратора
yellowcode config export configuration.cf --subsystem СтандартныеПодсистемы.Пользователи --output users-src

# Запрос к файловой базе
yellowcode ib query base --text 'ВЫБРАТЬ ПЕРВЫЕ 10 Код, Наименование ИЗ Справочник.Валюты' --format json

# Сводка сервера 1С при входе без пользователя
yellowcode remote info http://server/base
```

Справка по любой команде — `yellowcode <команда> --help`, в конце `yellowcode --help` — коды выхода и переменные окружения.
