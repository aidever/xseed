/* xseed 小助手前端：无构建步骤，原生 JS。设计规范见 docs/ui/README.md。 */
'use strict';

const root = document.getElementById('root');
let S = { session: null, csrf: null };
let actions = {};      // 当前页面的点击动作：data-act="名字"
let timer = null;

/* ───────── 工具 ───────── */
const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const fmtN = (n) => Number(n || 0).toLocaleString('en-US');
function fmtSize(b) {
  b = Number(b || 0);
  const u = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let i = 0;
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
  return (i === 0 ? b : b.toFixed(b >= 100 ? 0 : 1)) + ' ' + u[i];
}
function fmtAgo(t) {
  if (!t) return '';
  const s = Math.max(0, Date.now() / 1000 - t);
  if (s < 90) return '刚刚';
  if (s < 3600) return Math.round(s / 60) + ' 分钟前';
  if (s < 86400) return Math.round(s / 3600) + ' 小时前';
  return Math.round(s / 86400) + ' 天前';
}
const letter = (name) => (String(name || '?').trim()[0] || '?').toUpperCase();
function toast(msg) {
  const t = document.createElement('div');
  t.className = 'toast'; t.textContent = msg; t.setAttribute('role', 'status');
  document.body.appendChild(t);
  setTimeout(() => t.remove(), 2600);
}
const val = (id) => (document.getElementById(id) || {}).value || '';

async function api(method, path, body) {
  const opt = { method, headers: {} };
  if (body !== undefined) { opt.headers['Content-Type'] = 'application/json'; opt.body = JSON.stringify(body); }
  if (method !== 'GET' && S.csrf) opt.headers['X-CSRF-Token'] = S.csrf;
  const r = await fetch('/api' + path, opt);
  let data = {};
  try { data = await r.json(); } catch (_) { /* 空响应 */ }
  if (r.status === 401 && path !== '/login') { S.csrf = null; go('#/login'); throw new Error(data.error || '请先登录'); }
  if (!r.ok) throw new Error(data.error || ('请求失败 ' + r.status));
  if (data.csrf) S.csrf = data.csrf;
  return data;
}

function go(hash) { if (location.hash !== hash) location.hash = hash; else route(); }

/* ───────── 主题 ───────── */
function getTheme() { try { return localStorage.getItem('xseed.theme') || 'dark'; } catch (_) { return 'dark'; } }
function setTheme(t) { try { localStorage.setItem('xseed.theme', t); } catch (_) { /* 无痕模式 */ } document.documentElement.dataset.theme = t; }
setTheme(getTheme());

/* ───────── 图标 ───────── */
const I = {
  logo: '<svg width="26" height="26" viewBox="0 0 26 26" aria-hidden="true"><circle cx="10" cy="13" r="7" fill="none" stroke="var(--acc)" stroke-width="2.4"/><circle cx="16" cy="13" r="7" fill="none" stroke="currentColor" stroke-opacity=".45" stroke-width="2.4"/></svg>',
  copy: '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h8"/></svg>',
  refresh: '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 11a8 8 0 0 0-14.6-4.4L4 8M4 4v4h4M4 13a8 8 0 0 0 14.6 4.4L20 16M20 20v-4h-4"/></svg>',
  warn: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" style="flex:none"><path d="M12 4 2.5 20h19z"/><path d="M12 10v4M12 17.2v.1"/></svg>',
  check: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>',
};

/* ───────── 星轨 ───────── */
// planets: [{id, label, state:'on'|'bad'|'off', weight?}]；beam: 'live'|'still'
function orbit({ size = 340, planets = [], core = ['你的 NAS', ''], coreAcc = false, beam = 'still', caption = '', label = '' }) {
  const C = size / 2, R = size / 2 - 30, inner = size * 0.44;
  const n = Math.max(planets.length, 1);
  let beams = '', ps = '';
  planets.slice(0, 8).forEach((p, i) => {
    const deg = -62 + i * (360 / Math.max(n, 6));
    const a = deg * Math.PI / 180;
    const x = C + R * Math.cos(a), y = C + R * Math.sin(a);
    if (p.state !== 'off') {
      const cls = p.state === 'bad' ? 'bad' : (p.beam || beam);
      const h = p.weight > 40 ? 3 : 2;
      beams += `<div class="beam ${cls}" style="left:${C}px;top:${C - 1}px;width:${R - 22}px;height:${h}px;transform:rotate(${deg}deg)"></div>`;
    }
    ps += `<div class="planet ${p.state}" style="left:${(x - 40).toFixed(1)}px;top:${(y - 22).toFixed(1)}px"><b>${esc(p.id)}</b><span>${esc(p.label)}</span></div>`;
  });
  const cs = 92;
  return `<div class="orbit" role="img" aria-label="${esc(label)}" style="width:${size}px;height:${size}px">
    <div class="ring" style="left:30px;top:30px;width:${size - 60}px;height:${size - 60}px"></div>
    <div class="ring in" style="left:${C - inner / 2}px;top:${C - inner / 2}px;width:${inner}px;height:${inner}px"></div>
    ${beams}
    <div class="core ${coreAcc ? 'acc' : ''}" style="left:${C - cs / 2}px;top:${C - cs / 2}px;width:${cs}px;height:${cs}px"><span style="font-size:${coreAcc ? 22 : 13}px;font-weight:600;line-height:1.1">${esc(core[0])}</span><span style="font-size:11px;opacity:.7">${esc(core[1])}</span></div>
    ${ps}
    ${caption ? `<span class="caption">${esc(caption)}</span>` : ''}
  </div>`;
}

// 星球上的缩写：取站名首字母；撞车时改用“首字母+末字母”，让每颗星都能区分
const GENERIC = new Set(['www', 'tracker', 'pt', 't', 'tk']);
function initials(sites) {
  const label = (s) => {
    if (!s.site_ref.startsWith('host:')) return String(s.name || '?').replace(/\s+/g, '');
    const parts = s.host.split('.');
    return parts.find((p, i) => i < parts.length - 1 && !GENERIC.has(p)) || parts[0];
  };
  const labels = sites.map(label);
  const one = labels.map((l) => letter(l));
  return labels.map((l, i) => (one.filter((x) => x === one[i]).length > 1 && l.length > 1 ? one[i] + l[l.length - 1].toLowerCase() : one[i]));
}

function sitePlanets(sites, opts = {}) {
  const ids = initials(sites);
  return sites.slice(0, 8).map((s, i) => ({
    id: ids[i],
    label: opts.label ? opts.label(s) : fmtN(s.torrents),
    state: s.status === 'denied' || !s.enabled ? 'off' : opts.bad && s.status === 'need_cookie' ? 'bad' : 'on',
    beam: opts.beam ? opts.beam(s) : undefined,
    weight: s.torrents,
  }));
}
function siteBalls(sites) {
  const ids = initials(sites), m = {};
  sites.forEach((s, i) => { m[s.site_ref] = ids[i]; });
  return m;
}

/* ───────── 外框 ───────── */
const NAV = [['#/', '概览'], ['#/sites', '站点'], ['#/jobs', '辅种记录'], ['#/settings', '设置'], ['#/privacy', '隐私说明']];

function shell(active, inner, side = {}) {
  const nav = NAV.map(([h, t]) => `<a href="${h}"${h === active ? ' aria-current="page"' : ''}>${t}</a>`).join('');
  const dl = side.downloader;
  return `<div class="app dots">
    <aside class="side">
      <a class="logo" href="#/">${I.logo}<b>xseed</b></a>
      <nav class="nav" aria-label="主导航">${nav}</nav>
      <div class="side-status">
        <div><span class="dot ${dl ? (dl.ok === false ? 'err' : 'ok') : ''}"></span><span>${esc(dl ? (dl.ok === false ? '下载器连接失败' : (dl.status || '下载器')) : '下载器')}</span></div>
        ${netLine(side.network)}
      </div>
    </aside>
    <main class="main" id="main"><div class="wrap${side.narrow ? ' narrow' : ''}">${inner}</div></main>
  </div>`;
}

// 网络状态：off（未完成设置）| no_registry | connecting | online | error
const NET = { online: ['ok', '辅种网络'], connecting: ['warn', '正在连接辅种网络'], error: ['err', '辅种网络连不上'], no_registry: ['', '辅种网络'], off: ['', '辅种网络'] };
function netLine(n) {
  const st = (n && n.state) || 'off';
  const [dot, text] = NET[st] || NET.off;
  const tail = st === 'online' ? (n.entry ? n.entry.host : '') : st === 'no_registry' ? '未配置' : st === 'off' ? '' : '';
  return `<div><span class="dot ${dot}"></span><span>${text}</span><span style="margin-left:auto;max-width:96px" class="tiny trunc">${esc(tail)}</span></div>`;
}
const netOn = (n) => n && (n.state === 'online' || n.state === 'connecting');

