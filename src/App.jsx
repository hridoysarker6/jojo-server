import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";

const META = {
  nginx: { ic: "NX", c: "#14b87a", desc: "Web server" },
  apache: { ic: "AP", c: "#d22128", desc: "Web server with .htaccess" },
  php: { ic: "PHP", c: "#7a86cb", desc: "Default PHP (sites can override)" },
  mysql: { ic: "MY", c: "#e48a1d", desc: "Database" },
  postgres: { ic: "PG", c: "#3a7ac2", desc: "Database" },
  redis: { ic: "RD", c: "#e0402f", desc: "Cache and queues" },
  mailpit: { ic: "ML", c: "#9a5cf0", desc: "Mail catcher" },
};
const ORDER = ["nginx", "apache", "php", "mysql", "postgres", "redis", "mailpit"];
const NAMES = { nginx: "Nginx", apache: "Apache", php: "PHP", mysql: "MySQL", postgres: "PostgreSQL", redis: "Redis", mailpit: "Mailpit", node: "Node.js" };

const fmtUp = (s) => {
  if (!s) return "--";
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = s % 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${String(x).padStart(2, "0")}s` : `${x}s`;
};
const mb = (n) => (n / 1048576).toFixed(1);
const errText = (e) => (typeof e === "string" ? e : e?.message || JSON.stringify(e));

function Icon({ d }) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" dangerouslySetInnerHTML={{ __html: d }} />
  );
}
const ICONS = {
  dash: '<path d="M3 12l9-8 9 8M5 10v10h14V10"/>',
  sites: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18"/>',
  ver: '<path d="M12 3v12m0 0l-4-4m4 4l4-4M4 20h16"/>',
  redis: '<ellipse cx="12" cy="6" rx="8" ry="3"/><path d="M4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/>',
  logs: '<path d="M4 5h16M4 10h16M4 15h10M4 20h7"/>',
  set: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
};

export default function App() {
  const [tab, setTab] = useState("dash");
  const [ov, setOv] = useState(null);
  const [catalog, setCatalog] = useState([]);
  const [checkedAt, setCheckedAt] = useState(null);
  const [update, setUpdate] = useState(null);
  const [updProg, setUpdProg] = useState(null);
  const [appVersion, setAppVersion] = useState("");
  useEffect(() => { getVersion().then(setAppVersion).catch(() => {}); }, []);
  const [logs, setLogs] = useState([]);
  const [progress, setProgress] = useState({});
  const [lastProgress, setLastProgress] = useState(null);
  const [busy, setBusy] = useState({});
  const [toastMsg, setToastMsg] = useState("");
  const [theme, setTheme] = useState(() => {
    try { return localStorage.getItem("jojo-theme") || "dark"; } catch { return "dark"; }
  });
  const toastTimer = useRef();

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try { localStorage.setItem("jojo-theme", theme); } catch {}
  }, [theme]);

  const toast = useCallback((m) => {
    setToastMsg(m);
    clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToastMsg(""), 4000);
  }, []);

  const pushLog = useCallback((svc, level, line) => {
    const t = new Date().toTimeString().slice(0, 8);
    setLogs((l) => [...l.slice(-299), { t, svc, level, line }]);
  }, []);

  const refresh = useCallback(async () => {
    try { setOv(await invoke("get_overview")); } catch (e) { pushLog("app", "err", errText(e)); }
  }, [pushLog]);

  useEffect(() => {
    refresh();
    const loadCatalog = () =>
      invoke("get_catalog").then((c) => { setCatalog(c.entries); setCheckedAt(c.checked_at); }).catch(() => {});
    loadCatalog();
    const iv = setInterval(refresh, 2000);
    const subs = [
      listen("catalog", loadCatalog),
      listen("update_available", (e) => setUpdate(e.payload)),
      listen("update_progress", (e) => setUpdProg(e.payload)),
      listen("log", (e) => pushLog(e.payload.svc, e.payload.level, e.payload.line)),
      listen("progress", (e) => {
        const p = e.payload;
        setProgress((m) => ({ ...m, [`${p.service}@${p.version}`]: p }));
        setLastProgress(p);
        if (p.stage === "done") refresh();
      }),
    ];
    return () => { clearInterval(iv); subs.forEach((s) => s.then((un) => un())); };
  }, [refresh, pushLog]);

  const act = useCallback(async (key, cmd, args, okMsg) => {
    setBusy((b) => ({ ...b, [key]: true }));
    try {
      const r = await invoke(cmd, args);
      if (okMsg) toast(typeof r === "string" && r ? r : okMsg);
    } catch (e) {
      const m = errText(e);
      toast(m);
      pushLog(args?.id || "app", "err", m);
    } finally {
      setBusy((b) => ({ ...b, [key]: false }));
      refresh();
    }
  }, [refresh, toast, pushLog]);

  const services = useMemo(() => {
    const m = Object.fromEntries((ov?.services || []).map((s) => [s.id, s]));
    return ORDER.map((id) => m[id]).filter(Boolean);
  }, [ov]);
  // services plus tools that have versions but never run (Node.js): for the Versions and Settings tabs
  const withTools = useMemo(() => [...services, ...(ov?.services || []).filter((s) => s.id === "node")], [services, ov]);
  const running = services.filter((s) => s.running).length;
  const nginxPort = services.find((s) => s.id === "nginx")?.port ?? 80;
  const siteUrl = (site) => {
    if (site.ssl && ov?.ssl_ready) return `https://${site.domain}${ov.https_port === 443 ? "" : ":" + ov.https_port}`;
    return `http://${site.domain}${nginxPort === 80 ? "" : ":" + nginxPort}`;
  };

  return (
    <div className="app">
      <aside className="rail">
        <div className="brand">
          <div className="logo">JJ</div>
          <div><b>JOJO Server</b><small>{appVersion ? `v${appVersion}` : ""} · local</small></div>
        </div>
        <nav className="nav">
          {[["dash", "Dashboard"], ["sites", "Sites"], ["redis", "Redis"], ["logs", "Logs"], ["ver", "Versions"], ["set", "Settings"]].map(([k, n]) => (
            <button key={k} aria-current={tab === k ? "page" : undefined} onClick={() => setTab(k)}>
              <Icon d={ICONS[k]} />{n}
            </button>
          ))}
        </nav>
        <div className="rail-foot">
          <button className="btn small" onClick={() => act("term", "open_terminal", {})}>⌨ Terminal</button>
          <button className="btn small" onClick={() => act("www", "open_path", { kind: "www" })}>📁 Open www folder</button>
          <div className="sysbox">
            Data folder<b title={ov?.root}>{ov?.root || "…"}</b>
          </div>
        </div>
      </aside>

      <main>
        {tab === "dash" && (
          <Dashboard
            services={services} running={running} ov={ov} logs={logs} busy={busy} act={act}
            setTab={setTab} siteUrl={siteUrl} setLogs={setLogs} lastProgress={lastProgress}
            update={update} updProg={updProg}
          />
        )}
        {tab === "sites" && <Sites ov={ov} services={services} act={act} busy={busy} siteUrl={siteUrl} logs={logs} toast={toast} />}
        {tab === "logs" && <LogsTab sites={ov?.sites || []} toast={toast} />}
        {tab === "set" && <SettingsTab catalog={catalog} ov={ov} services={withTools} act={act} busy={busy} toast={toast} refresh={refresh} update={update} setUpdate={setUpdate} updProg={updProg} lastProgress={lastProgress} appVersion={appVersion} />}
        {tab === "redis" && <RedisBrowser service={services.find((s) => s.id === "redis")} act={act} busy={busy} setTab={setTab} toast={toast} />}
        {tab === "ver" && (
          <Versions services={withTools} catalog={catalog} checkedAt={checkedAt} progress={progress} busy={busy} act={act} />
        )}
      </main>

      <button className="theme" aria-label="Switch theme" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
        {theme === "dark" ? "☀" : "☾"}
      </button>
      <div className={"toast" + (toastMsg ? " show" : "")} role="status">{toastMsg}</div>
    </div>
  );
}

function UpdateBanner({ update, updProg, busy, act }) {
  if (!update) return null;
  const pct = updProg?.total ? Math.min(100, (updProg.received / updProg.total) * 100) : null;
  return (
    <div className="banner setup">
      <div>
        <b>JOJO Server {update.version} is available.</b> You have {update.current}.
        {update.notes && <div className="notes">{update.notes}</div>}
        {busy.update && (
          <div className="prog wide"><div className="bar"><i style={{ width: pct == null ? "30%" : `${pct}%` }} /></div>
            <small>{pct == null ? "Downloading…" : pct >= 100 ? "Installing, the app will restart…" : `Downloading ${pct.toFixed(0)}%`}</small></div>
        )}
      </div>
      <button className="btn primary" disabled={busy.update} onClick={() => act("update", "install_update", {})}>
        {busy.update ? "Updating…" : "Update and restart"}
      </button>
    </div>
  );
}

