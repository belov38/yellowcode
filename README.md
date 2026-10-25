# Yellowcode

Просмотрщик конфигураций и файловых баз 1С для macOS, Windows и Linux. Только просмотр. Ранняя версия.

Скачать — [Releases](https://github.com/belov38/yellowcode/releases).

## Совместимость

Проверенные версии БП, УНФ, ЗУП, ERP, Документооборота и БСП, результаты открытия и известные проблемы — [матрица совместимости](COMPATIBILITY.md).

## Установка

**macOS:**

```sh
brew install --cask belov38/yellowcode/yellowcode
```

или `.dmg` со страницы выпуска. Обновление: `brew update && brew upgrade --cask belov38/yellowcode/yellowcode`.

**Windows:** установщик `Yellowcode-<версия>-windows-x64-setup.exe` или без установки — `Yellowcode-<версия>-windows-x64-portable.zip` (распакуйте и запустите `yellowcode.exe`).

**Linux** (x86_64; Debian 12, Ubuntu 24.04 и новее): пакет `.deb` или `.rpm`, либо `.AppImage`.

## Как начать

Откройте `.cf`, `.dt` или файловую базу (`1Cv8.1CD` или папку базы).

- **Просмотр данных:** правый клик по справочнику, документу или регистру → «Открыть данные».
- **Консоль запросов:** «Сервис» → «Консоль запросов».