/* ───────── 路由 ───────── */
const ROUTES = {
  '#/': pageOverview, '#/sites': pageSites, '#/jobs': pageJobs, '#/settings': pageSettings, '#/privacy': pagePrivacy,
  '#/login': pageLogin, '#/setup/1': setupAdmin, '#/setup/2': setupDownloader, '#/setup/3': setupSites, '#/setup/4': setupDryRun,
};

async function route() {
  clearInterval(timer); timer = null; actions = {};
  const hash = (location.hash || '#/').split('?')[0];
  try {
    const s = S.session = await api('GET', '/session');
    if (s.csrf) S.csrf = s.csrf;
    // 首次运行强制走向导
    const need = !s.has_admin ? '#/setup/1' : !s.logged_in ? '#/login' : !s.has_downloader ? '#/setup/2' : !s.setup_done ? null : null;
    if (need && hash !== need) return go(need);
    if (s.logged_in && !s.setup_done && !hash.startsWith('#/setup')) return go(s.has_downloader ? '#/setup/3' : '#/setup/2');
    if (s.logged_in && hash === '#/login') return go('#/');
    await (ROUTES[hash] || pageOverview)();
    const h = document.querySelector('h1');
    if (h) document.title = h.textContent.replace(/\s+/g, ' ').slice(0, 40) + ' · xseed';
  } catch (e) {
    if (!String(e.message).includes('登录')) root.innerHTML = `<div class="wiz dots"><div class="wmain"><div><h1 class="wh">出了点问题</h1><p class="muted">${esc(e.message)}</p><button class="btn" data-act="reload">重试</button></div></div></div>`;
    actions.reload = route;
  }
}
window.addEventListener('hashchange', route);

document.addEventListener('click', (ev) => {
  const el = ev.target.closest('[data-act]');
  if (!el || el.disabled) return;
  const fn = actions[el.dataset.act];
  if (!fn) return;
  ev.preventDefault();
  Promise.resolve(fn(el.dataset.arg, el, ev)).catch((e) => toast(e.message));
});
document.addEventListener('submit', (ev) => {
  const fn = actions[ev.target.dataset.submit];
  if (!fn) return;
  ev.preventDefault();
  Promise.resolve(fn(ev.target)).catch((e) => toast(e.message));
});

async function copyText(text) {
  try { await navigator.clipboard.writeText(text); toast('已复制'); }
  catch (_) {
    const ta = document.createElement('textarea'); ta.value = text; document.body.appendChild(ta); ta.select();
    try { document.execCommand('copy'); toast('已复制'); } catch (__) { toast('复制失败，请手动选择'); }
    ta.remove();
  }
}

/* ───────── 概览 ───────── */
async function pageOverview() {
  const st = await api('GET', '/status');
  const s = st.stats, sites = st.sites || [];
  const onSites = sites.filter((x) => x.enabled && x.status !== 'denied');
  const alerts = (st.alerts || []).map((a) => a.kind === 'need_cookie'
    ? `<section class="banner warn" aria-label="需要处理">${I.warn}<p><b>${esc(a.site)} 需要 Cookie 才能下载种子。</b><span> 用 PT-depiler 同步，或在站点页手动填写。</span></p><a class="btn sm ghost" href="#/sites" style="border-color:currentColor;color:var(--warn)">去处理</a></section>`
    : `<section class="banner err" aria-label="需要处理">${I.warn}<p><b>连不上下载器。</b><span> 请检查下载器是否在运行、地址和密码是否正确。${esc(a.detail || '')}</span></p><a class="btn sm ghost" href="#/settings" style="border-color:currentColor;color:var(--err)">去设置</a></section>`).join('');
  const scanning = st.scan && st.scan.running;
  const net = st.network || {};
  const live = netOn(net);
  const balls = siteBalls(sites);
  const ad = st.added || { today: { added: 0, size: 0, by_site: {} }, total: { added: 0, size: 0 }, heat: [] };
  const today = ad.today, byS = today.by_site || {};
  const hasAdds = ad.total.added > 0;
  const pipeNote = net.state === 'online' ? `今天探测 ${fmtN(net.probes_today)} 次；社区里找到 ${fmtN(net.peers)} 个同款持有者，答对会员题 ${fmtN(net.answers_today)} 次`
    : net.state === 'connecting' ? '正在连接辅种网络…'
    : net.state === 'error' ? `辅种网络暂时连不上：${net.error || ''}`
    : net.state === 'no_registry' ? '还没有可用的社区节点清单，可在“设置”中填写' : '完成设置后开始连接辅种网络';
  const steps = [['01', '发现同款文件', '社区里有人有同样的文件'], ['02', '会员题通过', '对方证明了是该站会员'], ['03', '下载种子', '用你自己的账号下载'], ['04', '校验一致', '文件列表逐个比对'], ['05', '开始做种', '直接用已有文件']];
  root.innerHTML = shell('#/', `
    <div class="row" style="justify-content:space-between">
      <span class="pill"><span class="pulse${scanning ? '' : ' off'}" aria-hidden="true"></span>${scanning ? `正在读取下载器 <span class="mono">${fmtN(st.scan.done)} / ${fmtN(st.scan.total)}</span>` : `本机清单 ${st.scan && st.scan.last_at ? fmtAgo(st.scan.last_at) + '更新' : '尚未扫描'}`}</span>
      <button class="btn sm" data-act="rescan" ${scanning ? 'disabled' : ''}>${I.refresh}重新扫描</button>
    </div>
    <section class="hero" aria-labelledby="hero-h">
      <div class="hero-l">${hasAdds ? `
        <h1 id="hero-h" style="font-size:15px;font-weight:500;color:var(--ink2)">${today.added ? `同一份文件，今天又在 <b style="color:var(--ink);font-weight:600">${fmtN(Object.keys(byS).length)} 个站</b> 多做了种` : '今天还没有新的辅种，小助手会继续寻找'}</h1>
        <div class="row" style="align-items:flex-end;gap:14px"><span class="big acc-t">+${fmtN(today.added)}</span><span style="display:flex;flex-direction:column;padding-bottom:4px;font-size:13px" class="muted"><span>个种子</span><span class="mono" style="color:var(--ink)">${fmtSize(today.size)}</span></span></div>
        <p class="muted" style="max-width:380px">新种子都直接指向你硬盘里已有的文件——<span style="color:var(--ink)">多做种，不多占一个字节。</span></p>
        <dl class="kv3">
          <div><dt>累计多做种</dt><dd>${fmtSize(ad.total.size)}</dd></div>
          <div><dt>xseed 加入</dt><dd>${fmtN(ad.total.added)}</dd></div>
          <div><dt>额外占用</dt><dd>0 B</dd></div>
        </dl>` : `
        <h1 id="hero-h" style="font-size:15px;font-weight:500;color:var(--ink2)">你的 NAS 里，有 <b style="color:var(--ink);font-weight:600">${fmtN(s.sites)} 个站</b> 的种子</h1>
        <div class="row" style="align-items:flex-end;gap:14px"><span class="big${s.ok >= 10000 ? ' sm' : ''}">${fmtN(s.ok)}</span><span style="display:flex;flex-direction:column;padding-bottom:4px;font-size:13px" class="muted"><span>个种子</span><span class="mono" style="color:var(--ink)">${fmtSize(s.total_size)}</span></span></div>
        <p class="muted" style="max-width:400px">${live && net.discovered ? `其中 <b style="color:var(--ink)">${fmtN(net.discovered)}</b> 个已在社区里找到同款。` : ''}xseed 会帮这些文件在更多站点做种——<span style="color:var(--ink)">多做种，不多占一个字节。</span></p>
        <dl class="kv3">
          <div><dt>可以回答别人</dt><dd>${fmtN(s.holders)}</dd></div>
          <div><dt>已在多站做种</dt><dd>${fmtN(s.multi_site)}</dd></div>
          <div><dt>参与的站点</dt><dd>${fmtN(onSites.length)}</dd></div>
        </dl>`}
      </div>
      <div class="hero-r">${orbit({ planets: sitePlanets(sites, { bad: true, label: (x) => byS[x.site_ref] ? '+' + byS[x.site_ref] : x.status === 'need_cookie' ? '登录失效' : fmtN(x.torrents), beam: (x) => (byS[x.site_ref] ? 'live' : 'still') }), core: ['你的 NAS', fmtN(s.ok) + ' 种'], label: '星轨：' + sites.map((x) => x.name + (byS[x.site_ref] ? `（今天 +${byS[x.site_ref]}）` : x.enabled ? '（参与）' : '（未开启）')).join('、') })}</div>
    </section>
    <section class="card" aria-labelledby="pipe-h">
      <div class="card-head"><h2 id="pipe-h">一个辅种是怎么来的</h2><span class="small faint">${pipeNote}</span></div>
      <ol class="pipe">${steps.map(([no, t, d], i) => { const v = live ? [net.discovered, net.offers, net.fetched, net.verified, net.seeding][i] ?? null : null; return `<li><span class="mono tiny faint">${no}</span><div class="row" style="justify-content:space-between;gap:8px"><span style="font-size:13px;font-weight:500">${t}</span><span class="mono" style="font-size:22px;color:var(${v ? '--ink' : '--ink3'})">${v === null ? '—' : fmtN(v)}</span></div><div class="bar">${v ? `<i style="width:${Math.max(4, Math.min(100, Math.round(v / Math.max(s.ok, 1) * 100)))}%"></i>` : ''}</div><span class="tiny faint">${d}</span></li>`; }).join('')}</ol>
    </section>
    ${alerts}
    <div class="cols">
      <section class="card" style="flex:7 1 420px;min-width:0" aria-labelledby="recent-h">
        <div class="card-head"><h2 id="recent-h">最近辅种</h2><a href="#/jobs" class="small muted">全部记录 →</a></div>
        ${(st.recent || []).length ? `<div class="feed">${st.recent.map((x) => feedRow(x, balls)).join('')}</div>` : '<p class="empty">还没有辅种记录。<br>社区里有人答对会员题、告知你种子编号后，新加入的种子会出现在这里。</p>'}
      </section>
      ${hasAdds ? heatCard(ad.heat) : `<section class="card" style="flex:4 1 280px;min-width:0" aria-labelledby="lib-h">
        <div class="card-head"><h2 id="lib-h">本机清单</h2><span class="tiny faint">只读下载器，不读硬盘</span></div>
        <dl class="facts" style="border:0;padding:0">
          <div><dt>识别到站点</dt><dd class="mono">${fmtN(s.ok)}</dd></div>
          <div><dt>同时属于多个站，暂不参与</dt><dd class="mono">${fmtN(s.ambiguous)}</dd></div>
          <div><dt>公共种子，不参与</dt><dd class="mono">${fmtN(s.unknown_site)}</dd></div>
          <div><dt>无法读取文件列表</dt><dd class="mono">${fmtN(s.invalid)}</dd></div>
        </dl>
      </section>`}
    </div>`, { downloader: st.downloader, network: st.network });
  actions.rescan = async () => { await api('POST', '/scan'); toast('开始重新扫描'); setTimeout(route, 600); };
  if (scanning) timer = setInterval(() => { if ((location.hash || '#/') === '#/') pageOverview().catch(() => {}); }, 2500);
}