function Dashboard({ services, running, ov, logs, busy, act, setTab, siteUrl, setLogs, lastProgress, update, updProg }) {
  const logRef = useRef();
  useEffect(() => { if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight; }, [logs]);
  const has = (id) => !!services.find((s) => s.id === id)?.versions.length;
  const needsSetup = ov && (!(has("nginx") || has("apache")) || !has("php") || !has("mysql"));
  const installing = busy.rec;
  return (
    <>
      <UpdateBanner update={update} updProg={updProg} busy={busy} act={act} />
      <header className="top">
        <div>
          <h1>{running ? <>{running} {running === 1 ? "service" : "services"} <span>running</span></> : <>Everything is <span>ready</span></>}</h1>
          <p>{running ? "Your sites are live. Add a folder to www and it gets its own local domain." : "Turn on the services you need, or click Start All."}</p>
        </div>
        <div className="actions">
          <button className="btn primary" disabled={busy.all} onClick={() => act("all", "start_all", {}, "All services started")}>▶ Start All</button>
          <button className="btn" disabled={busy.all || !running} title={running ? "Stop and start every running service again" : "Nothing is running"}
            onClick={() => act("all", "restart_all", {}, "Running services restarted")}>↻ Restart All</button>
          <button className="btn stop" disabled={busy.all} onClick={() => act("all", "stop_all", {}, "All services stopped")}>■ Stop All</button>
        </div>
      </header>

      {needsSetup && (
        <div className="banner setup">
          <div>
            <b>Get started in one click.</b> Install the recommended stack: Nginx, PHP 8.3, MySQL 8.4, Node.js, Composer, Mailpit and Adminer. Downloads about 350 MB.
            {installing && lastProgress && (
              <div className="prog wide">
                <div className="bar"><i style={{ width: lastProgress.total ? `${(lastProgress.received / lastProgress.total) * 100}%` : "30%" }} /></div>
                <small>
                  {NAMES[lastProgress.service] || lastProgress.service} {lastProgress.version} ·{" "}
                  {lastProgress.stage === "extract" ? "extracting…" : `${mb(lastProgress.received)}${lastProgress.total ? " / " + mb(lastProgress.total) : ""} MB`}
                </small>
              </div>
            )}
          </div>
          <button className="btn primary" disabled={installing} onClick={() => act("rec", "install_recommended", {}, "Recommended stack installed")}>
            {installing ? "Installing…" : "Install recommended stack"}
          </button>
        </div>
      )}

      <section className="stats">
        <div className="stat"><small>Running</small><strong>{running}<em>/ {services.length}</em></strong></div>
        <div className="stat"><small>Sites</small><strong>{ov?.sites?.length ?? 0}<em>.{ov?.tld || "test"}</em></strong></div>
        <div className="stat"><small>Installed</small><strong>{services.reduce((a, s) => a + s.versions.length, 0)}<em>versions</em></strong></div>
      </section>

      <section>
        <div className="sh"><h2>Services</h2><span>Switch a service on, or change its version and port</span></div>
        <div className="grid">
          {services.map((s) => <Tile key={s.id} s={s} busy={busy} act={act} setTab={setTab} />)}
        </div>
      </section>

      <section className="two">
        <div className="card">
          <div className="sh"><h2>Sites</h2><button className="link" onClick={() => setTab("sites")}>Manage →</button></div>
          {ov?.sites?.length ? (
            <ul className="sitelist">
              {ov.sites.slice(0, 6).map((x) => (
                <li key={x.name}>
                  <a href="#" onClick={(e) => { e.preventDefault(); act("url", "open_url", { url: siteUrl(x) }); }}>{x.domain}</a>
                  <small>{x.path}</small>
                </li>
              ))}
            </ul>
          ) : <p className="empty">No sites yet. Create one in the Sites tab.</p>}
        </div>
        <div className="card">
          <div className="sh"><h2>Live log</h2><button className="link" onClick={() => setLogs([])}>Clear</button></div>
          <div className="console" ref={logRef} role="log">
            {logs.length === 0 && <div className="t">Service output appears here.</div>}
            {logs.map((l, i) => (
              <div key={i}><span className="t">{l.t}</span> <span className="svc">{l.svc}</span> <span className={l.level}>{l.line}</span></div>
            ))}
          </div>
        </div>
      </section>
    </>
  );
}

function Tile({ s, busy, act, setTab }) {
  const m = META[s.id];
  const [port, setPort] = useState(String(s.port));
  useEffect(() => setPort(String(s.port)), [s.port]);
  const installed = s.versions.length > 0;
  const b = busy[s.id];
  const commitPort = () => {
    const n = parseInt(port, 10);
    if (!n || n === s.port || n < 1 || n > 65535) { setPort(String(s.port)); return; }
    act(s.id, "set_port", { id: s.id, port: n }, `${NAMES[s.id]} now uses port ${n}`);
  };
  return (
    <article className={"tile" + (s.running ? " on" : "")}>
      <div className="thead">
        <div className="ico" style={{ background: m.c }}>{m.ic}</div>
        <div className="tname"><b>{s.name}</b><small>{m.desc}</small></div>
        <button
          className="sw" role="switch" aria-checked={s.running} aria-label={`Turn ${s.name} ${s.running ? "off" : "on"}`}
          disabled={!installed || b}
          onClick={() => act(s.id, s.running ? "stop_service" : "start_service", { id: s.id })}
        />
      </div>
      {installed ? (
        <div className="meta">
          <label>Version
            <select value={s.active || ""} disabled={b}
              onChange={(e) => act(s.id, "set_active_version", { id: s.id, version: e.target.value }, `${s.name} ${e.target.value} is now active`)}>
              {s.versions.map((v) => <option key={v}>{v}</option>)}
            </select>
          </label>
          <label>{s.id === "php" ? "Base port" : "Port"}
            <input className="pill-in" value={port} inputMode="numeric"
              onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))}
              onBlur={commitPort} onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
          </label>
        </div>
      ) : (
        <div className="notinst">
          <p>Not installed</p>
          <button className="btn small" onClick={() => setTab("ver")}>Install →</button>
        </div>
      )}
      <div className="tfoot">
        <span className="state"><i className="dot" />{b ? "Working…" : s.running ? "Running" : "Stopped"}</span>
        <span>
          {s.running && s.id === "mailpit" && <button className="link" onClick={() => act("url", "open_url", { url: `http://127.0.0.1:${s.port}` })}>Open inbox ↗ </button>}
          {s.running && <button className="link restart" disabled={b} title={`Restart ${s.name}`} onClick={() => act(s.id, "restart_service", { id: s.id }, `${s.name} restarted`)}>↻ Restart</button>}
          {s.running ? ` up ${fmtUp(s.uptime)}` : "--"}
        </span>
      </div>
    </article>
  );
}

const PROJECT_KINDS = [
  { id: "blank", label: "Blank PHP", hint: "An empty folder with a starter index.php." },
  { id: "laravel", label: "Laravel", hint: "Installed with Composer. Mail and the database are set up for you." },
  { id: "wordpress", label: "WordPress", hint: "Downloaded from wordpress.org with a database ready for the install screen." },
  { id: "symfony", label: "Symfony", hint: "The Symfony skeleton, installed with Composer." },
];
const WORKER_PRESETS = [
  { label: "Laravel queue worker", name: "queue", cmd: "php artisan queue:work --tries=3" },
  { label: "Laravel scheduler", name: "scheduler", cmd: "php artisan schedule:work" },
  { label: "Vite dev server", name: "vite", cmd: "npm run dev" },
  { label: "Custom command", name: "", cmd: "" },
];

function WildcardCard({ ov, act, busy, suffix, toast }) {
  const d = ov?.dns;
  const test = async () => {
    try {
      const ok = await invoke("check_wildcard_dns");
      toast(ok ? `Windows resolves anything.${suffix} to this PC. It works.` : `Windows did not resolve a test name under .${suffix}. Try turning wildcard domains off and on again.`);
    } catch (e) { toast(errText(e)); }
  };
  const stale = d?.enabled && d.applied_tld !== suffix;
  const live = d?.enabled && !stale && d.running;
  return (
    <div className="card">
      <div className="sh"><h2>Wildcard domains</h2><span>{live ? "On" : stale ? "Needs applying again" : d?.enabled ? "Starting…" : "Off"}</span></div>
      {live ? (
        <>
          <p className="empty">Any name ending in <code>.{suffix}</code> opens your sites, and sub-domains like <code>tenant.shop.{suffix}</code> work too. The hosts file is not used, so new sites need no administrator prompt.</p>
          <div className="row wrap">
            <button className="btn" onClick={test}>Test it</button>
            <button className="btn stop" disabled={busy.dns} onClick={() => act("dns", "disable_wildcard_dns", {}, "Wildcard domains are off")}>Turn off</button>
          </div>
        </>
      ) : (
        <div className="row wrap">
          <p className="empty grow">
            {stale
              ? <>You changed the suffix to <code>.{suffix}</code>. Apply it again so Windows asks JOJO Server about it (one administrator prompt).</>
              : <>Normally every new site needs a hosts-file change. Turn this on once (one administrator prompt) and every <code>name.{suffix}</code>, including sub-domains, works by itself.</>}
          </p>
          <button className="btn primary" disabled={busy.dns} onClick={() => act("dns", "enable_wildcard_dns", {}, "Wildcard domains are on")}>
            {busy.dns ? "Working…" : stale ? "Apply again" : "Turn on"}
          </button>
        </div>
      )}
    </div>
  );
}

