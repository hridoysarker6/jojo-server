# JOJO Server

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform: Windows x64](https://img.shields.io/badge/platform-Windows%20x64-lightgrey)

**A free, open-source local web server for Windows.** Run Nginx or Apache with PHP, MySQL, PostgreSQL, Redis and Mailpit on your own PC, and create Laravel, WordPress and Symfony sites in one click. A Laragon and XAMPP alternative for PHP developers, built with Tauri v2 (Rust) and React.

**Download:** installers are published on the [Releases page](https://github.com/hridoysarker6/jojo-server/releases). Website: https://hridoysarker6.github.io/jojo-server/. The installer is not code-signed yet, so Windows SmartScreen may say "Unknown publisher": choose **More info, then Run anyway**.

> JOJO Server is an independent project. Laragon, XAMPP and other product names are mentioned only to say which kind of tool this is.

## Why JOJO Server

- **One window for the whole stack.** Start and stop the web server, PHP, databases, cache and mail catcher, and switch versions with a click.
- **Real project tools.** `.test` domains, local HTTPS, wildcard sub-domains, per-site PHP version, Composer, Node.js, a log viewer and background workers for Laravel queues.
- **Free and open.** MIT licensed. Nothing is bundled: programs are downloaded from their official sources when you ask for them.

**Services:** Nginx **or** Apache (one runs at a time, same port; Apache reads `.htaccess`), PHP (php-cgi), MySQL, PostgreSQL, Redis, Mailpit
**Tools:** Composer, Node.js (npm), DbGate, Adminer

## Features

- One-click recommended setup, several versions of every service, the newest versions found automatically on the official sites
- Per-site PHP version, per-site Terminal (that site's PHP, Node.js and Composer), PHP settings and extension manager
- **New project** wizard: Laravel (MySQL + mail + migrations + npm), WordPress, Symfony (not covered by an automated test yet), blank PHP
- Automatic `.test` virtual hosts, local HTTPS (mkcert), custom domain suffix
- **Wildcard domains**: a built-in DNS server, so `anything.test` and `tenant.shop.test` work without editing the hosts file
- **Mail**: Mailpit catches everything; PHP `mail()` and the Laravel `.env` settings are ready-made; "Send test email"
- **Log viewer**: PHP errors, Nginx, and every project's `laravel.log`
- **Background workers** per project: queue worker, scheduler, Vite dev server, any command
- **Public share link** for a site (free Cloudflare quick tunnel)
- Redis browser, DbGate (modern database manager), Adminer
- Automatic updates of JOJO Server itself, tray icon, start with Windows, installer that asks where to keep your files (suggests drive D:)

## What is tested

`src-tauri/tests/e2e.rs` runs the real programs (20 tests, about 12 minutes, downloads a few hundred MB the first time). It covers Nginx + PHP + MySQL, Apache (switching, `.htaccess`, HTTPS, sub-domains), per-site PHP, HTTPS, Adminer, PHP extensions and settings, Redis, Mailpit and logs, Composer and Node in project terminals, workers, the DNS server over UDP, new Laravel and WordPress projects, the public share link, DbGate, folder moves, leftover-process handling and version discovery.

```bash
cd src-tauri
cargo test --test e2e -- --test-threads=1
```

Not covered by an automated test (needs a person at the PC): the installer's folder dialog, the Windows prompts for mkcert and wildcard domains (one-time administrator steps), and the automatic-update download itself.

## Build requirements (one time)

Node.js LTS, Rust (https://rustup.rs), Visual Studio Build Tools with "Desktop development with C++", WebView2.

## Run and build

```bash
npm install
npm run tauri dev
```

## Making a release

Updates are signed. The signing key is created **outside** the project, in `%USERPROFILE%\.jojo-signing` (`updater.key` and `updater.pass`). **Back this folder up.** Without it, copies that are already installed can never update again. The matching public key is in `src-tauri/tauri.conf.json`.

```powershell
.\scripts\build-release.ps1 -Version 1.1.1 -Notes "What is new" `
    -DownloadBase "https://github.com/hridoysarker6/jojo-server/releases/download/v1.1.1" `
    -UpdateUrl    "https://github.com/hridoysarker6/jojo-server/releases/latest/download/latest.json"
```

The script builds the installer, signs it for updates and writes `release\<version>\` with:

- `JOJO-Server_<version>_x64-setup.exe` (the installer)
- `JOJO-Server_<version>_x64-setup.exe.sig` (update signature)
- `latest.json` (what installed copies read to find the new version)

Upload all three to the place `-UpdateUrl` points at (for GitHub: a release whose files are these three). `-UpdateUrl` is built into the app, so use the same address for every release. Installed copies check a few seconds after start-up and on the Settings tab ("Check for updates"); nothing is installed without a click.

### Releases from GitHub

`.github/workflows/build.yml` builds the installer when you push a tag such as `v1.2.1` and attaches it to a GitHub Release, using `.github/RELEASE_NOTES.md` as the release text (update that file before tagging). It needs no setup. If you also add the repository secrets `TAURI_SIGNING_PRIVATE_KEY` (the text of `updater.key`) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (the text of `updater.pass`), the release also gets the update signature and `latest.json` for the in-app updater. `.github/workflows/ci.yml` checks that the code builds on every push.

### Removing the Windows "unknown publisher" warning

That needs a code-signing certificate (separate from the update key). With one installed on this PC, add `-CertThumbprint <thumbprint>` to the command above. Without it the installer works, but Windows SmartScreen shows "Unknown publisher" on other PCs (More info, then Run anyway).

## Folder layout

The data folder is chosen in the installer (default suggestion: `D:\JojoServer`, or `%USERPROFILE%\JojoServer` when there is no D: drive). Set `JOJO_ROOT` to override it.

```
bin/<service>/<version>/   programs (downloaded by the app)
bin/tools/                 Composer, DbGate, mkcert, cloudflared
data/                      database files
www/                       your projects: www/shop becomes shop.test
etc/php/<ver>/php.ini      generated PHP configuration
etc/ssl/                   local certificate authority, site certificates, CA bundle for PHP
run/nginx/                 generated nginx.conf and Nginx logs
logs/php-error.log         PHP error log
catalog.json               extra download entries (merged with the built-in and discovered lists)
settings.json              active versions, ports, per-site settings, workers
```

## Known limitations

- Redis has no official Windows build; the catalog uses the community port (Redis 5.x). Valkey or Memurai ZIPs work through "Install from a URL".
- MariaDB is not supported yet. Windows x64 only.
- If port 80 is used by another program (IIS, Skype, XAMPP), change the HTTP port in Settings.
- Apache comes from Apache Lounge (the Apache project ships no Windows build) and needs the Microsoft Visual C++ 2015-2026 runtime; the app says so if it is missing.
- Wildcard domains use port 53 on 127.0.0.1; if Docker, WSL or Internet Connection Sharing holds it, the app says so and the hosts-file method keeps working.

## What JOJO Server changes on your PC

Nothing is installed system-wide. Everything lives in one folder you choose (see "Folder layout"). The few things that reach outside it:

| What | When | Needs administrator rights |
|---|---|---|
| Adds `*.test` lines to the Windows `hosts` file (between `# JOJO-BEGIN` and `# JOJO-END`) | **Sync hosts** button | Yes, one Windows prompt |
| Adds a DNS rule for your domain suffix so every `*.test` name works | **Wildcard domains: Turn on** | Yes, one Windows prompt (removed again with **Turn off**) |
| Trusts a local certificate authority (mkcert) so `https://shop.test` works | **Enable HTTPS** | No, but Windows asks you to confirm |
| Registers the app to start with Windows | **Settings: Start JOJO Server when Windows starts** | No |
| Opens a public link to one site through a Cloudflare quick tunnel | **Share** button on a site; stays open until you stop it | No |

The administrator steps run small PowerShell scripts that are written to `<data folder>\run\` first, so you can read them.

## Third-party programs

JOJO Server does not bundle other software. When you ask for it, it downloads the official builds and checks the checksum where the download site publishes one:

| Program | Downloaded from |
|---|---|
| PHP, with its PECL extensions | windows.php.net, downloads.php.net |
| Nginx | nginx.org |
| Apache | Apache Lounge (apachelounge.com), since the Apache project ships no Windows build |
| MySQL | cdn.mysql.com |
| PostgreSQL | EnterpriseDB |
| Redis (community Windows port), Mailpit, DbGate, cloudflared, mkcert | their GitHub releases |
| Node.js | nodejs.org |
| Composer | getcomposer.org |
| Adminer | GitHub releases |

Each of these has its own license, which applies to that program and not to JOJO Server.

## Contributing

Bug reports, ideas and pull requests are welcome. Please open an issue first for larger changes. Before sending code, run the front-end build and the tests described under "What is tested". JOJO Server is an independent project and is not affiliated with Laragon or with any other product or brand.

## License

[MIT](LICENSE) © 2026 Hridoy Sarker