/* ───────── 站点 ───────── */
const CRED = { ptd: ['PT-depiler', 'ok-t'], manual: ['手动填写', 'ok-t'], tracker: ['tracker 地址', 'ok-t'], none: ['需要 Cookie', 'warn-t'] };
const P2P = { allow: ['允许', 'ok-t'], unknown: ['未表态', 'muted'], deny: ['不允许', 'err-t'] };

async function pageSites() {
  const [{ sites }, ptd, st] = await Promise.all([api('GET', '/sites'), api('GET', '/ptd'), api('GET', '/status')]);
  const on = sites.filter((s) => s.enabled && s.status !== 'denied').length;
  const last = ptd.last;
  const lastText = !last ? '还没有收到过备份' : last.ok ? `${fmtAgo(last.at)}同步了 ${last.sites} 个站点、${last.cookies} 个 Cookie` : (last.error === 'ptd_decrypt_failed' ? '备份无法解密：请确认这里填写的加密密钥与 PT-depiler 中一致' : last.error === 'ptd_hash_mismatch' ? '备份文件校验失败，请重新备份一次' : 'PT-depiler 备份格式无法识别，已继续使用上次同步的登录信息');
  const ids = initials(sites);
  const card = (s, i) => {
    const live = s.enabled && s.status !== 'denied';
    const [ct, cc] = CRED[s.cred] || CRED.none;
    const [pt, pc] = P2P[s.p2p] || P2P.unknown;
    return `<article class="site ${live ? 'on' : 'off'}">
      <div class="row" style="gap:12px;flex-wrap:nowrap">
        <span class="av" aria-hidden="true">${esc(ids[i])}</span>
        <div style="flex:1 1 auto;min-width:0;display:flex;flex-direction:column"><h2 style="font-size:15px;font-weight:600" class="trunc">${esc(s.name)}</h2><span class="mono tiny faint trunc">${esc(s.host)}</span></div>
        <button type="button" class="switch" role="switch" aria-checked="${live}" aria-label="${esc(s.name)} 参与辅种" data-act="toggle" data-arg="${esc(s.site_ref)}" ${s.status === 'denied' ? 'disabled' : ''}></button>
      </div>
      <div class="row" style="gap:18px;align-items:baseline">
        <div style="display:flex;flex-direction:column"><span class="num">${fmtN(s.torrents)}</span><span class="tiny faint">本机种子</span></div>
        <div style="display:flex;flex-direction:column"><span class="num acc-t">${fmtN(s.holders)}</span><span class="tiny faint">可以回答别人</span></div>
        <div style="display:flex;flex-direction:column"><span class="num">${fmtN(s.xseed_added || 0)}</span><span class="tiny faint">由 xseed 辅入</span></div>
      </div>
      <dl class="facts">
        <div><dt>登录</dt><dd class="${cc}">${ct}</dd></div>
        <div><dt>站长态度</dt><dd class="${pc}">${pt}</dd></div>
        <div><dt>今日额度</dt><dd class="mono muted">${s.added_today} / ${s.daily_add_limit}</dd></div>
      </dl>
      <details><summary class="small muted" style="cursor:pointer">手动填写 Cookie</summary>
        <form data-submit="cookie" data-ref="${esc(s.site_ref)}" class="row" style="margin-top:10px;gap:8px;flex-wrap:nowrap">
          <label class="sr" for="ck-${esc(s.site_ref)}">${esc(s.name)} 的 Cookie</label>
          <input class="input mono" id="ck-${esc(s.site_ref)}" name="cookie" placeholder="uid=…; pass=…" autocomplete="off" style="height:38px">
          <button class="btn sm" type="submit">保存</button>
        </form>
      </details>
    </article>`;
  };
  root.innerHTML = shell('#/sites', `
    <div class="head"><div><h1 class="h1">${sites.length} 个站点，<em>${on}</em> 个在辅种</h1><p class="muted">从你的做种列表里自动认出来的。打开开关，它就加入星轨。</p></div>
      <button class="btn" data-act="rescan">${I.refresh}重新识别</button></div>
    ${sites.length ? `<section class="site-grid" aria-label="站点列表">${sites.map(card).join('')}</section>` : `<section class="card"><p class="empty">还没有识别到站点。${st.scan && st.scan.running ? '正在读取下载器…' : '请确认下载器里有已完成的 PT 种子，然后点“重新识别”。'}</p></section>`}
    <section class="card flush" aria-labelledby="ptd-h"><div class="row" style="align-items:stretch;gap:0">
      <div class="grow" style="padding:28px;display:flex;flex-direction:column;gap:18px;border-right:1px solid var(--line);flex-basis:340px">
        <div class="row" style="justify-content:space-between;flex-wrap:nowrap;align-items:flex-start"><div style="display:flex;flex-direction:column;gap:6px"><h2 id="ptd-h" style="font-size:20px;font-weight:700;letter-spacing:-.02em">让 PT-depiler 替你管 Cookie</h2><p class="muted">它每次自动备份，各站登录信息就顺手同步到这里。站点登录失效？在浏览器里重新登录一次就好。</p></div>
          <button type="button" class="switch" role="switch" aria-checked="${ptd.enabled}" aria-label="接收 PT-depiler 备份" data-act="ptdToggle"></button></div>
        <div aria-hidden="true" class="row" style="gap:0;flex-wrap:nowrap"><span class="pill" style="border-radius:8px;height:34px;color:var(--ink)">PT-depiler</span><span class="flow${ptd.enabled ? '' : ' off'}"></span><span class="pill mono" style="border-radius:8px;height:34px;background:var(--acc);border-color:var(--acc);color:var(--on-acc);font-weight:600">xseed</span></div>
        <span class="status ${last && !last.ok ? 'warn-t' : 'muted'}" style="white-space:normal"><span class="dot ${last ? (last.ok ? 'ok' : 'warn') : ''}"></span>${esc(ptd.enabled ? lastText : '未开启：打开右上角开关后，PT-depiler 才能把备份发到这里')}</span>
        <ol class="small muted" style="margin:0;padding-left:20px;display:flex;flex-direction:column;gap:4px">
          <li>PT-depiler：设置 → 备份 → 添加 WebDAV</li><li>填入右边的地址、用户名、密码（不要勾选 Digest）</li><li>备份内容勾选“Cookie”和“元数据”，自动备份间隔 6 小时</li></ol>
      </div>
      <div class="grow" style="padding:28px;display:flex;flex-direction:column;gap:12px;background:var(--panel2);flex-basis:340px">
        ${[['地址', ptd.url, 'url'], ['用户名', ptd.user, 'user'], ['密码', ptd.password, 'password']].map(([k, v, a]) => `<div class="field"><span class="tiny faint">${k}</span><div class="copy"><code>${a === 'password' ? esc(v.slice(0, 4) + '••••••••••' + v.slice(-3)) : esc(v)}</code><button type="button" class="icon-btn" aria-label="复制${k}" data-act="copy" data-arg="${esc(v)}">${I.copy}</button>${a === 'password' ? `<button type="button" class="icon-btn" aria-label="重新生成密码" data-act="rotate">${I.refresh}</button>` : ''}</div></div>`).join('')}
        <form class="field" data-submit="ptdKey"><label for="ptd-key" class="tiny faint" style="font-weight:400">加密密钥（推荐，与 PT-depiler 备份设置里的一致）</label>
          <div class="row" style="gap:8px;flex-wrap:nowrap"><input class="input" id="ptd-key" type="password" autocomplete="off" placeholder="${ptd.has_key ? '已设置，留空不修改' : '未设置'}"><button class="btn" type="submit">保存</button></div></form>
      </div>
    </div></section>`, { downloader: st.downloader, network: st.network });
  actions.toggle = async (ref, el) => {
    const next = el.getAttribute('aria-checked') !== 'true';
    await api('PUT', '/sites/' + encodeURIComponent(ref), { enabled: next });
    pageSites();
  };
  actions.rescan = async () => { await api('POST', '/scan'); toast('开始重新识别，稍后刷新'); };
  actions.copy = (v) => copyText(v);
  actions.rotate = async () => { if (!confirm('重新生成后，需要在 PT-depiler 里改成新密码。继续吗？')) return; await api('POST', '/ptd/rotate'); pageSites(); };
  actions.ptdToggle = async (_, el) => { await api('PUT', '/ptd', { enabled: el.getAttribute('aria-checked') !== 'true' }); pageSites(); };
  actions.ptdKey = async () => { const k = val('ptd-key'); if (!k) return toast('请输入加密密钥'); await api('PUT', '/ptd', { key: k }); toast('已保存加密密钥'); pageSites(); };
  actions.cookie = async (form) => { await api('PUT', '/sites/' + encodeURIComponent(form.dataset.ref), { cookie: form.cookie.value }); toast('已保存 Cookie'); pageSites(); };
}