function WorkersCard({ ov, act, busy }) {
  const sites = ov?.sites || [];
  const workers = ov?.workers || [];
  const [site, setSite] = useState("");
  const [preset, setPreset] = useState(0);
  const [wname, setWname] = useState(WORKER_PRESETS[0].name);
  const [cmd, setCmd] = useState(WORKER_PRESETS[0].cmd);
  useEffect(() => { if (!site && sites.length) setSite(sites[0].name); }, [sites, site]);
  const pick = (i) => { setPreset(i); setWname(WORKER_PRESETS[i].name); setCmd(WORKER_PRESETS[i].cmd); };
  return (
    <div className="card">
      <div className="sh"><h2>Background workers</h2><span>Queue workers, schedulers, dev servers</span></div>
      {!workers.length && <p className="empty">None yet. Add one below, for example the Laravel queue worker of a project.</p>}
      <div className="wlist">
        {workers.map((w) => {
          const k = `w-${w.site}-${w.name}`;
          return (
            <div className="wrow" key={k}>
              <i className={"dot" + (w.running ? " live" : "")} />
              <div className="winfo"><b>{w.site}</b> <span className="mono">/ {w.name}</span><small>{w.command}</small></div>
              <label className="chk small"><input type="checkbox" checked={w.autostart} onChange={(e) => act(k, "set_worker_autostart", { site: w.site, name: w.name, on: e.target.checked })} /> Start with the app</label>
              {w.running
                ? <button className="btn small stop" disabled={busy[k]} onClick={() => act(k, "stop_worker", { site: w.site, name: w.name })}>■ Stop</button>
                : <button className="btn small" disabled={busy[k]} onClick={() => act(k, "start_worker", { site: w.site, name: w.name }, `${w.name} started`)}>▶ Start</button>}
              <button className="link" disabled={busy[k]} onClick={() => act(k, "remove_worker", { site: w.site, name: w.name }, "Worker removed")}>Remove</button>
            </div>
          );
        })}
      </div>
      <h3 className="sub">Add a worker</h3>
      <div className="row wrap">
        <select aria-label="Site" value={site} onChange={(e) => setSite(e.target.value)} className="field narrow">
          {sites.map((s) => <option key={s.name} value={s.name}>{s.domain}</option>)}
        </select>
        <select aria-label="Kind of worker" value={preset} onChange={(e) => pick(Number(e.target.value))} className="field narrow">
          {WORKER_PRESETS.map((p, i) => <option key={p.label} value={i}>{p.label}</option>)}
        </select>
        <input className="field narrow" aria-label="Worker name" placeholder="Name" value={wname} onChange={(e) => setWname(e.target.value)} />
        <input className="field grow mono" aria-label="Command" placeholder="Command, for example php artisan queue:work" value={cmd} onChange={(e) => setCmd(e.target.value)} />
        <button className="btn primary" disabled={!site || !wname.trim() || !cmd.trim() || busy.addworker}
          onClick={async () => { await act("addworker", "add_worker", { site, name: wname.trim(), command: cmd.trim() }, "Worker added"); }}>Add</button>
      </div>
      <p className="tip">Workers run in the project folder with that project's PHP version and your Node.js. Their output appears in the Live log on the Dashboard.</p>
    </div>
  );
}

function Sites({ ov, services, act, busy, siteUrl, logs, toast }) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState("blank");
  const [withDb, setWithDb] = useState(true);
  const [withNpm, setWithNpm] = useState(false);
  const [tld, setTld] = useState("test");
  useEffect(() => { if (ov?.tld) setTld(ov.tld); }, [ov?.tld]);
  const suffix = ov?.tld || "test";
  const phpVersions = services.find((s) => s.id === "php")?.versions || [];
  const defaultPhp = services.find((s) => s.id === "php")?.active;
  const create = async () => {
    if (!name.trim()) return;
    if (kind === "blank") {
      await act("site", "create_site", { name }, `Site '${name}' created`);
    } else {
      await act("site", "create_project", { project: { name, kind, database: withDb, npm: withNpm } }, `Project '${name}' is ready`);
    }
    setName("");
  };
  const projectLog = (logs || []).filter((l) => l.svc === "project").slice(-14);
  const share = (x) => act(`share-${x.name}`, "share_site", { site: x.name }, "The site is public now");
  const unshare = (x) => act(`share-${x.name}`, "unshare_site", { site: x.name }, "Sharing stopped");
  const setCfg = (site, php, ssl) =>
    act(`cfg-${site.name}`, "set_site_config", { name: site.name, php, ssl }, `${site.domain} updated`);

  return (
    <>
      <header className="top">
        <div>
          <h1>Your <span>sites</span></h1>
          <p>Every folder inside <code>www</code> gets its own <code>folder.{suffix}</code> domain. For Laravel, the <code>public</code> folder is detected automatically.</p>
        </div>
        <div className="actions"><button className="btn" onClick={() => act("www", "open_path", { kind: "www" })}>📁 Open www folder</button></div>
      </header>

      {ov && !ov.hosts_ok && (
        <div className="banner">
          <div><b>The hosts file needs an update.</b> Windows must map your .{suffix} domains to this computer before the browser can open them. You will see one administrator prompt (UAC).</div>
          <button className="btn primary" disabled={busy.hosts} onClick={() => act("hosts", "sync_hosts", {}, "Hosts file updated")}>Sync hosts</button>
        </div>
      )}

      <div className="two even">
        <div className="card">
          <div className="sh"><h2>New site</h2><span>{PROJECT_KINDS.find((k) => k.id === kind)?.hint}</span></div>
          <div className="seg" role="radiogroup" aria-label="Project type">
            {PROJECT_KINDS.map((k) => (
              <button key={k.id} role="radio" aria-checked={kind === k.id} className={kind === k.id ? "on" : ""} disabled={busy.site} onClick={() => setKind(k.id)}>{k.label}</button>
            ))}
          </div>
          <div className="row" style={{ marginTop: 10 }}>
            <input className="field" placeholder="Site name, for example: shop" value={name} disabled={busy.site}
              onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && create()} />
            <button className="btn primary" disabled={busy.site || !name.trim()} onClick={create}>{busy.site ? "Working…" : "Create"}</button>
          </div>
          {(kind === "laravel" || kind === "wordpress") && (
            <div className="row wrap" style={{ marginTop: 8 }}>
              <label className="chk"><input type="checkbox" checked={withDb} disabled={busy.site} onChange={(e) => setWithDb(e.target.checked)} /> Create a MySQL database</label>
              {kind === "laravel" && <label className="chk"><input type="checkbox" checked={withNpm} disabled={busy.site} onChange={(e) => setWithNpm(e.target.checked)} /> Run npm install</label>}
            </div>
          )}
          {busy.site && projectLog.length > 0 && (
            <div className="console minilog">{projectLog.map((l, i) => <div key={i} className={l.level}>{l.line}</div>)}</div>
          )}
        </div>

        <div className="card">
          <div className="sh"><h2>HTTPS</h2><span>{ov?.ssl_ready ? "Ready" : "Not set up"}</span></div>
          {ov?.ssl_ready ? (
            <p className="empty">Local HTTPS is ready. Turn it on for each site in the table below.</p>
          ) : (
            <div className="row">
              <p className="empty grow">Creates a trusted local certificate authority (mkcert). Windows will ask you to confirm once.</p>
              <button className="btn primary" disabled={busy.ssl} onClick={() => act("ssl", "setup_ssl", {}, "HTTPS is ready")}>
                {busy.ssl ? "Working…" : "Enable HTTPS"}
              </button>
            </div>
          )}
        </div>
      </div>

      <WildcardCard ov={ov} act={act} busy={busy} suffix={suffix} toast={toast} />

      <div className="card">
        <div className="sh"><h2>Domain suffix</h2><span>Currently .{suffix}</span></div>
        <div className="row wrap">
          <span className="dotlabel">.</span>
          <input className="field narrow" aria-label="Domain suffix" value={tld} maxLength={15}
            onChange={(e) => setTld(e.target.value.toLowerCase().replace(/[^a-z0-9]/g, ""))}
            onKeyDown={(e) => e.key === "Enter" && tld && tld !== suffix && act("tld", "set_tld", { tld }, `Suffix changed to .${tld}. Click Sync hosts to apply it.`)} />
          <button className="btn" disabled={busy.tld || !tld || tld === suffix}
            onClick={() => act("tld", "set_tld", { tld }, `Suffix changed to .${tld}. Click Sync hosts to apply it.`)}>Change suffix</button>
          <p className="empty grow">Good choices: <code>test</code>, <code>localhost</code>, <code>loc</code>. Real internet suffixes such as .com, .dev, .app and .local are blocked because they would break real websites or mDNS.</p>
        </div>
      </div>

      <div className="card">
        <div className="sh"><h2>Sites</h2><span>{ov?.sites?.length ?? 0} total</span></div>
        <div className="tw">
          <table>
            <thead><tr><th>Domain</th><th>PHP version</th><th>HTTPS</th><th>Document root</th><th></th></tr></thead>
            <tbody>
              {(ov?.sites || []).map((x) => (
                <tr key={x.name}>
                  <td>
                    <a href="#" onClick={(e) => { e.preventDefault(); act("url", "open_url", { url: siteUrl(x) }); }}>{x.domain}</a>
                    <small>{x.path}</small>
                  </td>
                  <td>
                    <select aria-label={`PHP version for ${x.domain}`} value={x.php || ""} disabled={!phpVersions.length || busy[`cfg-${x.name}`]}
                      onChange={(e) => setCfg(x, e.target.value, x.ssl)}>
                      <option value="">Default{defaultPhp ? ` (${defaultPhp})` : ""}</option>
                      {phpVersions.map((v) => <option key={v} value={v}>{v}</option>)}
                    </select>
                  </td>
                  <td>
                    <button className="sw" role="switch" aria-checked={x.ssl} aria-label={`HTTPS for ${x.domain}`}
                      disabled={!ov?.ssl_ready || busy[`cfg-${x.name}`]} title={ov?.ssl_ready ? "" : "Enable HTTPS first"}
                      onClick={() => setCfg(x, x.php || "", !x.ssl)} />
                  </td>
                  <td><small>{x.doc_root}</small></td>
                  <td className="right">
                    <div className="rowbtns">
                      <button className="btn small" title={`Open a terminal in ${x.path} with PHP ${x.php || defaultPhp || ""}`}
                        onClick={() => act(`term-${x.name}`, "open_terminal", { site: x.name })}>⌨ Terminal</button>
                      {ov?.shares?.[x.name]
                        ? <button className="btn small stop" disabled={busy[`share-${x.name}`]} onClick={() => unshare(x)}>Stop sharing</button>
                        : <button className="btn small" disabled={busy[`share-${x.name}`]} title="Get a public link anyone can open" onClick={() => share(x)}>
                            {busy[`share-${x.name}`] ? "Connecting…" : "🌐 Share"}</button>}
                    </div>
                    {ov?.shares?.[x.name] && (
                      <div className="sharebox">
                        <a href="#" onClick={(e) => { e.preventDefault(); act("url", "open_url", { url: ov.shares[x.name] }); }}>{ov.shares[x.name]}</a>
                        <button className="link" onClick={() => navigator.clipboard?.writeText(ov.shares[x.name]).then(() => toast("Link copied")).catch(() => toast("Select the link and press Ctrl+C"))}>Copy</button>
                      </div>
                    )}
                  </td>
                </tr>
              ))}
              {!ov?.sites?.length && <tr><td colSpan="5" className="empty">No sites yet.</td></tr>}
            </tbody>
          </table>
        </div>
        <p className="tip">Changing a site's PHP version starts that version automatically and reloads Nginx. The Terminal button opens a window in the project folder where <code>php</code> is that site's version. <b>Share</b> gives a temporary public link (through Cloudflare); anyone who has it can open the site until you stop sharing or close JOJO Server.</p>
      </div>

      <WorkersCard ov={ov} act={act} busy={busy} />

      <div className="card">
        <div className="sh"><h2>Database manager</h2><span>Adminer · MySQL, PostgreSQL, SQLite</span></div>
        {ov?.adminer ? (
          <div className="row wrap">
            <button className="btn primary" onClick={() => act("url", "open_url", { url: siteUrl({ domain: `adminer.${suffix}`, ssl: false }) })}>Open Adminer</button>
            <p className="empty grow">Use server <code>127.0.0.1:3306</code> (MySQL) or <code>127.0.0.1:5432</code> (PostgreSQL) with user <code>root</code> or <code>postgres</code> and an empty password. Use the port shown on the service tile if you changed it.</p>
          </div>
        ) : (
          <div className="row wrap">
            <button className="btn" disabled={busy.adminer} onClick={() => act("adminer", "install_adminer", {}, "Adminer installed")}>Install Adminer</button>
            <p className="empty grow">A single-file database manager. It is installed as the site <code>adminer.{suffix}</code>.</p>
          </div>
        )}
      </div>
    </>
  );
}