/* ───────── 辅种记录 ───────── */
const STEPS = ['发现', '答题', '下载', '校验', '做种'];
// 失败原因文案（03 §5.3）
const REASON = {
  daily_limit: (s) => `${s} 今天的加种额度用完了，明天继续。`,
  site_rate_limited: (s) => `${s} 暂时限制了下载，今天会自动减速，明天继续。`,
  site_unreachable: (s) => `${s} 暂时连不上，稍后自动重试。`,
  downloader_unreachable: () => '连不上下载器，请检查下载器是否在运行、地址和密码是否正确。',
  site_auth_failed: (s) => `${s} 下载种子失败：登录信息已失效。请在 PT-depiler 中重新登录该站，或在站点页更新 Cookie。`,
  site_denied: (s) => `${s} 拒绝了下载：登录信息可能已失效，或账号没有下载权限。`,
  passkey_invalid: (s) => `${s} 的 passkey 已失效。请到站点重置 passkey，并在下载器中更新任意一个该站种子的 tracker 地址。`,
  no_source: (s) => `${s} 上找不到这个种子，可能已被删除。`,
  ih_mismatch: () => '下载到的种子与对方告知的不一致，已放弃。',
  sfp_mismatch: () => '下载到的种子文件列表与本机不一致，已放弃。',
  foreign_tracker: (s) => `种子里的 tracker 不属于 ${s}，已放弃。`,
  source_state_changed: () => '本机原种子有变化（被移动、删除或不再完整），已放弃。',
  downloader_error: () => '下载器拒绝加入这个种子。',
  missing_files: () => '加入后下载器找不到文件，已自动移除（文件未动）。原种子的文件可能被移动或改名。',
  tracker_unregistered: (s) => `${s} 的 tracker 不认这个种子，已自动移除（文件未动）。`,
  tracker_auth_failed: (s) => `${s} 的 tracker 拒绝了你的 passkey，已自动移除（文件未动）。`,
  already_present: () => '下载器里已经有这个种子。',
  cancelled: () => '已随“全部移除”取消。',
  site_disabled: (s) => `${s} 已关闭辅种（你关掉了开关，或站长表态不允许），没有加入。`,
};
function fmtIn(t) {
  const s = Math.max(0, t - Date.now() / 1000);
  return s < 90 ? '马上' : s < 3600 ? `约 ${Math.round(s / 60)} 分钟后` : s < 86400 ? `约 ${Math.round(s / 3600)} 小时后` : '明天';
}
// → {status, kind: ok|info|err|muted, note, live, failAt}
function jobView(x) {
  const r = x.reason && REASON[x.reason] ? REASON[x.reason](x.site) : '';
  const base = `${fmtSize(x.size)} · ${x.site} #${x.torrent_id}`;
  switch (x.state) {
    case 'seeding': return { status: '做种中', kind: 'ok', note: x.reason === 'already_present' ? `${base} · ${r}` : `${base} · 直接用已有文件` };
    case 'pending':
      if (r) return { status: '稍后继续', kind: 'info', note: r, live: true };
      return { status: '排队中', kind: 'info', note: x.not_before > Date.now() / 1000 ? `${base} · ${fmtIn(x.not_before)}开始（启动时间随机错开）` : base, live: true };
    case 'fetching': return { status: '下载种子', kind: 'info', note: `${base} · 正在用你的账号下载种子`, live: true };
    case 'verifying': return { status: '校验中', kind: 'info', note: `${base} · 正在核对文件列表`, live: true };
    case 'adding': return { status: '加入中', kind: 'info', note: `${base} · 正在加入下载器`, live: true };
    case 'observing': return { status: '等待确认', kind: 'info', note: `${base} · 已加入下载器，确认文件完整、tracker 正常后开始做种`, live: true };
    case 'failed': return { status: '失败', kind: 'err', note: r || `失败：${x.reason || '未知原因'}`, failAt: x.step };
    default: return { status: x.reason === 'site_disabled' ? '已取消' : '已撤回', kind: 'muted', note: r || `${base} · 已从下载器移除（文件未动）` };
  }
}
// 五步进度轨：前 step 步完成；failAt 为失败的那一步；live 时下一步闪烁
function track(step, v = {}) {
  const fail = v.failAt ?? -1;
  const label = fail >= 0 ? `卡在「${STEPS[fail]}」这一步` : step >= 5 ? '五步全部完成' : `已完成到「${STEPS[Math.max(step - 1, 0)]}」`;
  return `<span class="track" role="img" aria-label="${label}">${STEPS.map((_, i) => {
    const cls = i === fail ? 'bad' : i < step ? 'on' : v.live && i === step ? 'live' : '';
    return `${i ? `<i class="${i <= step && fail < 0 ? 'on' : ''}"></i>` : ''}<b class="${cls}"></b>`;
  }).join('')}</span>`;
}
const KIND = { ok: 'var(--ink2)', info: 'var(--info)', err: 'var(--err)', muted: 'var(--ink3)' };
function jobRow(x, balls = {}) {
  const v = jobView(x);
  return `<li class="job">
    <div class="job-main"><b class="mono trunc" style="font-size:13px;font-weight:500">${esc(x.name || '（未命名）')}</b>
      <span class="small" style="color:${x.state === 'failed' ? 'var(--err)' : 'var(--ink3)'}">${esc(v.note)}</span></div>
    <div class="row" style="gap:10px;flex-wrap:nowrap"><span class="ball" aria-hidden="true" title="${esc(x.site)}">${esc(balls[x.site_ref] || '?')}</span>${track(x.step, v)}</div>
    <span class="job-st" style="color:${KIND[v.kind]}">${v.status}</span>
    <span class="job-t mono tiny faint">${fmtAgo(x.updated_at)}</span>
    <span class="job-a">${x.state === 'failed' ? `<button class="btn sm" data-act="retry" data-arg="${x.id}">重试</button>` : ''}</span></li>`;
}
function feedRow(x, balls) {
  const v = jobView(x);
  const c = { ok: 'var(--ok)', info: 'var(--info)', err: 'var(--err)', muted: 'var(--ink3)' }[v.kind];
  return `<div><span class="ball" aria-hidden="true">${esc(balls[x.site_ref] || '?')}</span>
    <div style="flex:1 1 auto;min-width:0;display:flex;flex-direction:column"><span class="mono trunc" style="font-size:13px">${esc(x.name || '（未命名）')}</span><span class="tiny faint">${esc(x.site)} · <span class="mono">${fmtSize(x.size)} · ${fmtAgo(x.updated_at)}</span></span></div>
    <span class="small" style="flex:none;display:inline-flex;align-items:center;gap:6px;white-space:nowrap;color:${v.kind === 'ok' ? 'var(--ink2)' : c}"><span class="dot" style="background:${c}"></span>${v.status}</span></div>`;
}
function heatCard(heat) {
  const max = Math.max(0, ...heat);
  const start = new Date(Date.now() - 83 * 86400000);
  const cells = heat.map((n, i) => {
    const lv = n === 0 ? 0 : n < max * 0.25 ? 1 : n < max * 0.5 ? 2 : n < max * 0.75 ? 3 : 4;
    const d = new Date(start.getTime() + i * 86400000);
    return `<div title="${i === 83 ? '今天' : `${d.getMonth() + 1}月${d.getDate()}日`} · ${n} 个" style="border-radius:3px;background:${lv ? 'var(--acc)' : 'var(--heat0)'};opacity:${[1, 0.3, 0.55, 0.8, 1][lv]}"></div>`;
  }).join('');
  return `<section class="card" style="flex:4 1 300px;min-width:0" aria-labelledby="heat-h">
    <div class="card-head"><h2 id="heat-h">最近 12 周</h2><span class="mono tiny faint">单日最多 ${fmtN(max)}</span></div>
    <div role="img" aria-label="最近 12 周每天辅种数，今天 ${heat[83] || 0} 个，单日最多 ${max} 个" class="heat">${cells}</div>
    <div class="row tiny faint" style="justify-content:space-between"><span>${start.getMonth() + 1}月${start.getDate()}日</span><span>今天</span></div>
  </section>`;
}
async function pageJobs() {
  const [j, st] = await Promise.all([api('GET', '/jobs'), api('GET', '/status')]);
  const balls = siteBalls(st.sites || []);
  let tab = 'all';
  const RUN = ['pending', 'fetching', 'verifying', 'adding', 'observing'];
  const draw = () => {
    const list = j.jobs.filter((x) => tab === 'all' || (tab === 'seeding' && x.state === 'seeding') || (tab === 'running' && RUN.includes(x.state)) || (tab === 'failed' && x.state === 'failed'));
    const tabs = [['all', '全部', j.counts.all], ['seeding', '做种中', j.counts.seeding], ['running', '进行中', j.counts.running], ['failed', '失败', j.counts.failed]];
    root.innerHTML = shell('#/jobs', `
      <div class="head"><div><h1 class="h1">辅种记录</h1><p class="muted">每一条都能看到走到了哪一步；卡住的会写明原因和下一步。</p></div></div>
      <div class="row" style="justify-content:space-between">
        <div class="seg" role="group" aria-label="按状态筛选">${tabs.map(([k, t, n]) => `<button type="button" aria-pressed="${k === tab}" data-act="tab" data-arg="${k}">${t}<span class="n">${n}</span></button>`).join('')}</div>
        <div aria-hidden="true" class="row tiny faint" style="gap:14px">${STEPS.join('<span>›</span>')}</div>
      </div>
      <section class="card" aria-label="记录列表">${list.length ? `<ul class="divide" style="list-style:none;margin:0;padding:0">${list.map((x) => jobRow(x, balls)).join('')}</ul>` : `<div class="empty">
        <div class="track" aria-hidden="true" style="justify-content:center;margin-bottom:14px"><b></b><i></i><b></b><i></i><b></b><i></i><b></b><i></i><b></b></div>
        ${tab === 'all' ? '还没有辅种记录。<br>社区里有人答对会员题、告知你种子编号后，会出现在这里。' : '这个分类下暂时没有记录。'}</div>`}</section>
      <section class="dashed row" aria-labelledby="danger-h" style="justify-content:space-between">
        <div class="grow"><h2 id="danger-h" style="font-size:15px;font-weight:600">想全部撤回？</h2><p class="muted">从下载器移除 xseed 加入的全部种子。文件不会被删除，原来的种子不受影响。</p></div>
        <button class="btn danger" data-act="removeAll">全部移除…</button>
      </section>`, { downloader: st.downloader, network: st.network });
  };
  draw();
  actions.tab = (k) => { tab = k; draw(); };
  actions.retry = async (id) => { await api('POST', `/jobs/${id}/retry`); toast('已重新排队'); pageJobs(); };
  actions.removeAll = async () => {
    if (!confirm('确定从下载器移除 xseed 加入的全部种子吗？文件不会被删除。')) return;
    const r = await api('POST', '/remove-all');
    toast(r.removed || r.skipped ? `已移除 ${r.removed} 个${r.skipped ? `，${r.skipped} 个已去掉 xseed 标签，未动` : ''}` : '目前没有 xseed 加入的种子');
    pageJobs();
  };
  if (j.counts.running) timer = setInterval(() => { if (location.hash === '#/jobs') api('GET', '/jobs').then((n) => { if (JSON.stringify(n) !== JSON.stringify(j)) { j.jobs = n.jobs; j.counts = n.counts; draw(); } }).catch(() => {}); }, 5000);
}

/* ───────── 设置 ───────── */
async function pageSettings() {
  const [c, st] = await Promise.all([api('GET', '/config'), api('GET', '/status')]);
  const d = st.downloader || {};
  const net = st.network || {};
  let add = c.daily_add_limit, ans = c.daily_answer_limit;
  const stepper = (key, label, hint, v) => `<div class="setting"><div class="grow"><b style="font-weight:600">${label}</b><div class="small faint">${hint}</div></div>
    <div class="stepper" role="group" aria-label="${label}"><button type="button" aria-label="减少" data-act="step" data-arg="${key}:-">−</button><output id="out-${key}" aria-live="polite">${v}</output><button type="button" aria-label="增加" data-act="step" data-arg="${key}:+">+</button></div></div>`;
  root.innerHTML = shell('#/settings', `
    <div class="head"><div><h1 class="h1">设置</h1><p class="muted">默认值适合大多数人，一般不用改。</p></div><button class="btn primary" data-act="save">保存更改</button></div>
    <section class="card give ${c.respond ? 'on' : ''} row" aria-labelledby="give-h" style="padding:28px;gap:20px 32px">
      <div class="grow" style="position:relative;display:flex;flex-direction:column;gap:8px;flex-basis:380px">
        <h2 id="give-h" style="font-size:22px;font-weight:700;letter-spacing:-.02em">${c.respond ? '你正在帮别人找到辅种' : '你暂时只为自己辅种'}</h2>
        <p class="muted" style="max-width:520px">别人有同样的文件、并答对了该站会员题，你的小助手才告诉他种子编号。你帮的人越多，社区里能帮你的人也越多。</p>
        <span class="mono small faint">${!c.respond ? '关掉后你能找到的辅种会变少' : net.state === 'online' ? `本周登记了 ${fmtN(net.holders)} 个持有者代号，今天回应了 ${fmtN(net.answered_today)} 次探测` : '连上辅种网络后开始生效'}</span>
      </div>
      <button type="button" class="switch big" role="switch" aria-checked="${c.respond}" aria-label="帮别人找到辅种" data-act="respond" style="position:relative"></button>
    </section>
    <section class="card divide" style="padding:4px 28px" aria-label="节奏">
      ${stepper('add', '每站每天最多加种', '单个站点可在站点页另行设置', add)}
      ${stepper('ans', '每站每天最多答题', '每答一题要从站点下载一个种子文件', ans)}
    </section>
    <section class="card divide" style="padding:4px 28px" aria-label="连接与安全">
      <div class="setting"><div class="grow"><h2>下载器</h2><span class="mono small faint">${esc((d.status || '') + ' · ' + (d.url || '') + ' · ' + (d.user || ''))}</span></div>
        <span class="status ${d.ok === false ? 'err-t' : 'ok-t'}"><span class="dot ${d.ok === false ? 'err' : 'ok'}"></span>${d.ok === false ? '连接失败' : '已连接'}</span><a class="btn sm" href="#/setup/2?edit=1">修改</a></div>
      <div class="setting"><div class="grow"><label for="entry" style="font-weight:600">入口节点</label><div class="small faint">消息都经它转发：它知道你的 IP，看不到内容。${net.entry ? `当前：<span class="mono">${esc(net.entry.host)}</span>` : ''}</div></div>
        <select id="entry" class="input" style="flex:0 1 240px;height:40px" ${(net.nodes || []).length ? '' : 'disabled'}><option value="">自动（延迟最低）</option>${(net.nodes || []).map((x) => `<option value="${esc(x.id)}" ${c.pin_entry === x.id ? 'selected' : ''}>${esc(x.host)}${x.ok ? (x.rtt_ms != null ? ' · ' + x.rtt_ms + ' ms' : '') : ' · 不可用'}</option>`).join('')}</select></div>
      <form class="setting" data-submit="registry"><div class="grow"><label for="registry" style="font-weight:600">社区节点清单</label><div class="small faint">${net.state === 'online' ? `已连接 ${(net.nodes || []).filter((x) => x.ok).length} / ${(net.nodes || []).length} 个节点` : net.state === 'error' ? `<span class="err-t">${esc(net.error || '')}</span>` : (c.official_registry ? '留空即使用官方清单；自建注册中心时填写其地址与签名公钥' : '官方清单尚未发布；可填写自建节点清单的地址')}</div></div>
        <div style="flex:0 1 300px;display:flex;flex-direction:column;gap:8px;min-width:0"><input class="input mono" id="registry" value="${esc(c.registry || '')}" placeholder="${c.official_registry ? '官方清单' : 'https://… /registry.json'}" autocomplete="off" style="height:40px">
          <input class="input mono" id="registry-keys" value="${esc((c.registry_keys || []).join(' '))}" placeholder="签名公钥 ed25519:…（可选）" aria-label="自建注册中心的签名公钥" autocomplete="off" style="height:40px"></div><button class="btn sm" type="submit">保存</button></form>
      <div class="setting"><div class="grow"><b style="font-weight:600" class="muted">加种前抽查文件</b><div class="small faint">需要给小助手映射数据目录。默认不读硬盘，直接信任下载器的做种状态。</div></div>
        <button type="button" class="switch" role="switch" aria-checked="false" aria-label="加种前抽查文件" disabled></button></div>
      <div class="setting"><div class="grow"><label for="theme" style="font-weight:600">外观</label><div class="small faint">只保存在这台浏览器里</div></div>
        <select id="theme" class="input" style="flex:0 1 240px;height:40px"><option value="dark">深色</option><option value="light">浅色</option><option value="auto">跟随系统</option></select></div>
      <form class="setting" data-submit="password"><div class="grow"><b style="font-weight:600">管理员密码</b><div class="small faint">${c.admin_changed_at ? '上次修改：' + new Date(c.admin_changed_at * 1000).toLocaleDateString('zh-CN') : '首次设置后未修改过'}</div></div>
        <label class="sr" for="pw-old">原密码</label><input class="input" id="pw-old" type="password" placeholder="原密码" autocomplete="current-password" style="flex:0 1 160px;height:40px">
        <label class="sr" for="pw-new">新密码</label><input class="input" id="pw-new" type="password" placeholder="新密码（≥8 位）" autocomplete="new-password" style="flex:0 1 180px;height:40px">
        <button class="btn sm" type="submit">修改密码</button></form>
      <div class="setting"><div class="grow"><b style="font-weight:600">退出登录</b><div class="small faint">xseed ${esc(S.session.version)}</div></div><button class="btn sm ghost" data-act="logout">退出</button></div>
    </section>`, { narrow: true, downloader: st.downloader, network: st.network });
  const sel = document.getElementById('theme'); sel.value = getTheme(); sel.addEventListener('change', () => setTheme(sel.value));
  const ent = document.getElementById('entry');
  ent.addEventListener('change', () => api('PUT', '/config', { pin_entry: ent.value }).then(() => toast(ent.value ? '已固定入口节点' : '已改为自动选择')).catch((e) => toast(e.message)));
  actions.registry = async () => { await api('PUT', '/config', { registry: val('registry'), registry_keys: val('registry-keys').split(/[\s,]+/).filter(Boolean) }); toast('已保存，正在连接…'); setTimeout(pageSettings, 1500); };
  actions.step = (arg) => {
    const [k, dir] = arg.split(':');
    if (k === 'add') { add = Math.max(10, Math.min(500, add + (dir === '+' ? 10 : -10))); document.getElementById('out-add').textContent = add; }
    else { ans = Math.max(5, Math.min(200, ans + (dir === '+' ? 5 : -5))); document.getElementById('out-ans').textContent = ans; }
  };
  actions.save = async () => { await api('PUT', '/config', { daily_add_limit: add, daily_answer_limit: ans }); toast('已保存'); };
  actions.respond = async (_, el) => { await api('PUT', '/config', { respond: el.getAttribute('aria-checked') !== 'true' }); pageSettings(); };
  actions.password = async () => { await api('POST', '/password', { old: val('pw-old'), password: val('pw-new') }); toast('密码已修改'); pageSettings(); };
  actions.logout = async () => { await api('POST', '/logout'); S.csrf = null; go('#/login'); };
}