function LogsTab({ sites, toast }) {
  const [sources, setSources] = useState([]);
  const [src, setSrc] = useState("php");
  const [data, setData] = useState(null);
  const [limit, setLimit] = useState(300);
  const [auto, setAuto] = useState(true);
  const [q, setQ] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const [error, setError] = useState("");
  const box = useRef();
  const stick = useRef(true);

  useEffect(() => {
    invoke("get_log_sources").then((s) => { setSources(s); if (!s.some((x) => x.id === src)) setSrc("php"); }).catch(() => {});
  }, [sites]); // eslint-disable-line

  const load = useCallback(async () => {
    try { setData(await invoke("read_log", { source: src, lines: limit })); setError(""); }
    catch (e) { setData(null); setError(errText(e)); }
  }, [src, limit]);
  useEffect(() => { load(); }, [load]);
  useEffect(() => {
    if (!auto) return undefined;
    const t = setInterval(load, 2500);
    return () => clearInterval(t);
  }, [auto, load]);
  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [data]);

  const lines = (data?.lines || []).filter((l) => !q || l.toLowerCase().includes(q.toLowerCase()));
  const cls = (l) => (/\b(ERROR|CRITICAL|EMERGENCY|ALERT)\b|Fatal|Exception|\[(error|emerg|crit)\]/i.test(l) ? "err" : /\b(WARNING|WARN)\b|Warning:|Deprecated|Notice|\[warn\]/i.test(l) ? "warn" : "");
  const kb = (n) => (n > 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);

  return (
    <>
      <header className="top">
        <div><h1>Log <span>viewer</span></h1><p>PHP errors, Nginx and your Laravel logs in one place. New lines appear by themselves.</p></div>
      </header>
      <div className="card">
        <div className="row wrap logbar">
          <select aria-label="Log" value={src} onChange={(e) => { setSrc(e.target.value); setConfirmClear(false); stick.current = true; }}>
            {sources.map((s) => <option key={s.id} value={s.id}>{s.label}</option>)}
          </select>
          <input className="field grow" aria-label="Filter lines" placeholder="Filter, for example: SQLSTATE" value={q} onChange={(e) => setQ(e.target.value)} />
          <select aria-label="Lines to show" value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
            {[100, 300, 1000, 3000].map((n) => <option key={n} value={n}>Last {n}</option>)}
          </select>
          <label className="chk"><input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} /> Live</label>
          <button className="btn small" onClick={load}>↻ Refresh</button>
          {confirmClear ? (
            <>
              <button className="btn stop small" onClick={async () => { try { await invoke("clear_log", { source: src }); setConfirmClear(false); toast("Log cleared"); load(); } catch (e) { toast(errText(e)); } }}>Yes, clear it</button>
              <button className="btn small" onClick={() => setConfirmClear(false)}>Cancel</button>
            </>
          ) : <button className="btn stop small" disabled={!data?.exists} onClick={() => setConfirmClear(true)}>Clear</button>}
        </div>
        {error && <p className="empty pad">{error}</p>}
        {data && (
          <p className="tip" style={{ margin: "8px 0" }}>
            <code>{data.path}</code> · {data.exists ? `${kb(data.size)}, showing ${lines.length} line${lines.length === 1 ? "" : "s"}` : "no file yet"}
          </p>
        )}
        <div className="console logbox" ref={box} role="log"
          onScroll={(e) => { const el = e.currentTarget; stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40; }}>
          {data && !data.exists && <div className="t">Nothing has been logged here yet.</div>}
          {data?.exists && !lines.length && <div className="t">{q ? "No line matches the filter." : "The log is empty."}</div>}
          {lines.map((l, i) => <div key={i} className={cls(l)}>{l}</div>)}
        </div>
      </div>
    </>
  );
}

const ZONES = ["UTC", "Asia/Dhaka", "Asia/Kolkata", "Asia/Dubai", "Asia/Singapore", "Asia/Tokyo", "Europe/London", "Europe/Berlin", "Europe/Paris", "America/New_York", "America/Chicago", "America/Los_Angeles", "Australia/Sydney"];

function Switch({ on, onClick, label, disabled }) {
  return <button className="sw" role="switch" aria-checked={on} aria-label={label} disabled={disabled} onClick={onClick} />;
}

const MAIL_ENV = (port) => `MAIL_MAILER=smtp\nMAIL_HOST=127.0.0.1\nMAIL_PORT=${port}\nMAIL_USERNAME=null\nMAIL_PASSWORD=null\nMAIL_ENCRYPTION=null\nMAIL_FROM_ADDRESS="dev@jojo.test"\nMAIL_FROM_NAME="\${APP_NAME}"`;

function UpdatesCard({ ov, act, busy, toast, update, setUpdate, updProg, appVersion }) {
  const [configured, setConfigured] = useState(true);
  const [checking, setChecking] = useState(false);
  const [note, setNote] = useState("");
  useEffect(() => { invoke("app_update_configured").then(setConfigured).catch(() => {}); }, []);
  const pct = updProg?.total ? Math.min(100, (updProg.received / updProg.total) * 100) : null;
  const check = async () => {
    setChecking(true); setNote("");
    try {
      const u = await invoke("check_update");
      setUpdate(u || null);
      setNote(u ? "" : "You have the newest version.");
    } catch (e) { setNote(errText(e)); }
    finally { setChecking(false); }
  };
  const g = ov?.general;
  return (
    <div className="card">
      <div className="sh"><h2>JOJO Server updates</h2><span>You have version {appVersion || "…"}</span></div>
      {!configured ? (
        <p className="empty">Updates are not set up in this build yet, so the app cannot look for new versions by itself. Install the newest installer by hand for now.</p>
      ) : (
        <>
          <div className="row wrap">
            <button className="btn" disabled={checking || busy.update} onClick={check}>{checking ? "Checking…" : "Check for updates"}</button>
            {update && <button className="btn primary" disabled={busy.update} onClick={() => act("update", "install_update", {})}>{busy.update ? "Updating…" : `Install ${update.version} and restart`}</button>}
            {note && <span className="tip" style={{ margin: 0 }}>{note}</span>}
          </div>
          {update && <p className="tip">Version {update.version} is available.{update.notes ? ` ${update.notes}` : ""} Running services are stopped cleanly, then the app restarts.</p>}
          {busy.update && <div className="prog wide"><div className="bar"><i style={{ width: pct == null ? "30%" : `${pct}%` }} /></div><small>{pct == null ? "Downloading…" : `${pct.toFixed(0)}%`}</small></div>}
        </>
      )}
      {g && (
        <div className="opt" style={{ marginTop: 10 }}>
          <div><b>Look for updates when JOJO Server starts</b><small>A quick check a few seconds after start-up. Nothing is installed without your click.</small></div>
          <Switch on={g.check_updates} label="Check for updates at start-up" disabled={busy.general} onClick={() => act("general", "set_general", { general: { ...g, check_updates: !g.check_updates } })} />
        </div>
      )}
    </div>
  );
}