/* ───────── 隐私说明 ───────── */
function pagePrivacy() {
  const V = [
    ['entry', '入口节点', '帮你转发消息的社区节点', '它只负责转发。消息是加密的，它连你在问哪个站都不知道。'],
    ['index', '登记节点', '保存文件指纹的社区节点', '它只看到一串指纹，而且不知道是谁登记的——IP 和指纹分在两个节点上。'],
    ['peer', '其他用户', '手里有同样文件的人', '只有答对了该站的会员题，他才知道“某个匿名用户在这个站有这个种子”。'],
    ['site', 'PT 站点', '和你平时使用一样', '站点看到的和你平时手动下载种子、开始做种完全一样，不会多出任何信息。'],
  ];
  const F = [['你的 IP', '203.0.113.24', ['entry', 'site']], ['文件指纹', '9f3a·e1b7·…·c71e', ['index', 'peer']], ['站点名', '站点 Y', ['peer', 'site']], ['种子编号', '#48211', ['peer', 'site']], ['你的站点账号', 'seeder_42', ['site']], ['你还在哪些站', '站点 Z、V、W', []], ['Cookie / passkey', 'c0ffee…', []]];
  let view = 'peer';
  const draw = () => {
    const cur = V.find((x) => x[0] === view);
    const seen = F.filter((f) => f[2].includes(view)).length;
    root.innerHTML = shell('#/privacy', `
      <div class="head"><div><h1 class="h1" style="font-size:40px;font-weight:800">换个身份，<br>看看别人能看到你什么。</h1><p class="muted">选一方，下面这份“档案”就只露出它能看到的部分，其余的全部涂黑。</p></div></div>
      <section class="card flush" aria-label="各方能看到什么"><div class="row" style="align-items:stretch;gap:0">
        <div class="who" role="radiogroup" aria-label="选择视角" style="flex:1 1 240px;padding:20px;background:var(--panel2);border-right:1px solid var(--line)">
          <span class="tiny faint" style="padding:0 8px 4px">我是…</span>
          ${V.map(([id, n, sub]) => `<button type="button" role="radio" aria-checked="${id === view}" data-act="view" data-arg="${id}"><b style="font-weight:600">${n}</b><span class="small faint">${sub}</span></button>`).join('')}
        </div>
        <div style="flex:2 1 420px;padding:28px;min-width:0;display:flex;flex-direction:column;gap:4px">
          <div class="row" style="justify-content:space-between;margin-bottom:10px"><h2 class="mono faint" style="font-size:13px;font-weight:500;letter-spacing:.04em">关于你的档案 · ${cur[1]}视角</h2><span class="mono small"><span class="acc-t">${seen}</span> / ${F.length} 项可见</span></div>
          <dl class="divide" style="margin:0">${F.map(([k, v, who], i) => { const s = who.includes(view); return `<div class="row" style="padding:12px 0;gap:6px 16px"><dt class="small muted" style="flex:0 0 120px">${k}</dt><dd style="flex:1 1 200px;margin:0;min-width:0">${s ? `<span class="mono">${esc(v)}</span>` : `<span class="redact" style="width:${90 + (i * 37) % 80}px"></span><span class="sr">看不到</span>`}</dd><span class="tiny ${s ? 'warn-t' : 'ok-t'}" style="font-weight:500">${s ? '能看到' : '看不到'}</span></div>`; }).join('')}</dl>
          <p class="small muted" style="margin-top:14px;padding:12px 14px;border-radius:10px;background:var(--panel2)">${cur[3]}</p>
        </div></div></section>
      <section class="card" aria-labelledby="how-h"><h2 id="how-h" style="margin-bottom:18px">一次辅种是怎么发生的</h2><ol class="steps4">
        ${[['01', '登记指纹', '小助手给每个种子算一个文件指纹交给社区节点。指纹只由文件名和大小算出，看不出内容。'], ['02', '加密询问', '别人也有同样文件时，经节点发来一条加密消息：“你在 Y 站有这个吗？”'], ['03', '会员答题', '你的小助手出一道只有 Y 站会员才答得上的题，答对了才告诉他种子编号。'], ['04', '自己下载', '他用自己的账号去 Y 站下载种子、加进下载器做种。你找辅种也一样。']].map(([n, t, b]) => `<li><span class="mono tiny faint">${n}</span><b style="font-size:15px;font-weight:650">${t}</b><span class="small muted">${b}</span></li>`).join('')}
      </ol></section>
      <section class="dashed" aria-labelledby="never-h"><h2 id="never-h" class="h2" style="margin-bottom:14px">xseed 承诺不做的事</h2><ul style="list-style:none;margin:0;padding:0;display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:10px 28px">
        ${['不删除、不移动你的任何文件，不改动原有的种子', '不把 Cookie、passkey 发给任何人，只加密保存在本机', '不在站长明确不允许的站点上运行', '不读取硬盘上的文件内容（除非你手动开启抽查）'].map((t) => `<li class="row" style="gap:10px;flex-wrap:nowrap;align-items:baseline"><span class="mono acc-t" aria-hidden="true">✕</span><span>${t}</span></li>`).join('')}
      </ul></section>`);
  };
  draw();
  actions.view = (id) => { view = id; draw(); };
}

/* ───────── 登录 ───────── */
function pageLogin() {
  root.innerHTML = `<div class="wiz"><aside class="stage"><div class="logo" style="padding:0;color:var(--ink)">${I.logo}<b>xseed</b></div>
      ${orbit({ size: 300, planets: [], caption: '' })}<p class="muted small" style="text-align:center">去中心化辅种小助手</p></aside>
    <main class="wmain"><div>
      <h1 class="wh">欢迎回来。</h1>
      <form class="field" data-submit="login" style="gap:14px">
        <div class="field"><label for="pw">管理员密码</label><input class="input" id="pw" type="password" autocomplete="current-password" required autofocus></div>
        <div><button class="btn primary lg" type="submit">登录 →</button></div>
        <p class="small faint">忘记密码：在小助手所在设备上运行 <code class="mono">xseed-agent reset-password</code>。</p>
      </form></div></main></div>`;
  actions.login = async () => { await api('POST', '/login', { password: val('pw') }); go('#/'); };
}

/* ───────── 向导 ───────── */
const WSTEPS = ['设置密码', '连接下载器', '确认站点', '试运行'];
function wizard(n, stage, body) {
  const steps = WSTEPS.map((t, i) => `<li${i + 1 === n ? ' aria-current="step"' : ''}><span aria-hidden="true">${i + 1 < n ? '✓' : '0' + (i + 1)}</span>${t}</li>`).join('');
  return `<div class="wiz"><aside class="stage"><div class="logo" style="padding:0;color:var(--ink)">${I.logo}<b>xseed</b></div>${stage}<ol class="wsteps" aria-label="设置进度">${steps}</ol></aside>
    <main class="wmain"><div><span class="mono small faint">0${n} / 04</span>${body}</div></main></div>`;
}

function setupAdmin() {
  if (S.session.has_admin) return go('#/setup/2');
  root.innerHTML = wizard(1, orbit({ size: 300, planets: [], caption: '你的星轨从这里开始' }), `
    <h1 class="wh">先给小助手设一个密码。</h1>
    <p class="muted">以后打开这个页面都要用它登录。小助手会保存你的站点登录信息，请不要用太简单的密码。</p>
    <form data-submit="admin" style="display:flex;flex-direction:column;gap:14px">
      <div class="fields-2"><div class="field"><label for="p1">管理员密码</label><input class="input" id="p1" type="password" minlength="8" autocomplete="new-password" required></div>
      <div class="field"><label for="p2">再输一次</label><input class="input" id="p2" type="password" minlength="8" autocomplete="new-password" required></div></div>
      <span class="hint">至少 8 位</span>
      <div class="wfoot"><span></span><button class="btn primary lg" type="submit">下一步：连接下载器 →</button></div>
    </form>`);
  actions.admin = async () => {
    if (val('p1') !== val('p2')) return toast('两次输入的密码不一样');
    await api('POST', '/setup/admin', { password: val('p1') });
    go('#/setup/2');
  };
}

async function setupDownloader() {
  const edit = location.hash.includes('edit=1');
  const st = S.session.has_downloader ? await api('GET', '/status') : {};
  const cur = st.downloader || {};
  let kind = cur.kind || 'qbittorrent';
  let tested = null;
  const draw = (found) => {
    const short = kind === 'qbittorrent' ? 'qB' : 'Tr';
    const stage = `<div class="orbit" role="img" aria-label="正在把下载器接入 xseed" style="width:300px;height:300px">
        <div class="ring" style="left:20px;top:20px;width:260px;height:260px;animation:xs-spin 40s linear infinite"></div>
        <div class="ring in" style="left:80px;top:80px;width:140px;height:140px"></div>
        <div class="core" style="left:104px;top:104px;width:92px;height:92px"><span style="font-size:12.5px;font-weight:600">你的 NAS</span></div>
        <div style="position:absolute;left:149px;top:196px;width:2px;height:44px;background-image:repeating-linear-gradient(180deg,var(--acc) 0 6px,transparent 6px 12px)"></div>
        <div style="position:absolute;left:116px;top:240px;width:68px;height:30px;border-radius:8px;background:var(--acc);color:var(--on-acc);display:flex;align-items:center;justify-content:center;font-family:var(--mono);font-size:12.5px;font-weight:600">${short}</div>
        <span class="caption">站点稍后出现在这条轨道上</span></div>`;
    const ch = (k, s, n, v) => `<label class="${kind === k ? 'on' : ''}"><input type="radio" name="kind" value="${k}" ${kind === k ? 'checked' : ''} data-act="kind" data-arg="${k}"><span class="lg">${s}</span><span style="display:flex;flex-direction:column"><b style="font-weight:600">${n}</b><span class="small faint">${v}</span></span></label>`;
    const url = val('url') || cur.url || (found && found.kind === kind ? found.url : (kind === 'qbittorrent' ? 'http://localhost:8080' : 'http://localhost:9091'));
    const user = val('user') || cur.user || (kind === 'qbittorrent' ? 'admin' : '');
    root.innerHTML = wizard(2, stage, `
      <h1 class="wh">${edit ? '修改下载器。' : '先把下载器接进来。'}</h1>
      <p class="muted">小助手只用下载器的网页接口，不需要挂载你的数据目录。</p>
      ${found ? `<p class="banner note" style="margin:0"><span class="dot acc"></span><span>在 <b class="mono" style="font-weight:500;color:var(--ink)">${esc(found.url)}</b> 找到了 ${found.kind === 'qbittorrent' ? 'qBittorrent' : 'Transmission'}，地址已经帮你填好。</span></p>` : ''}
      <form data-submit="saveDl" style="display:flex;flex-direction:column;gap:18px">
        <fieldset style="border:0;margin:0;padding:0"><legend class="label" style="margin-bottom:8px">下载器</legend><div class="choice">${ch('qbittorrent', 'qB', 'qBittorrent', '4.x / 5.x')}${ch('transmission', 'Tr', 'Transmission', '3.x / 4.x')}</div></fieldset>
        <div class="field"><label for="url">网页界面地址</label><input class="input mono" id="url" type="url" value="${esc(url)}" required><span class="hint">就是你在浏览器里打开下载器网页界面用的地址</span></div>
        <div class="fields-2"><div class="field"><label for="user">用户名</label><input class="input" id="user" value="${esc(user)}" autocomplete="off"></div>
          <div class="field"><label for="pass">密码</label><input class="input" id="pass" type="password" autocomplete="off" placeholder="${edit ? '留空表示不修改' : ''}"></div></div>
        <div id="test-result">${tested ? testHtml(tested) : ''}</div>
        <div class="wfoot"><button type="button" class="btn" data-act="test">测试连接</button><button class="btn primary lg" type="submit">${edit ? '保存' : '下一步：确认站点 →'}</button></div>
      </form>`);
  };
  const testHtml = (r) => r.ok
    ? `<div class="row card" style="padding:14px 16px;border-radius:12px;gap:10px 16px"><span style="flex:none;width:28px;height:28px;border-radius:50%;background:var(--acc);color:var(--on-acc);display:inline-flex;align-items:center;justify-content:center">${I.check}</span><div class="grow" style="display:flex;flex-direction:column"><b style="font-weight:600">已连上 ${esc(r.version)}</b><span class="small muted">发现 <span class="mono" style="color:var(--ink)">${fmtN(r.completed)}</span> 个已完成的种子</span></div></div>`
    : `<div class="banner err" style="margin:0">${I.warn}<p><b>连不上：</b><span>${esc(r.error)}</span></p></div>`;
  const body = () => ({ kind, url: val('url'), user: val('user'), password: val('pass') });
  draw(null);
  if (!edit && !cur.url) api('GET', '/setup/discover').then((r) => { if (r.found) { kind = r.found.kind; draw(r.found); } }).catch(() => {});
  actions.kind = (k) => { kind = k; tested = null; draw(null); };
  actions.test = async () => {
    document.getElementById('test-result').innerHTML = '<p class="small muted">正在连接…</p>';
    tested = await api('POST', '/setup/downloader/test', body());
    document.getElementById('test-result').innerHTML = testHtml(tested);
  };
  actions.saveDl = async () => {
    const r = await api('POST', '/setup/downloader/test', body());
    if (!r.ok && !(edit && !val('pass'))) { tested = r; document.getElementById('test-result').innerHTML = testHtml(r); return; }
    await api('POST', '/setup/downloader', body());
    S.session.has_downloader = true;
    go(edit ? '#/settings' : '#/setup/3');
  };
}