function WebServerCard({ ov, services, catalog, act, busy }) {
  const nginx = services.find((s) => s.id === "nginx");
  const apache = services.find((s) => s.id === "apache");
  const active = ov?.web_server || "nginx";
  const apacheEntry = (catalog || []).find((c) => c.service === "apache");
  return (
    <div className="card">
      <div className="sh"><h2>Web server</h2><span>Only one runs at a time, on the same port</span></div>
      <div className="row wrap">
        <label className="fld">Used by Start All
          <select value={active} disabled={busy.web} onChange={(e) => act("web", "set_web_server", { id: e.target.value }, `${NAMES[e.target.value]} is now your web server`)}>
            <option value="nginx" disabled={!nginx?.versions?.length}>Nginx{nginx?.versions?.length ? "" : " (not installed)"}</option>
            <option value="apache" disabled={!apache?.versions?.length}>Apache{apache?.versions?.length ? "" : " (not installed)"}</option>
          </select>
        </label>
        {!apache?.versions?.length && apacheEntry && (
          <button className="btn" disabled={busy[`i-apache@${apacheEntry.version}`]}
            onClick={() => act(`i-apache@${apacheEntry.version}`, "install_version", { id: "apache", version: apacheEntry.version, url: apacheEntry.url }, `Apache ${apacheEntry.version} installed`)}>
            {busy[`i-apache@${apacheEntry.version}`] ? "Installing…" : `Install Apache ${apacheEntry.version}`}
          </button>
        )}
        <p className="empty grow">
          <b>Nginx</b> is lighter and quicker. <b>Apache</b> reads <code>.htaccess</code> files, which many older PHP projects and WordPress plugins rely on.
          Turning one on stops the other. Your sites, HTTPS and PHP versions work with both. Apache needs the Microsoft Visual C++ 2015-2026 runtime (most PCs have it).
        </p>
      </div>
    </div>
  );
}

function SettingsTab({ catalog, ov, services, act, busy, toast, refresh, update, setUpdate, updProg, lastProgress, appVersion }) {
  const php = services.find((s) => s.id === "php");
  const [ver, setVer] = useState("");
  const [exts, setExts] = useState([]);
  const [extCatalog, setExtCatalog] = useState([]);
  const [conf, setConf] = useState(null);
  const [dirty, setDirty] = useState(false);
  const [ports, setPorts] = useState({ nginx: "", https: "", smtp: "" });
  const [wwwNew, setWwwNew] = useState("");
  const [moveEx, setMoveEx] = useState(true);
  const nginx = services.find((s) => s.id === "nginx");
  const mailpit = services.find((s) => s.id === "mailpit");
  const node = services.find((s) => s.id === "node");

  useEffect(() => { invoke("get_extension_catalog").then(setExtCatalog).catch(() => {}); }, []);
  useEffect(() => { if (!ver && php?.versions?.length) setVer(php.active || php.versions[0]); }, [php, ver]);
  useEffect(() => { if (ov?.php_conf && !dirty) setConf(ov.php_conf); }, [ov, dirty]);
  useEffect(() => {
    if (nginx && ov) setPorts({ nginx: String(nginx.port), https: String(ov.https_port), smtp: String(ov.smtp_port) });
  }, [nginx?.port, ov?.https_port, ov?.smtp_port]); // eslint-disable-line

  const loadExts = useCallback(async () => {
    if (!ver) { setExts([]); return; }
    try { setExts(await invoke("get_php_extensions", { version: ver })); } catch { setExts([]); }
  }, [ver]);
  useEffect(() => { loadExts(); }, [loadExts]);

  const change = (k, v) => { setConf((c) => ({ ...c, [k]: v })); setDirty(true); };
  const save = async () => {
    await act("conf", "set_php_conf", { conf: { ...conf, max_execution_time: Number(conf.max_execution_time) || 0, max_input_vars: Number(conf.max_input_vars) || 1000 } }, "PHP settings saved");
    setDirty(false);
  };
  const toggleExt = async (name, enabled) => { await act(`x-${name}`, "set_php_extension", { version: ver, name, enabled }, `${name} turned ${enabled ? "on" : "off"}`); loadExts(); };
  const addExt = async (name) => { await act(`x-${name}`, "install_php_extension", { version: ver, name }, `${name} installed`); loadExts(); };
  const rmExt = async (name) => { await act(`x-${name}`, "remove_php_extension", { version: ver, name }, `${name} removed`); loadExts(); };

  const g = ov?.general;
  const setGeneral = (patch) => act("general", "set_general", { general: { ...g, ...patch } });
  const commitPort = (id) => {
    const n = parseInt(ports[id], 10);
    const cur = id === "nginx" ? nginx?.port : id === "https" ? ov?.https_port : ov?.smtp_port;
    if (!n || n < 1 || n > 65535 || n === cur) { setPorts((p) => ({ ...p, [id]: String(cur) })); return; }
    const label = { nginx: "HTTP", https: "HTTPS", smtp: "Mail (SMTP)" }[id];
    act(`p-${id}`, "set_port", { id, port: n }, `${label} port is now ${n}`);
  };
  const installedNames = new Set(exts.map((e) => e.name));
  const bundled = exts.filter((e) => !e.external);
  const added = exts.filter((e) => e.external);

  return (
    <>
      <header className="top">
        <div><h1><span>Settings</span></h1><p>PHP configuration, extensions and how JOJO Server behaves.</p></div>
      </header>

      <div className="card">
        <div className="sh"><h2>PHP</h2><span>{php?.running ? "Saving restarts PHP automatically" : "PHP is stopped"}</span></div>
        {!php?.versions?.length ? <p className="empty">PHP is not installed yet. Install it from the Versions tab.</p> : (
          <>
            <div className="row wrap">
              <label className="fld">Default PHP version
                <select value={php.active || ""} onChange={(e) => act("php", "set_active_version", { id: "php", version: e.target.value }, `PHP ${e.target.value} is now the default`)}>
                  {php.versions.map((v) => <option key={v}>{v}</option>)}
                </select>
              </label>
              <p className="empty grow">Used by sites that do not choose their own PHP version, and by the Terminal button. You can set a version per site in the Sites tab.</p>
            </div>
            {conf && (
              <div className="confgrid">
                <label className="fld">Memory limit<input className="field" value={conf.memory_limit} onChange={(e) => change("memory_limit", e.target.value)} placeholder="512M" /></label>
                <label className="fld">Max upload file size<input className="field" value={conf.upload_max_filesize} onChange={(e) => change("upload_max_filesize", e.target.value)} placeholder="128M" /></label>
                <label className="fld">Max POST size<input className="field" value={conf.post_max_size} onChange={(e) => change("post_max_size", e.target.value)} placeholder="128M" /></label>
                <label className="fld">Max execution time (seconds)<input className="field" inputMode="numeric" value={conf.max_execution_time} onChange={(e) => change("max_execution_time", e.target.value.replace(/\D/g, ""))} /></label>
                <label className="fld">Max input variables<input className="field" inputMode="numeric" value={conf.max_input_vars} onChange={(e) => change("max_input_vars", e.target.value.replace(/\D/g, ""))} /></label>
                <label className="fld">Timezone<input className="field" list="zones" value={conf.timezone} onChange={(e) => change("timezone", e.target.value)} /><datalist id="zones">{ZONES.map((z) => <option key={z} value={z} />)}</datalist></label>
                <label className="fld wide">Error reporting<input className="field mono" value={conf.error_reporting} onChange={(e) => change("error_reporting", e.target.value)} placeholder="E_ALL" /></label>
                <div className="fld"><span>Show errors on the page</span><Switch on={conf.display_errors} label="Show errors on the page" onClick={() => change("display_errors", !conf.display_errors)} /></div>
                <div className="fld"><span>OPcache (faster, but caches files)</span><Switch on={conf.opcache} label="OPcache" onClick={() => change("opcache", !conf.opcache)} /></div>
                <label className="fld full">Extra php.ini lines
                  <textarea className="field mono" rows={3} value={conf.extra} onChange={(e) => change("extra", e.target.value)} placeholder={"; one directive per line, for example\nsession.gc_maxlifetime = 7200"} spellCheck={false} />
                </label>
              </div>
            )}
            <div className="row" style={{ marginTop: 12 }}>
              <button className="btn primary" disabled={!dirty || busy.conf} onClick={save}>{busy.conf ? "Saving…" : "Save PHP settings"}</button>
              {dirty && <button className="btn" onClick={() => { setConf(ov.php_conf); setDirty(false); }}>Discard changes</button>}
              <span className="tip" style={{ margin: 0 }}>These apply to every installed PHP version.</span>
            </div>
          </>
        )}
      </div>

      {!!php?.versions?.length && (
        <div className="card">
          <div className="sh">
            <h2>PHP extensions</h2>
            <label className="inl">for PHP
              <select value={ver} onChange={(e) => setVer(e.target.value)}>{php.versions.map((v) => <option key={v}>{v}</option>)}</select>
            </label>
          </div>

          <h3 className="sub">Add an extension</h3>
          <div className="extgrid">
            {extCatalog.map((x) => {
              const have = installedNames.has(x.name);
              return (
                <div className="extcard" key={x.name}>
                  <div><b>{x.name}</b><small>{x.description}</small></div>
                  {have ? <span className="badge ok">Installed</span> :
                    <button className="btn small" disabled={busy[`x-${x.name}`]} onClick={() => addExt(x.name)}>{busy[`x-${x.name}`] ? "Installing…" : "Install"}</button>}
                </div>
              );
            })}
          </div>
          <p className="tip">Extensions are downloaded from PECL, the official PHP extension site, and matched to your PHP version.</p>

          {added.length > 0 && (
            <>
              <h3 className="sub">Downloaded extensions</h3>
              <div className="extlist">
                {added.map((e) => (
                  <div className="extrow" key={e.name}>
                    <Switch on={e.enabled} label={`${e.name} on or off`} disabled={busy[`x-${e.name}`]} onClick={() => toggleExt(e.name, !e.enabled)} />
                    <b>{e.name}</b>{e.zend && <span className="badge">zend</span>}
                    <span className="grow" />
                    <button className="link" disabled={busy[`x-${e.name}`]} onClick={() => rmExt(e.name)}>Remove</button>
                  </div>
                ))}
              </div>
            </>
          )}

          <h3 className="sub">Bundled with PHP</h3>
          <div className="extlist cols">
            {bundled.map((e) => (
              <div className="extrow" key={e.name}>
                <Switch on={e.enabled} label={`${e.name} on or off`} disabled={busy[`x-${e.name}`]} onClick={() => toggleExt(e.name, !e.enabled)} />
                <span className="mono">{e.name}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="card">
        <div className="sh"><h2>Mail</h2><span>Everything your apps send is caught by Mailpit, nothing leaves your PC</span></div>
        {!mailpit?.versions?.length ? (
          <p className="empty">Mailpit is not installed. Install it from the Versions tab and your apps' emails will show up in an inbox.</p>
        ) : (
          <>
            <div className="row wrap">
              <button className="btn primary" disabled={!mailpit.running} onClick={() => act("url", "open_url", { url: `http://127.0.0.1:${mailpit.port}` })}>Open inbox</button>
              <button className="btn" disabled={!mailpit.running || busy.testmail} onClick={() => act("testmail", "send_test_mail", {}, "Test email sent")}>Send test email</button>
              {!mailpit.running && <button className="btn" disabled={busy.mailpit} onClick={() => act("mailpit", "start_service", { id: "mailpit" }, "Mailpit started")}>▶ Start Mailpit</button>}
              <span className="tip" style={{ margin: 0 }}>{mailpit.running ? "Mailpit is running." : "Mailpit is stopped."}</span>
            </div>
            <h3 className="sub">Laravel .env</h3>
            <pre className="rawval envbox">{MAIL_ENV(ov?.smtp_port || 1025)}</pre>
            <div className="row"><button className="btn small" onClick={() => navigator.clipboard?.writeText(MAIL_ENV(ov?.smtp_port || 1025)).then(() => toast("Copied")).catch(() => toast("Select the text and copy it with Ctrl+C"))}>Copy</button>
              <span className="tip" style={{ margin: 0 }}>PHP's own <code>mail()</code> already uses this. New Laravel projects made here are set up for it automatically.</span></div>
          </>
        )}
      </div>

      <WebServerCard ov={ov} services={services} catalog={catalog} act={act} busy={busy} />

      <UpdatesCard ov={ov} act={act} busy={busy} toast={toast} update={update} setUpdate={setUpdate} updProg={updProg} appVersion={appVersion} />

      <div className="card">
        <div className="sh"><h2>Developer tools</h2><span>Available in every Terminal you open from JOJO Server</span></div>
        <div className="extgrid">
          <div className="extcard">
            <div><b>Composer</b><small>PHP package manager. Runs on the PHP version of the project you opened the terminal for.</small></div>
            {ov?.composer
              ? <button className="btn small" disabled={busy.composer} onClick={() => act("composer", "install_composer", {}, "Composer updated")}>{busy.composer ? "Updating…" : "Update"}</button>
              : <button className="btn small primary" disabled={busy.composer} onClick={() => act("composer", "install_composer", {}, "Composer installed. Open a new Terminal and type: composer --version")}>{busy.composer ? "Installing…" : "Install"}</button>}
          </div>
          <div className="extcard">
            <div><b>Node.js</b><small>node, npm and npx for Vite and Laravel's frontend.{node?.versions?.length ? "" : " Not installed yet."}</small></div>
            {node?.versions?.length ? (
              <select aria-label="Active Node.js version" value={node.active || ""} disabled={busy.node}
                onChange={(e) => act("node", "set_active_version", { id: "node", version: e.target.value }, `Node.js ${e.target.value} is now active in new terminals`)}>
                {node.versions.map((v) => <option key={v}>{v}</option>)}
              </select>
            ) : <button className="btn small primary" disabled={busy.node} onClick={() => act("node", "install_node_lts", {}, "Node.js installed. Open a new Terminal and type: node --version")}>{busy.node ? "Installing…" : "Install LTS"}</button>}
          </div>
          <div className="extcard">
            <div><b>DbGate</b><small>A modern database manager for MySQL, PostgreSQL and Redis. About 180 MB.</small>
              {busy.dbgate && lastProgress?.service === "dbgate" && (
                <div className="prog wide"><div className="bar"><i style={{ width: lastProgress.total ? `${(lastProgress.received / lastProgress.total) * 100}%` : "30%" }} /></div>
                  <small>{lastProgress.stage === "extract" ? "Unpacking…" : `${mb(lastProgress.received)}${lastProgress.total ? " / " + mb(lastProgress.total) : ""} MB`}</small></div>
              )}
            </div>
            {ov?.dbgate
              ? <button className="btn small primary" onClick={() => act("opendbgate", "launch_dbgate", {})}>Open DbGate</button>
              : <button className="btn small primary" disabled={busy.dbgate} onClick={() => act("dbgate", "install_dbgate", {}, "DbGate installed")}>{busy.dbgate ? "Installing…" : "Install"}</button>}
          </div>
        </div>
        {ov?.dbgate && <p className="tip">In DbGate add a connection: MySQL host <code>127.0.0.1</code> port <code>3306</code> user <code>root</code> (no password); PostgreSQL host <code>127.0.0.1</code> port <code>5432</code> user <code>postgres</code>; Redis host <code>127.0.0.1</code> port <code>6379</code>. Use the ports on the Dashboard tiles if you changed them.</p>}
        <p className="tip">{ov?.composer ? "Composer is installed. Terminals that were already open need to be reopened to find it." : "Install it once, then use the Terminal button in the Sites tab and run composer install."}</p>
      </div>

      <div className="card">
        <div className="sh"><h2>General</h2></div>
        {g && (
          <div className="optlist">
            <div className="opt">
              <div><b>Start JOJO Server when Windows starts</b><small>It opens minimized in the tray.</small></div>
              <Switch on={g.launch_on_login} label="Start with Windows" disabled={busy.general} onClick={() => setGeneral({ launch_on_login: !g.launch_on_login })} />
            </div>
            <div className="opt">
              <div><b>Close button</b><small>{g.close_to_tray ? "Hides the window to the tray. Services keep running." : "Quits the app and stops all services."}</small></div>
              <select value={g.close_to_tray ? "tray" : "quit"} disabled={busy.general} onChange={(e) => setGeneral({ close_to_tray: e.target.value === "tray" })}>
                <option value="tray">Minimize to tray</option>
                <option value="quit">Quit and stop services</option>
              </select>
            </div>
            <div className="opt col">
              <div><b>Start these services when JOJO Server opens</b><small>Only installed services can be selected.</small></div>
              <div className="chips">
                {services.map((s) => {
                  const on = g.autostart.includes(s.id);
                  return (
                    <label key={s.id} className={"chk" + (on ? " on" : "") + (!s.versions.length ? " off" : "")}>
                      <input type="checkbox" checked={on} disabled={!s.versions.length || busy.general}
                        onChange={() => setGeneral({ autostart: on ? g.autostart.filter((x) => x !== s.id) : [...g.autostart, s.id] })} />
                      {s.name}
                    </label>
                  );
                })}
              </div>
            </div>
          </div>
        )}
      </div>

      <div className="card">
        <div className="sh"><h2>Web server ports</h2><span>Changing a port restarts Nginx</span></div>
        <div className="confgrid">
          <label className="fld">HTTP port
            <input className="field" inputMode="numeric" value={ports.nginx} onChange={(e) => setPorts({ ...ports, nginx: e.target.value.replace(/\D/g, "") })}
              onBlur={() => commitPort("nginx")} onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
          </label>
          <label className="fld">HTTPS port
            <input className="field" inputMode="numeric" value={ports.https} onChange={(e) => setPorts({ ...ports, https: e.target.value.replace(/\D/g, "") })}
              onBlur={() => commitPort("https")} onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
          </label>
          <label className="fld">Mail (SMTP) port
            <input className="field" inputMode="numeric" value={ports.smtp} onChange={(e) => setPorts({ ...ports, smtp: e.target.value.replace(/\D/g, "") })}
              onBlur={() => commitPort("smtp")} onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
          </label>
        </div>
        <p className="tip">If port 80 or 443 is busy (IIS, Skype, XAMPP), use 8080 and 8443. Sites then open as <code>http://shop.{ov?.tld || "test"}:8080</code>. Database and cache ports are on the Dashboard tiles.</p>
      </div>

      <div className="card">
        <div className="sh"><h2>Projects folder</h2><span>Where your websites live</span></div>
        <div className="row wrap">
          <code className="pathbox">{ov?.www_dir}</code>
          <button className="btn" onClick={() => act("www", "open_path", { kind: "www" })}>📁 Open folder</button>
        </div>
        <h3 className="sub">Use a different folder</h3>
        <div className="row wrap">
          <input className="field grow" aria-label="New projects folder" placeholder="For example D:\websites" value={wwwNew} onChange={(e) => setWwwNew(e.target.value)} />
          <button className="btn" onClick={async () => {
            try { const p = await invoke("pick_folder", { initial: wwwNew || ov?.www_dir || "" }); if (p) setWwwNew(p); } catch (e) { toast(errText(e)); }
          }}>Browse…</button>
          <button className="btn primary" disabled={!wwwNew.trim() || busy.wwwdir}
            onClick={async () => { await act("wwwdir", "set_www_dir", { dir: wwwNew.trim(), moveExisting: moveEx }, "Projects folder changed"); setWwwNew(""); }}>
            {busy.wwwdir ? "Working…" : "Use this folder"}
          </button>
        </div>
        <label className="chk move"><input type="checkbox" checked={moveEx} onChange={(e) => setMoveEx(e.target.checked)} /> Move my existing projects to the new folder</label>
        <p className="tip">Nginx and PHP restart while the folder changes. When moving, the new folder must be empty. By default it is the <code>www</code> folder inside the data folder below.</p>
      </div>

      <div className="card">
        <div className="sh"><h2>Data folder</h2></div>
        <div className="row wrap">
          <code className="pathbox">{ov?.root}</code>
          <button className="btn" onClick={() => act("root", "open_path", { kind: "root" })}>📁 Open folder</button>
        </div>
        <p className="tip">PHP, MySQL and the other programs, your databases and the generated configuration live here. The installer asks where to put this folder (it suggests drive D: when there is one). To move it later, install again and pick a new folder, or set the <code>JOJO_ROOT</code> environment variable. Moving the programs and databases to the new place by hand is up to you.</p>
      </div>
    </>
  );
}

const fmtTtl = (t) => {
  if (t === -1) return "No expiry";
  if (t === -2) return "Gone";
  const d = Math.floor(t / 86400), h = Math.floor((t % 86400) / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
  return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : m ? `${m}m ${s}s` : `${s}s`;
};

// Prints a reply the way redis-cli does.
function fmtReply(v, depth = 0) {
  const pad = " ".repeat(depth * 3);
  if (v === null) return "(nil)";
  if (typeof v === "number") return `(integer) ${v}`;
  if (typeof v === "string") return v === "OK" || v === "PONG" ? v : JSON.stringify(v);
  if (Array.isArray(v)) {
    if (!v.length) return "(empty list or set)";
    return v.map((x, i) => `${pad}${i + 1}) ${fmtReply(x, depth + 1).replace(/^\s+/, "")}`).join("\n");
  }
  if (v && v.error) return `(error) ${v.error}`;
  return JSON.stringify(v);
}

const TYPE_COLORS = { string: "#14b87a", list: "#e48a1d", hash: "#3a7ac2", set: "#9a5cf0", zset: "#e0402f", stream: "#7a86cb" };

function RedisBrowser({ service, act, busy, setTab, toast }) {
  const [db, setDb] = useState(0);
  const [pattern, setPattern] = useState("");
  const [page, setPage] = useState({ keys: [], cursor: "0", dbsize: 0 });
  const [info, setInfo] = useState(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [sel, setSel] = useState(null);
  const [detail, setDetail] = useState(null);
  const [editVal, setEditVal] = useState("");
  const [ttlIn, setTtlIn] = useState("");
  const [confirm, setConfirm] = useState("");
  const [newKey, setNewKey] = useState(null);
  const [cmd, setCmd] = useState("");
  const [hist, setHist] = useState([]);
  const histRef = useRef();
  const running = !!service?.running;

  const loadKeys = useCallback(async (more = false) => {
    setLoading(true);
    setError("");
    try {
      const p = await invoke("redis_keys", { db, pattern, cursor: more ? page.cursor : "", limit: 200 });
      setPage((old) => (more ? { ...p, keys: [...old.keys, ...p.keys] } : p));
      setInfo(await invoke("redis_info", { db }));
    } catch (e) { setError(errText(e)); setPage({ keys: [], cursor: "0", dbsize: 0 }); setInfo(null); }
    finally { setLoading(false); }
  }, [db, pattern, page.cursor]);

  const open = useCallback(async (key) => {
    setSel(key); setConfirm("");
    try {
      const d = await invoke("redis_get", { db, key });
      setDetail(d);
      setEditVal(d.kind === "string" ? d.value ?? "" : "");
      setTtlIn("");
    } catch (e) { setDetail(null); toast(errText(e)); }
  }, [db, toast]);

  useEffect(() => { if (running) loadKeys(false); else { setPage({ keys: [], cursor: "0", dbsize: 0 }); setInfo(null); setSel(null); setDetail(null); } }, [running, db]); // eslint-disable-line
  useEffect(() => { if (histRef.current) histRef.current.scrollTop = histRef.current.scrollHeight; }, [hist]);

  const run = async (fn, okMsg) => {
    try { await fn(); if (okMsg) toast(okMsg); } catch (e) { toast(errText(e)); }
  };
  const afterWrite = async (key) => { await loadKeys(false); if (key) await open(key); };

  const saveString = () => run(async () => {
    await invoke("redis_set_string", { db, key: sel, value: editVal, ttl: detail.ttl > 0 ? detail.ttl : null });
    await open(sel);
  }, "Saved");
  const del = () => run(async () => {
    await invoke("redis_delete", { db, key: sel });
    setSel(null); setDetail(null); setConfirm("");
    await loadKeys(false);
  }, "Key deleted");
  const setExpiry = (seconds) => run(async () => {
    await invoke("redis_expire", { db, key: sel, seconds });
    await afterWrite(sel);
  }, seconds ? "Expiry set" : "Expiry removed");
  const createKey = () => run(async () => {
    const ttl = parseInt(newKey.ttl, 10);
    await invoke("redis_set_string", { db, key: newKey.key, value: newKey.value, ttl: ttl > 0 ? ttl : null });
    const k = newKey.key;
    setNewKey(null);
    await afterWrite(k);
  }, "Key created");
  const flush = () => run(async () => {
    await invoke("redis_flush", { db });
    setConfirm(""); setSel(null); setDetail(null);
    await loadKeys(false);
  }, `Database ${db} cleared`);
  const runCmd = async () => {
    const line = cmd.trim();
    if (!line) return;
    setCmd("");
    try {
      const r = await invoke("redis_exec", { db, line });
      setHist((h) => [...h.slice(-49), { line, ok: r.ok, out: r.ok ? fmtReply(r.reply) : `(error) ${r.reply}` }]);
      await loadKeys(false);
      if (sel) open(sel);
    } catch (e) { setHist((h) => [...h.slice(-49), { line, ok: false, out: errText(e) }]); }
  };

  const dbs = Array.from({ length: 16 }, (_, i) => i);
  const countFor = (i) => info?.dbs?.find((d) => d.db === i)?.keys;

  return (
    <>
      <header className="top">
        <div>
          <h1>Redis <span>browser</span></h1>
          <p>Browse, edit and delete keys. {info ? `Redis ${info.version} · ${info.memory} used · ${info.clients} client${info.clients === 1 ? "" : "s"}` : "Start Redis to see your data."}</p>
        </div>
        <div className="actions">
          <select className="dbpick" aria-label="Database" value={db} onChange={(e) => setDb(Number(e.target.value))} disabled={!running}>
            {dbs.map((i) => <option key={i} value={i}>DB {i}{countFor(i) ? ` (${countFor(i)})` : ""}</option>)}
          </select>
          <button className="btn" disabled={!running || loading} onClick={() => loadKeys(false)}>↻ Refresh</button>
          {confirm === "flush" ? (
            <>
              <button className="btn stop" onClick={flush}>Yes, delete all {page.dbsize} keys in DB {db}</button>
              <button className="btn" onClick={() => setConfirm("")}>Cancel</button>
            </>
          ) : (
            <button className="btn stop" disabled={!running || !page.dbsize} onClick={() => setConfirm("flush")}>Clear DB</button>
          )}
        </div>
      </header>

      {!running ? (
        <div className="card emptystate">
          {service?.versions?.length ? (
            <>
              <h2>Redis is not running</h2>
              <p className="empty">Turn it on to browse your keys.</p>
              <button className="btn primary" disabled={busy.redis} onClick={() => act("redis", "start_service", { id: "redis" }, "Redis started")}>▶ Start Redis</button>
            </>
          ) : (
            <>
              <h2>Redis is not installed</h2>
              <p className="empty">Install it from the Versions tab, then come back here.</p>
              <button className="btn primary" onClick={() => setTab("ver")}>Open Versions</button>
            </>
          )}
        </div>
      ) : (
        <>
          {error && <div className="banner"><div>{error}</div></div>}
          <div className="rbrowse">
            <div className="card kpanel">
              <div className="row">
                <input className="field" aria-label="Search keys" placeholder="Search keys, for example user:*" value={pattern}
                  onChange={(e) => setPattern(e.target.value)} onKeyDown={(e) => e.key === "Enter" && loadKeys(false)} />
                <button className="btn small" onClick={() => loadKeys(false)}>Search</button>
              </div>
              <div className="kmeta">
                <span>{page.dbsize} key{page.dbsize === 1 ? "" : "s"} in DB {db}{pattern ? ` · ${page.keys.length} match` : ""}</span>
                <button className="link" onClick={() => setNewKey({ key: "", value: "", ttl: "" })}>+ New key</button>
              </div>
              {newKey && (
                <div className="newkey">
                  <input className="field" aria-label="New key name" placeholder="Key name" value={newKey.key} onChange={(e) => setNewKey({ ...newKey, key: e.target.value })} />
                  <textarea className="field" aria-label="New key value" rows={3} placeholder="Value (text)" value={newKey.value} onChange={(e) => setNewKey({ ...newKey, value: e.target.value })} />
                  <div className="row">
                    <input className="field" aria-label="Expire after seconds" placeholder="Expire after (seconds, optional)" inputMode="numeric" value={newKey.ttl}
                      onChange={(e) => setNewKey({ ...newKey, ttl: e.target.value.replace(/\D/g, "") })} />
                    <button className="btn primary small" disabled={!newKey.key} onClick={createKey}>Create</button>
                    <button className="btn small" onClick={() => setNewKey(null)}>Cancel</button>
                  </div>
                </div>
              )}
              <ul className="klist">
                {page.keys.map((k) => (
                  <li key={k.key}>
                    <button className={sel === k.key ? "on" : ""} onClick={() => open(k.key)}>
                      <span className="tbadge" style={{ background: TYPE_COLORS[k.kind] || "#777" }}>{k.kind}</span>
                      <span className="kname" title={k.key}>{k.key}</span>
                      {k.ttl >= 0 && <span className="kttl">{fmtTtl(k.ttl)}</span>}
                    </button>
                  </li>
                ))}
                {!page.keys.length && !loading && <li className="empty pad">{page.dbsize ? "No keys match." : "This database is empty."}</li>}
              </ul>
              {page.cursor !== "0" && <button className="btn small" disabled={loading} onClick={() => loadKeys(true)}>Load more</button>}
            </div>

            <div className="card dpanel">
              {!detail ? (
                <p className="empty">Select a key to see its value.</p>
              ) : (
                <>
                  <div className="dhead">
                    <span className="tbadge" style={{ background: TYPE_COLORS[detail.kind] || "#777" }}>{detail.kind}</span>
                    <b className="dkey" title={sel}>{sel}</b>
                    <button className="link" onClick={() => navigator.clipboard?.writeText(sel).then(() => toast("Key name copied")).catch(() => {})}>Copy name</button>
                  </div>
                  <div className="dmeta">
                    <span>Size: <b>{detail.size}</b>{detail.kind === "string" ? " bytes" : " items"}</span>
                    <span>Expires: <b>{fmtTtl(detail.ttl)}</b></span>
                  </div>
                  <div className="row wrap ttlrow">
                    <input className="field narrow" aria-label="Seconds until expiry" placeholder="Seconds" inputMode="numeric" value={ttlIn} onChange={(e) => setTtlIn(e.target.value.replace(/\D/g, ""))} />
                    <button className="btn small" disabled={!ttlIn} onClick={() => setExpiry(parseInt(ttlIn, 10))}>Set expiry</button>
                    <button className="btn small" disabled={detail.ttl === -1} onClick={() => setExpiry(null)}>Remove expiry</button>
                    <span className="grow" />
                    {confirm === "del" ? (
                      <>
                        <button className="btn stop small" onClick={del}>Yes, delete</button>
                        <button className="btn small" onClick={() => setConfirm("")}>Cancel</button>
                      </>
                    ) : <button className="btn stop small" onClick={() => setConfirm("del")}>Delete key</button>}
                  </div>

                  {detail.kind === "string" && (
                    <>
                      <textarea className="field valbox" aria-label="Value" rows={10} value={editVal} onChange={(e) => setEditVal(e.target.value)} spellCheck={false} />
                      <div className="row"><button className="btn primary small" disabled={editVal === (detail.value ?? "")} onClick={saveString}>Save value</button></div>
                    </>
                  )}
                  {["list", "set"].includes(detail.kind) && (
                    <div className="tw vtable"><table><thead><tr><th>#</th><th>Value</th></tr></thead>
                      <tbody>{detail.value.map((v, i) => <tr key={i}><td>{i}</td><td><code>{v}</code></td></tr>)}</tbody></table></div>
                  )}
                  {detail.kind === "hash" && (
                    <div className="tw vtable"><table><thead><tr><th>Field</th><th>Value</th></tr></thead>
                      <tbody>{detail.value.map(([f, v], i) => <tr key={i}><td><code>{f}</code></td><td><code>{v}</code></td></tr>)}</tbody></table></div>
                  )}
                  {detail.kind === "zset" && (
                    <div className="tw vtable"><table><thead><tr><th>Member</th><th>Score</th></tr></thead>
                      <tbody>{detail.value.map(([m, s], i) => <tr key={i}><td><code>{m}</code></td><td><code>{s}</code></td></tr>)}</tbody></table></div>
                  )}
                  {detail.kind === "stream" && <pre className="rawval">{fmtReply(detail.value)}</pre>}
                  {detail.truncated && <p className="tip">Showing the first part only. Use the console below for the rest.</p>}
                  {detail.kind !== "string" && <p className="tip">To change items in a {detail.kind}, use the console below (for example <code>{detail.kind === "hash" ? `HSET ${sel} field value` : detail.kind === "list" ? `RPUSH ${sel} value` : detail.kind === "set" ? `SADD ${sel} value` : detail.kind === "zset" ? `ZADD ${sel} 1 member` : `DEL ${sel}`}</code>).</p>}
                </>
              )}
            </div>
          </div>

          <div className="card">
            <div className="sh"><h2>Console</h2><span>Runs on DB {db} · like redis-cli</span></div>
            <div className="console rconsole" ref={histRef}>
              {hist.length === 0 && <div className="t">Try: SET hello world, then GET hello</div>}
              {hist.map((h, i) => (
                <div key={i}><span className="svc">&gt; {h.line}</span>{"\n"}<span className={h.ok ? "" : "err"}>{h.out}</span></div>
              ))}
            </div>
            <div className="row" style={{ marginTop: 10 }}>
              <input className="field mono" aria-label="Redis command" placeholder="GET mykey" value={cmd} onChange={(e) => setCmd(e.target.value)} onKeyDown={(e) => e.key === "Enter" && runCmd()} />
              <button className="btn primary" disabled={!cmd.trim()} onClick={runCmd}>Run</button>
            </div>
          </div>
        </>
      )}
    </>
  );
}

function Versions({ services, catalog, checkedAt, progress, busy, act }) {
  const [custom, setCustom] = useState({ id: "php", version: "", url: "" });
  const installedSet = new Set(services.flatMap((s) => s.versions.map((v) => `${s.id}@${v}`)));
  const install = (id, version, url) =>
    act(`i-${id}@${version}`, "install_version", { id, version, url }, `${NAMES[id]} ${version} installed`);

  return (
    <>
      <header className="top">
        <div>
          <h1>Versions <span>and downloads</span></h1>
          <p>Programs live in <code>bin/&lt;service&gt;/&lt;version&gt;</code>. You can also drop an extracted folder there yourself.</p>
        </div>
        <div className="actions"><button className="btn" onClick={() => act("bin", "open_path", { kind: "bin" })}>📁 Open bin folder</button></div>
      </header>

      <div className="card">
        <div className="sh"><h2>Installed</h2></div>
        {services.every((s) => !s.versions.length) && <p className="empty">Nothing is installed yet. Pick something from the catalog below.</p>}
        {services.filter((s) => s.versions.length).map((s) => (
          <div className="vrow" key={s.id}>
            <b>{s.name}</b>
            <div className="chips">
              {s.versions.map((v) => (
                <span key={v} className={"chip" + (s.active === v ? " active" : "")}>
                  {v}
                  {s.active === v && <em>active</em>}
                  <button aria-label={`Remove ${s.name} ${v}`} title="Remove" disabled={busy[`r-${s.id}@${v}`]}
                    onClick={() => act(`r-${s.id}@${v}`, "remove_version", { id: s.id, version: v }, `${s.name} ${v} removed`)}>×</button>
                </span>
              ))}
            </div>
          </div>
        ))}
      </div>

      <div className="card">
        <div className="sh">
          <h2>Available downloads</h2>
          <span>
            {checkedAt ? `Checked ${new Date(checkedAt * 1000).toLocaleString()}` : "Not checked yet"}
            {" · "}
            <button className="link" disabled={busy.check}
              onClick={() => act("check", "refresh_catalog", {}, "Version list updated")}>
              {busy.check ? "Checking…" : "Check for new versions"}
            </button>
          </span>
        </div>
        <p className="tip" style={{ margin: "0 0 10px" }}>
          Versions are read from the official sites (windows.php.net, nginx.org, MySQL, EnterpriseDB and GitHub releases). The list also updates by itself once a day. You can add your own entries in <code>catalog.json</code>.
        </p>
        <div className="tw">
          <table>
            <thead><tr><th>Service</th><th>Version</th><th>Note</th><th></th></tr></thead>
            <tbody>
              {catalog.map((c) => {
                const key = `${c.service}@${c.version}`;
                const p = progress[key];
                const have = installedSet.has(key);
                const going = busy[`i-${key}`];
                return (
                  <tr key={key}>
                    <td>{NAMES[c.service] || c.service}</td>
                    <td><code>{c.version}</code></td>
                    <td><small>{c.note}</small></td>
                    <td className="right">
                      {have ? <span className="badge ok">Installed</span> : going ? (
                        <div className="prog">
                          <div className="bar"><i style={{ width: p?.total ? `${(p.received / p.total) * 100}%` : "30%" }} /></div>
                          <small>{p?.stage === "extract" ? "Extracting…" : p ? `${mb(p.received)}${p.total ? " / " + mb(p.total) : ""} MB` : "Starting…"}</small>
                        </div>
                      ) : <button className="btn small" onClick={() => install(c.service, c.version, c.url)}>Install</button>}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>

      <div className="card">
        <div className="sh"><h2>Install from a URL</h2><span>Windows x64 ZIP</span></div>
        <div className="row wrap">
          <select className="field" aria-label="Service" value={custom.id} onChange={(e) => setCustom({ ...custom, id: e.target.value })}>
            {[...ORDER, "node"].map((id) => <option key={id} value={id}>{NAMES[id]}</option>)}
          </select>
          <input className="field narrow" aria-label="Version" placeholder="Version (8.3.12)" value={custom.version} onChange={(e) => setCustom({ ...custom, version: e.target.value })} />
          <input className="field grow" aria-label="Download URL" placeholder="https://…/package.zip" value={custom.url} onChange={(e) => setCustom({ ...custom, url: e.target.value })} />
          <button className="btn primary" disabled={!custom.version || !custom.url} onClick={() => install(custom.id, custom.version, custom.url)}>Install</button>
        </div>
        <p className="tip">For PHP, choose the <b>Non-Thread-Safe</b> ZIP. For MySQL, use the official Community ZIP. For PostgreSQL, use the EDB binaries ZIP.</p>
      </div>
    </>
  );
}