async function setupSites() {
  const scan = await api('GET', '/scan');
  if (scan.running) {
    root.innerHTML = wizard(3, orbit({ size: 300, planets: [], caption: '正在认出你的站点…' }), `
      <h1 class="wh">正在读取你的做种列表。</h1>
      <p class="muted">只读取种子的文件名和大小，不读硬盘上的文件。种子多的话需要一两分钟。</p>
      <div class="bar" style="height:8px"><i style="width:${scan.total ? Math.round(scan.done / scan.total * 100) : 3}%"></i></div>
      <p class="mono small muted">${fmtN(scan.done)} / ${fmtN(scan.total || 0)}</p>`);
    timer = setTimeout(() => { if (location.hash.startsWith('#/setup/3')) setupSites(); }, 1500);
    return;
  }
  const { sites } = await api('GET', '/sites');
  const draw = () => {
    const picked = sites.filter((s) => s.enabled && s.status !== 'denied');
    const stage = orbit({ size: 300, planets: sitePlanets(sites), label: '星轨上已接入：' + (picked.map((s) => s.name).join('、') || '暂无站点') });
    root.innerHTML = wizard(3, stage, `
      <h1 class="wh">${sites.length ? '挑出要加入星轨的站点。' : '没有认出任何站点。'}</h1>
      <p class="muted">${sites.length ? `从你的做种列表里认出了 ${sites.length} 个站。大部分的登录信息已经从 tracker 地址里自动取得。` : '下载器里需要有已完成的 PT 种子。也可以先从 PT-depiler 同步站点信息，再回来这一步。'}</p>
      ${scan.error ? `<div class="banner err" style="margin:0">${I.warn}<p><b>读取下载器失败：</b><span>${esc(scan.error)}</span></p></div>` : ''}
      <section aria-label="站点" class="pick">${sites.map((s) => {
        const locked = s.status === 'denied';
        const note = locked ? '站长不允许，不会参与' : s.status === 'need_cookie' ? '需要 Cookie，可在站点页用 PT-depiler 同步' : (s.p2p === 'unknown' ? '可直接用 · 站点未表态' : '可直接用');
        const on = s.enabled && !locked;
        return `<label class="${on ? 'on' : ''}${locked ? ' locked' : ''}"><input type="checkbox" ${on ? 'checked' : ''} ${locked ? 'disabled' : ''} data-act="pick" data-arg="${esc(s.site_ref)}">
          <span style="flex:1 1 auto;min-width:0;display:flex;flex-direction:column;gap:2px"><span class="row" style="justify-content:space-between;gap:8px;flex-wrap:nowrap"><b style="font-weight:600" class="trunc">${esc(s.name)}</b><span class="mono small faint">${fmtN(s.torrents)}</span></span>
          <span class="small ${locked ? 'err-t' : s.status === 'need_cookie' ? 'warn-t' : 'faint'}">${note}</span></span></label>`;
      }).join('')}</section>
      <div class="wfoot"><a class="btn ghost" href="#/setup/2" style="border:0">← 上一步</a>
        <div class="row" style="gap:16px"><span class="small muted">已选 <b class="mono" style="color:var(--ink)">${picked.length}</b> 个站</span>
        <button class="btn ghost sm" data-act="rescan">${I.refresh}重新读取</button><a class="btn primary lg" href="#/setup/4">下一步：试运行 →</a></div></div>`);
  };
  draw();
  actions.pick = async (ref, el) => {
    const s = sites.find((x) => x.site_ref === ref);
    const next = !(s.enabled);
    await api('PUT', '/sites/' + encodeURIComponent(ref), { enabled: next });
    s.enabled = next;
    draw();
  };
  actions.rescan = async () => { await api('POST', '/scan'); setTimeout(setupSites, 400); };
}

async function setupDryRun() {
  const r = await api('GET', '/dry-run');
  const s = r.stats;
  const on = r.sites.filter((x) => x.enabled && x.status !== 'denied');
  const stage = orbit({ size: 300, planets: sitePlanets(r.sites), core: [fmtN(s.holders), '可回答'], coreAcc: true, beam: 'live', label: '参与辅种的站点：' + on.map((x) => x.name).join('、') });
  root.innerHTML = wizard(4, stage, `
    <span class="mono small faint" style="margin-top:-14px">试运行，什么都还没加</span>
    <h1 class="wh">你的硬盘里，有 <span class="mono acc-t" style="font-weight:600">${fmtSize(s.total_size)}</span> 的内容已经准备好加入星轨。</h1>
    <dl class="kv3" style="border:1px solid var(--line);border-radius:14px;gap:1px;background:var(--line);overflow:hidden;padding:0;margin:0">
      ${[['本机种子', fmtN(s.ok)], ['参与站点', on.length + ' 站'], ['额外占用', '0 B']].map(([k, v]) => `<div style="background:var(--panel);padding:16px 18px"><dt>${k}</dt><dd style="font-size:26px;letter-spacing:-.03em">${v}</dd></div>`).join('')}
    </dl>
    <p class="banner note" style="margin:0"><span class="dot"></span><span>${r.network && r.network.state === 'no_registry' ? '还没有配置社区节点。点亮星轨后，可以在“设置”里填写节点清单。' : '点亮星轨后，小助手会连上辅种网络：登记你的文件指纹，并找出社区里有同款的人。社区里有人答对会员题、告知种子编号后，会自动下载种子、核对并加入下载器。'}</span></p>
    <ul style="list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:12px">
      ${['新种子直接指向你已有的文件——<b>不复制、不移动、不删除</b>', '每站每天最多加 50 个，启动时间随机错开', '都带 <code class="mono" style="padding:1px 6px;border-radius:4px;background:var(--panel2)">xseed</code> 标签，随时可以在“辅种记录”里一键撤回'].map((t, i) => `<li class="row" style="gap:12px;flex-wrap:nowrap;align-items:baseline"><span class="mono tiny faint" aria-hidden="true">0${i + 1}</span><span>${t}</span></li>`).join('')}
    </ul>
    <div class="wfoot"><a class="btn ghost" href="#/setup/3" style="border:0">← 上一步</a><button class="btn primary xl" data-act="confirm">点亮星轨，开始 →</button></div>`);
  actions.confirm = async () => { await api('POST', '/dry-run/confirm'); S.session.setup_done = true; go('#/'); };
}

route();
