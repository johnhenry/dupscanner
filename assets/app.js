/* dupscanner web UI. Plain JS, no dependencies. All data from the server is
   rendered through textContent / DOM construction, never innerHTML.

   The keyboard model mirrors the terminal UI (src/tui.rs, src/app.rs): a
   cursor moves over file rows, single letters mark, clear and delete, and a
   footer bar shows either the key hints or the last status message. */
(function () {
  "use strict";

  // ---- storage helpers --------------------------------------------------

  const STORAGE_THEME = "dupscanner.theme";
  const STORAGE_FILTERS = "dupscanner.filters";
  const STORAGE_STATS = "dupscanner.stats";

  function loadJSON(key, fallback) {
    try {
      const raw = localStorage.getItem(key);
      return raw ? Object.assign({}, fallback, JSON.parse(raw)) : Object.assign({}, fallback);
    } catch (_) {
      return Object.assign({}, fallback);
    }
  }

  function saveJSON(key, value) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch (_) {
      /* storage unavailable; ignore */
    }
  }

  // ---- state ------------------------------------------------------------

  const state = {
    filters: loadJSON(STORAGE_FILTERS, { path: "", size: "all", type: "all", limit: 25 }),
    offset: 0,
    total: 0,
    version: -1,
    groups: [],
    scan: null,
    /** path -> { hash, size }; hash/size are null until hydrated for
        files that were marked while off-page (scope "all"). */
    marks: new Map(),
    /** path -> { hash, size } for every file on the current page */
    pathIndex: new Map(),
    /** path -> { group, file, host, row } for rows on the current page */
    rowRefs: new Map(),
    /** hashes of collapsed groups */
    collapsed: new Set(),
    defaultCollapsed: false,
    fetching: false,
    refetchQueued: false,
    stopped: false,
    /** keyboard cursor: { hash, path } or null */
    cursor: null,
    cursorGroupIndex: 0,
    /** "page" or "all": scope of the Auto-select dropdown */
    selectScope: "page",
    statsOpen: loadJSON(STORAGE_STATS, { open: false }).open === true,
    /** footer status message (replaces the key hints), like the TUI footer */
    status: null,
    /** which modal is open: "confirm" | "markmenu" | "help" | "preview" | "other" | null */
    modalKind: null,
    /** optional keydown handler for the open modal; returns true when handled */
    modalKeys: null,
    /** host element of the open inline rename form, if any */
    activeRename: null,
    hydrating: false,
  };

  const KEY_HINTS = "j/k file · n/p group · Space mark · a/A suggested · o/O keeper · m more · d/D delete · ? help";

  /** Used until /api/scan answers with the server's list. */
  const FALLBACK_MODES = [
    { key: "suggested", label: "Mark suggested copies", shortcut: "s" },
    { key: "allButKeeper", label: "Mark all but keeper", shortcut: "k" },
    { key: "allButOldest", label: "Mark all but oldest", shortcut: "o" },
    { key: "allButNewest", label: "Mark all but newest", shortcut: "n" },
    { key: "allButShortestPath", label: "Mark all but shortest path", shortcut: "h" },
    { key: "allButLongestPath", label: "Mark all but longest path", shortcut: "l" },
  ];

  const REASONS = {
    InTempDirectory: "in a temp directory",
    HasCopyInName: "name looks like a copy",
    InDownloadsDirectory: "in Downloads",
    InBackupDirectory: "in a backup folder",
    DeeperPath: "deeper in the tree",
    LongerFilename: "longer filename",
    PreferredLocation: "in a preferred location",
  };

  const IMAGE_EXT = ["jpg", "jpeg", "png", "gif", "bmp", "webp", "avif", "ico", "tif", "tiff", "heic"];
  const VIDEO_EXT = ["mp4", "webm", "mov", "m4v", "ogv", "mpg", "mpeg"];
  const AUDIO_EXT = ["mp3", "wav", "ogg", "oga", "m4a", "aac", "flac", "opus", "aiff", "aif"];

  /** Paths listed in the confirmation dialog before "... and N more". */
  const CONFIRM_LIST_CAP = 200;

  // ---- DOM helpers ------------------------------------------------------

  const $ = (id) => document.getElementById(id);

  function el(tag, attrs, children) {
    const node = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v === null || v === undefined || v === false) continue;
        if (k === "class") node.className = v;
        else if (k === "text") node.textContent = v;
        else if (k === "html") throw new Error("innerHTML is not allowed");
        else if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2), v);
        else if (k === "disabled" || k === "checked" || k === "hidden") node[k] = Boolean(v);
        else node.setAttribute(k, v);
      }
    }
    if (children !== undefined) {
      for (const child of [].concat(children)) {
        if (child === null || child === undefined || child === false) continue;
        node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
      }
    }
    return node;
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  function formatBytes(n) {
    n = Number(n) || 0;
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) {
      n /= 1024;
      i++;
    }
    return (i === 0 ? String(n) : n.toFixed(n >= 100 ? 0 : 1)) + " " + units[i];
  }

  function formatCount(n) {
    return Number(n || 0).toLocaleString();
  }

  function formatDuration(seconds) {
    seconds = Math.floor(Number(seconds) || 0);
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    if (h > 0) return h + "h " + m + "m " + s + "s";
    if (m > 0) return m + "m " + s + "s";
    return s + "s";
  }

  function formatDate(iso) {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "unknown date";
    return d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
  }

  function plural(n, word) {
    return n + " " + word + (n === 1 ? "" : "s");
  }

  function baseName(path) {
    const i = path.lastIndexOf("/");
    return i >= 0 ? path.slice(i + 1) : path;
  }

  function dirName(path) {
    const i = path.lastIndexOf("/");
    return i >= 0 ? path.slice(0, i + 1) : "";
  }

  function extension(path) {
    const name = baseName(path);
    const i = name.lastIndexOf(".");
    return i > 0 ? name.slice(i + 1).toLowerCase() : "";
  }

  function fileUrl(path, download) {
    return "/api/file?path=" + encodeURIComponent(path) + (download ? "&download=1" : "");
  }

  // ---- toasts and the footer key bar -----------------------------------

  function toast(message, kind, ttl) {
    const node = el("div", { class: "toast " + (kind || ""), role: "status", text: message });
    $("toasts").appendChild(node);
    setTimeout(() => node.remove(), ttl || (kind === "error" ? 8000 : 4500));
  }

  /** Show a status message in the key bar, as the TUI footer does. */
  function setStatus(message) {
    state.status = message || null;
    renderKeybar();
  }

  function clearStatus() {
    if (state.status !== null) setStatus(null);
  }

  /** Status bar plus a transient toast, for actions triggered by mouse or key. */
  function announce(message, kind) {
    setStatus(message);
    toast(message, kind);
  }

  function renderKeybar() {
    const bar = $("keybar");
    const text = $("keybarText");
    const marks = $("keybarMarks");
    if (state.status) {
      bar.classList.add("has-status");
      text.textContent = state.status;
      marks.textContent = "";
      return;
    }
    bar.classList.remove("has-status");
    text.textContent = KEY_HINTS;
    const { count, bytes } = markedTotals();
    const method = state.scan ? state.scan.delete_method : "";
    marks.textContent = "[" + count + " marked, " + formatBytes(bytes) + (method ? ", via " + method : "") + "]";
  }

  // ---- fetch helpers ----------------------------------------------------

  async function api(url, options) {
    const res = await fetch(url, options);
    let data = null;
    const ct = res.headers.get("content-type") || "";
    if (ct.includes("application/json")) {
      data = await res.json().catch(() => null);
    }
    if (!res.ok) {
      const msg = (data && data.error) || res.status + " " + res.statusText;
      const err = new Error(msg);
      err.status = res.status;
      err.data = data;
      throw err;
    }
    return data;
  }

  function postJSON(url, body) {
    return api(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
  }

  // ---- theme ------------------------------------------------------------

  function applyTheme(theme) {
    document.documentElement.setAttribute("data-theme", theme);
    $("themeToggle").textContent = theme === "dark" ? "Light" : "Dark";
  }

  function initTheme() {
    let theme = null;
    try {
      theme = localStorage.getItem(STORAGE_THEME);
    } catch (_) {
      /* ignore */
    }
    if (theme !== "dark" && theme !== "light") {
      theme = window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
    }
    applyTheme(theme);
    $("themeToggle").addEventListener("click", () => {
      const next = document.documentElement.getAttribute("data-theme") === "dark" ? "light" : "dark";
      applyTheme(next);
      try {
        localStorage.setItem(STORAGE_THEME, next);
      } catch (_) {
        /* ignore */
      }
    });
  }

  // ---- header -----------------------------------------------------------

  function selectModes() {
    return state.scan && Array.isArray(state.scan.select_modes) && state.scan.select_modes.length
      ? state.scan.select_modes
      : FALLBACK_MODES;
  }

  function modeByKey(key) {
    return selectModes().find((m) => m.key === key) || null;
  }

  let modesPopulated = false;

  function populateAutoSelect() {
    const select = $("autoSelect");
    const keep = select.value;
    clear(select);
    select.appendChild(el("option", { value: "", text: "Choose an action" }));
    for (const m of selectModes()) {
      select.appendChild(el("option", { value: m.key, text: m.label + " (" + m.shortcut + ")" }));
    }
    select.appendChild(el("option", { value: "clear", text: "Clear marks (this page)" }));
    select.value = keep;
    if (select.value !== keep) select.value = "";
  }

  function renderScan(scan) {
    if (!scan) return;
    state.scan = scan;
    if (!modesPopulated && Array.isArray(scan.select_modes) && scan.select_modes.length) {
      modesPopulated = true;
      populateAutoSelect();
    }
    const root = $("rootPath");
    root.textContent = scan.root;
    root.title = scan.root;
    $("statFiles").textContent = formatCount(scan.progress.files_scanned);
    $("statGroups").textContent = formatCount(scan.group_count);
    $("statDupes").textContent = formatCount(scan.duplicate_files);
    $("statWasted").textContent = formatBytes(scan.wasted_space);
    $("statElapsed").textContent = formatDuration(scan.elapsed_seconds);
    const badge = $("statusBadge");
    if (scan.complete) {
      badge.textContent = "Complete";
      badge.className = "badge badge-complete";
      $("scanBar").hidden = true;
    } else {
      badge.textContent = "Scanning";
      badge.className = "badge badge-scanning";
      $("scanBar").hidden = false;
      $("scanBarText").textContent =
        "Scanning: " +
        formatCount(scan.progress.files_scanned) +
        " files, " +
        formatBytes(scan.progress.bytes_scanned) +
        " read, " +
        formatCount(scan.group_count) +
        " duplicate groups so far" +
        (scan.progress.unreadable ? ", " + formatCount(scan.progress.unreadable) + " unreadable" : "");
    }
    $("footerMethod").textContent =
      "Delete method: " +
      scan.delete_method_description +
      (scan.scan_id !== null && scan.scan_id !== undefined ? ". Recorded as scan #" + scan.scan_id + "." : ".");
    renderKeybar();
    renderStats();
  }

  async function refreshScan() {
    try {
      renderScan(await api("/api/scan"));
    } catch (e) {
      toast("Could not load scan status: " + e.message, "error");
    }
  }

  // ---- statistics panel (mirrors the TUI Statistics view) --------------

  function renderStats() {
    const body = $("statsBody");
    if (!state.statsOpen) return;
    clear(body);
    const s = state.scan;
    if (!s) {
      body.appendChild(el("p", { class: "muted", text: "Waiting for the scan status..." }));
      return;
    }
    const elapsed = Number(s.elapsed_seconds) || 0;
    const files = Number(s.progress.files_scanned) || 0;
    const rate = elapsed > 0 ? files / elapsed : 0;
    const { count, bytes } = markedTotals();

    const section = (title, rows) => {
      const dl = el("dl", { class: "stats-list" });
      for (const [label, value, cls] of rows) {
        dl.appendChild(el("dt", { text: label }));
        dl.appendChild(el("dd", { class: cls || null, text: value }));
      }
      return el("div", { class: "stats-section" }, [el("h3", { text: title }), dl]);
    };

    body.appendChild(
      section("Scan", [
        ["Location", s.root],
        ["Status", s.complete ? "Complete" : "In progress", s.complete ? "ok" : "warn"],
        ["Elapsed", elapsed.toFixed(1) + "s (" + rate.toFixed(0) + " files/s)"],
        ["Files scanned", formatCount(files), "ok"],
        ["Bytes scanned", formatBytes(s.progress.bytes_scanned)],
        ["Unreadable", formatCount(s.progress.unreadable), "muted"],
      ])
    );
    body.appendChild(
      section("Duplicates", [
        ["Groups", formatCount(s.group_count), "warn"],
        ["Duplicate files", formatCount(s.duplicate_files), "warn"],
        ["Wasted space", formatBytes(s.wasted_space), "bad"],
        ["Marked", count + " files, " + formatBytes(bytes), "accent"],
        ["Deleted this run", formatCount(s.deleted_this_run) + " files, " + formatBytes(s.bytes_freed_this_run), "ok"],
      ])
    );
    body.appendChild(
      section("Safety", [
        ["Delete method", s.delete_method_description || s.delete_method || "unknown"],
        ["Database", s.db_path ? s.db_path : "not recorded"],
      ])
    );
    if (s.settings) {
      body.appendChild(
        section("Settings", [
          ["Min size", formatBytes(s.settings.min_size)],
          ["Max size", s.settings.max_size === null || s.settings.max_size === undefined ? "unlimited" : formatBytes(s.settings.max_size)],
          ["Exclusions", (Array.isArray(s.settings.exclusions) ? s.settings.exclusions.length : 0) + " patterns"],
        ])
      );
    }
  }

  function setStatsOpen(open) {
    state.statsOpen = Boolean(open);
    $("statsPanel").hidden = !state.statsOpen;
    $("statsToggle").setAttribute("aria-expanded", String(state.statsOpen));
    $("statsToggle").classList.toggle("active", state.statsOpen);
    saveJSON(STORAGE_STATS, { open: state.statsOpen });
    renderStats();
  }

  function toggleStats() {
    setStatsOpen(!state.statsOpen);
  }

  // ---- groups -----------------------------------------------------------

  function filterParams(p) {
    if (state.filters.path) p.set("path", state.filters.path);
    if (state.filters.size !== "all") p.set("size", state.filters.size);
    if (state.filters.type !== "all") p.set("type", state.filters.type);
    return p;
  }

  function groupsUrl() {
    const p = new URLSearchParams();
    p.set("offset", String(state.offset));
    p.set("limit", String(state.filters.limit));
    filterParams(p);
    return "/api/groups?" + p.toString();
  }

  async function fetchGroups() {
    if (state.fetching) {
      state.refetchQueued = true;
      return;
    }
    state.fetching = true;
    try {
      let data = await api(groupsUrl());
      // The page may have emptied out (deletions shrink the list); step back.
      if (data.groups.length === 0 && data.total > 0 && state.offset >= data.total) {
        state.offset = Math.max(0, Math.floor((data.total - 1) / state.filters.limit) * state.filters.limit);
        data = await api(groupsUrl());
      }
      state.groups = data.groups;
      state.total = data.total;
      state.version = data.version;
      rebuildPathIndex();
      reconcileMarks();
      reconcileCursor();
      renderGroups();
      saveFilters();
    } catch (e) {
      toast("Could not load groups: " + e.message, "error");
    } finally {
      state.fetching = false;
      if (state.refetchQueued) {
        state.refetchQueued = false;
        fetchGroups();
      }
    }
  }

  function rebuildPathIndex() {
    state.pathIndex = new Map();
    for (const g of state.groups) {
      for (const f of g.files) state.pathIndex.set(f.path, { hash: g.hash, size: g.file_size });
    }
  }

  function knownInfo(path) {
    return state.pathIndex.get(path) || { hash: null, size: null };
  }

  /** Drop marks that point at paths no longer present in groups we can see,
      and fill in hash/size for marks made while the file was off-page. */
  function reconcileMarks() {
    const present = new Map();
    for (const g of state.groups) present.set(g.hash, new Set(g.files.map((f) => f.path)));
    for (const [path, info] of Array.from(state.marks.entries())) {
      if (!info.hash) {
        const known = state.pathIndex.get(path);
        if (known) state.marks.set(path, known);
        continue;
      }
      const paths = present.get(info.hash);
      if (paths && !paths.has(path)) state.marks.delete(path);
    }
  }

  /** Fetch hash and size for marks made off-page (scope "all"), so totals
      and the confirmation dialog are exact. Pages through the filtered list. */
  async function hydrateMarks() {
    const need = new Set();
    for (const [path, info] of state.marks) if (!info.hash) need.add(path);
    if (need.size === 0 || state.hydrating) return;
    state.hydrating = true;
    try {
      let offset = 0;
      for (let guard = 0; guard < 200 && need.size > 0; guard++) {
        const p = new URLSearchParams();
        p.set("offset", String(offset));
        p.set("limit", "500");
        filterParams(p);
        const data = await api("/api/groups?" + p.toString());
        for (const g of data.groups) {
          for (const f of g.files) {
            if (need.has(f.path)) {
              state.marks.set(f.path, { hash: g.hash, size: g.file_size });
              need.delete(f.path);
            }
          }
        }
        offset += data.groups.length;
        if (data.groups.length === 0 || offset >= data.total) break;
      }
    } catch (_) {
      /* totals stay approximate until the next successful fetch */
    } finally {
      state.hydrating = false;
      refreshMarkViews();
    }
  }

  function markedTotals() {
    let bytes = 0;
    for (const info of state.marks.values()) bytes += Number(info.size) || 0;
    return { count: state.marks.size, bytes };
  }

  function markedInGroup(group) {
    return group.files.filter((f) => state.marks.has(f.path));
  }

  function updateActionButtons() {
    const { count, bytes } = markedTotals();
    const del = $("deleteSelected");
    del.disabled = count === 0;
    del.textContent = count === 0 ? "Delete marked" : "Delete " + count + " marked (" + formatBytes(bytes) + ")";
    const ren = $("batchRename");
    ren.disabled = count === 0;
    ren.textContent = count === 0 ? "Batch rename" : "Batch rename " + count;
    $("clearAllMarks").disabled = count === 0;
  }

  /** Everything that shows mark counts: summary, buttons, key bar, stats. */
  function refreshMarkViews() {
    renderSummary();
    updateActionButtons();
    renderKeybar();
    renderStats();
  }

  function isCollapsed(hash) {
    return state.collapsed.has(hash) ? !state.defaultCollapsed : state.defaultCollapsed;
  }

  function toggleCollapsed(hash) {
    if (state.collapsed.has(hash)) state.collapsed.delete(hash);
    else state.collapsed.add(hash);
  }

  function renderSummary() {
    const summary = $("summary");
    clear(summary);
    const from = state.total === 0 ? 0 : state.offset + 1;
    const to = Math.min(state.offset + state.groups.length, state.total);
    const filtered =
      state.filters.path || state.filters.size !== "all" || state.filters.type !== "all" ? " (filtered)" : "";
    summary.appendChild(
      el("span", { text: "Showing " + from + " to " + to + " of " + formatCount(state.total) + " groups" + filtered })
    );
    const { count, bytes } = markedTotals();
    summary.appendChild(
      el("span", {
        text: count === 0 ? "No files marked" : count + " marked for deletion, " + formatBytes(bytes),
      })
    );
  }

  function renderGroups() {
    const container = $("groups");
    const scrollY = window.scrollY;
    clear(container);
    state.rowRefs = new Map();
    state.activeRename = null;

    if (state.groups.length === 0) {
      const scanning = state.scan && !state.scan.complete;
      container.appendChild(
        el("div", {
          class: "empty",
          text: scanning
            ? "No duplicate groups yet. Results appear as the scan runs."
            : state.total === 0 && (state.filters.path || state.filters.size !== "all" || state.filters.type !== "all")
            ? "No groups match the current filters."
            : "No duplicates found.",
        })
      );
    } else {
      state.groups.forEach((group, i) => container.appendChild(renderGroup(group, state.offset + i + 1)));
    }
    refreshMarkViews();
    renderPagination();
    window.scrollTo(0, scrollY);
  }

  function renderGroup(group, index) {
    const markedHere = markedInGroup(group).length;
    const isCursorGroup = state.cursor && state.cursor.hash === group.hash;
    const card = el("div", {
      class: "group" + (isCollapsed(group.hash) ? " collapsed" : "") + (isCursorGroup ? " cursor-group" : ""),
      "data-hash": group.hash,
    });

    const head = el("div", { class: "group-head", role: "button", tabindex: "0", "aria-expanded": String(!isCollapsed(group.hash)) }, [
      el("span", { class: "chevron", "aria-hidden": "true" }),
      el("span", { class: "group-index", text: "#" + index }),
      el("span", { class: "group-title", text: group.file_count + " files x " + formatBytes(group.file_size) }),
      markedHere ? el("span", { class: "group-marked", text: markedHere + " marked" }) : null,
      el("span", { class: "group-meta" }, [
        el("span", {}, ["wasted ", el("span", { class: "wasted", text: formatBytes(group.wasted_space) })]),
        el("code", { text: group.hash.slice(0, 12), title: group.hash }),
      ]),
    ]);
    const toggle = () => {
      toggleCollapsed(group.hash);
      card.classList.toggle("collapsed");
      head.setAttribute("aria-expanded", String(!card.classList.contains("collapsed")));
    };
    head.addEventListener("click", toggle);
    head.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        e.stopPropagation();
        toggle();
      }
    });
    card.appendChild(head);

    // Per-group actions, the mouse counterpart of a / o / c / d.
    const stop = (fn) => (e) => {
      e.stopPropagation();
      fn();
    };
    const actions = el("div", { class: "group-actions" }, [
      el("button", { type: "button", class: "btn btn-small", text: "Mark suggested", title: "a", onclick: stop(() => runGroupSelect("suggested", group)) }),
      el("button", { type: "button", class: "btn btn-small", text: "Mark all but keeper", title: "o", onclick: stop(() => runGroupSelect("allButKeeper", group)) }),
      el("button", { type: "button", class: "btn btn-small", text: "Clear", title: "c", onclick: stop(() => clearGroupMarks(group)) }),
      el("button", {
        type: "button",
        class: "btn btn-small btn-danger group-delete",
        text: markedHere ? "Delete " + markedHere + " marked in group" : "Delete marked in group",
        title: "d",
        disabled: markedHere === 0,
        onclick: stop(() => requestDelete("group", group)),
      }),
    ]);
    if (group.canonical_name) {
      const note = el("div", { class: "group-canonical" }, [
        el("span", { class: "muted", text: "Original name: " }),
        el("code", { text: group.canonical_name }),
      ]);
      if (group.suggested_rename) {
        note.appendChild(
          el("button", {
            type: "button",
            class: "btn btn-small",
            text: "Rename keeper to " + group.suggested_rename.new_name,
            title: "N",
            onclick: stop(() => renameKeeperToCanonical(group)),
          })
        );
      }
      actions.appendChild(note);
    }
    card.appendChild(actions);

    const body = el("div", { class: "group-body" });
    for (const file of group.files) body.appendChild(renderFile(group, file));
    card.appendChild(body);
    return card;
  }

  function renderFile(group, file) {
    const marked = state.marks.has(file.path);
    const isCursor = state.cursor && state.cursor.path === file.path;
    const row = el("div", {
      class: "file" + (marked ? " marked" : "") + (file.keep ? " keep" : "") + (isCursor ? " cursor" : ""),
      "data-path": file.path,
    });

    const checkbox = el("input", {
      type: "checkbox",
      checked: marked,
      "aria-label": "Mark " + file.path + " for deletion",
    });
    checkbox.addEventListener("change", () => {
      setMark(file.path, group, checkbox.checked);
      row.classList.toggle("marked", checkbox.checked);
      refreshMarkViews();
      updateGroupMarkedCount(group);
    });
    row.appendChild(checkbox);

    const main = el("div", { class: "file-main" });
    const pathEl = el("span", { class: "file-path", title: file.path }, [
      el("span", { class: "dir", text: dirName(file.path) }),
      el("span", { class: "name", text: baseName(file.path) }),
    ]);
    main.appendChild(pathEl);

    const meta = el("div", { class: "file-meta" });
    if (file.keep) meta.appendChild(el("span", { class: "badge badge-keep", text: "Keep" }));
    meta.appendChild(el("span", { text: formatDate(file.modified) }));
    meta.appendChild(el("span", { class: "score", text: "score " + file.score }));
    for (const reason of file.reasons || []) {
      meta.appendChild(
        el("span", {
          class: "reason" + (reason === "PreferredLocation" ? " negative" : ""),
          text: REASONS[reason] || String(reason),
        })
      );
    }
    main.appendChild(meta);

    const renameHost = el("div");
    main.appendChild(renameHost);
    row.appendChild(main);

    const actions = el("div", { class: "file-actions" }, [
      el("button", { type: "button", class: "btn btn-small", text: "Preview", onclick: () => openPreview(file) }),
      el("button", { type: "button", class: "btn btn-small", text: "Download", onclick: () => downloadFile(file.path) }),
      el("button", {
        type: "button",
        class: "btn btn-small",
        text: "Rename",
        onclick: () => showRenameForm(renameHost, group, file),
      }),
    ]);
    row.appendChild(actions);

    // Clicking the row body moves the cursor there (buttons and the checkbox keep their own job).
    row.addEventListener("click", (e) => {
      if (e.target.closest("button, input, .rename-form")) return;
      setCursor({ g: group, gi: state.groups.indexOf(group), f: file }, { scroll: false });
    });

    state.rowRefs.set(file.path, { group, file, host: renameHost, row });
    return row;
  }

  function groupCard(hash) {
    return $("groups").querySelector('.group[data-hash="' + CSS.escape(hash) + '"]');
  }

  function updateGroupMarkedCount(group) {
    const card = groupCard(group.hash);
    if (!card) return;
    const count = markedInGroup(group).length;
    let node = card.querySelector(".group-marked");
    if (count === 0) {
      if (node) node.remove();
    } else {
      if (!node) {
        node = el("span", { class: "group-marked" });
        card.querySelector(".group-title").after(node);
      }
      node.textContent = count + " marked";
    }
    const del = card.querySelector(".group-delete");
    if (del) {
      del.disabled = count === 0;
      del.textContent = count ? "Delete " + count + " marked in group" : "Delete marked in group";
    }
  }

  function setMark(path, group, on) {
    if (on) state.marks.set(path, { hash: group.hash, size: group.file_size });
    else state.marks.delete(path);
  }

  /** Set a mark and update the row's checkbox and class without re-rendering. */
  function setMarkAndRow(path, group, on) {
    setMark(path, group, on);
    const ref = state.rowRefs.get(path);
    if (ref) {
      ref.row.classList.toggle("marked", on);
      const cb = ref.row.querySelector('input[type="checkbox"]');
      if (cb) cb.checked = on;
    }
  }

  // ---- cursor (the TUI's selected group / file) ------------------------

  function flatFiles() {
    const out = [];
    state.groups.forEach((g, gi) => g.files.forEach((f) => out.push({ g, gi, f })));
    return out;
  }

  function cursorIndex(files) {
    if (!state.cursor) return -1;
    return files.findIndex((x) => x.f.path === state.cursor.path);
  }

  function cursorGroupIndex() {
    if (!state.cursor) return -1;
    return state.groups.findIndex((g) => g.hash === state.cursor.hash);
  }

  function setCursor(entry, opts) {
    const scroll = !(opts && opts.scroll === false);
    if (!entry) {
      state.cursor = null;
      renderCursor(false);
      return;
    }
    state.cursor = { hash: entry.g.hash, path: entry.f.path };
    state.cursorGroupIndex = entry.gi;
    if (isCollapsed(entry.g.hash)) {
      toggleCollapsed(entry.g.hash);
      const card = groupCard(entry.g.hash);
      if (card) {
        card.classList.remove("collapsed");
        const head = card.querySelector(".group-head");
        if (head) head.setAttribute("aria-expanded", "true");
      }
    }
    renderCursor(scroll);
  }

  function renderCursor(scroll) {
    const container = $("groups");
    for (const n of container.querySelectorAll(".file.cursor")) n.classList.remove("cursor");
    for (const n of container.querySelectorAll(".group.cursor-group")) n.classList.remove("cursor-group");
    if (!state.cursor) return;
    const ref = state.rowRefs.get(state.cursor.path);
    if (!ref) return;
    ref.row.classList.add("cursor");
    const card = groupCard(state.cursor.hash);
    if (card) card.classList.add("cursor-group");
    if (scroll) ref.row.scrollIntoView({ block: "nearest" });
  }

  /** After a re-fetch, keep the cursor on the same file, else the same
      group, else the group at the same position (like App::clamp_selection). */
  function reconcileCursor() {
    if (!state.cursor) return;
    if (state.pathIndex.has(state.cursor.path)) return;
    const gi = state.groups.findIndex((g) => g.hash === state.cursor.hash);
    if (gi >= 0 && state.groups[gi].files.length) {
      state.cursor = { hash: state.groups[gi].hash, path: state.groups[gi].files[0].path };
      state.cursorGroupIndex = gi;
      return;
    }
    if (state.groups.length === 0) {
      state.cursor = null;
      return;
    }
    const idx = Math.min(state.cursorGroupIndex, state.groups.length - 1);
    const g = state.groups[idx];
    state.cursor = g.files.length ? { hash: g.hash, path: g.files[0].path } : null;
    state.cursorGroupIndex = idx;
  }

  /** Cursor entry, defaulting to the first file on the page (the TUI always
      has group 0 selected). Null when the page is empty. */
  function ensureCursor() {
    const files = flatFiles();
    if (files.length === 0) return null;
    let i = cursorIndex(files);
    if (i < 0) {
      i = 0;
      setCursor(files[0]);
    }
    return files[i];
  }

  function cursorGroup() {
    const entry = ensureCursor();
    return entry ? entry.g : null;
  }

  function moveFile(delta) {
    const files = flatFiles();
    if (files.length === 0) return;
    const i = cursorIndex(files);
    let next;
    if (i < 0) next = delta > 0 ? 0 : files.length - 1;
    else next = Math.min(files.length - 1, Math.max(0, i + delta));
    setCursor(files[next]);
  }

  function moveGroup(delta) {
    if (state.groups.length === 0) return;
    const gi = cursorGroupIndex();
    let next;
    if (gi < 0) next = delta > 0 ? 0 : state.groups.length - 1;
    else next = Math.min(state.groups.length - 1, Math.max(0, gi + delta));
    gotoGroup(next);
  }

  function gotoGroup(gi) {
    const g = state.groups[gi];
    if (!g || g.files.length === 0) return;
    setCursor({ g, gi, f: g.files[0] });
  }

  function toggleCursorMark() {
    const entry = ensureCursor();
    if (!entry) return;
    const on = !state.marks.has(entry.f.path);
    setMarkAndRow(entry.f.path, entry.g, on);
    refreshMarkViews();
    updateGroupMarkedCount(entry.g);
  }

  // ---- pagination -------------------------------------------------------

  function renderPagination() {
    const nav = $("pagination");
    clear(nav);
    const limit = state.filters.limit;
    const pages = Math.max(1, Math.ceil(state.total / limit));
    if (pages <= 1) return;
    const current = Math.floor(state.offset / limit) + 1;

    const go = (page) => {
      state.offset = (page - 1) * limit;
      state.cursor = null;
      fetchGroups().then(() => window.scrollTo({ top: 0, behavior: "smooth" }));
    };
    nav.appendChild(el("button", { type: "button", class: "btn", text: "Prev", disabled: current === 1, onclick: () => go(current - 1) }));
    let lastShown = 0;
    for (let p = 1; p <= pages; p++) {
      const show = p === 1 || p === pages || Math.abs(p - current) <= 2;
      if (!show) continue;
      if (lastShown && p - lastShown > 1) nav.appendChild(el("span", { class: "dots", text: "..." }));
      nav.appendChild(
        el("button", {
          type: "button",
          class: "btn" + (p === current ? " current" : ""),
          text: String(p),
          "aria-current": p === current ? "page" : null,
          onclick: () => go(p),
        })
      );
      lastShown = p;
    }
    nav.appendChild(el("button", { type: "button", class: "btn", text: "Next", disabled: current === pages, onclick: () => go(current + 1) }));
  }

  // ---- filters ----------------------------------------------------------

  function saveFilters() {
    saveJSON(STORAGE_FILTERS, state.filters);
  }

  function debounce(fn, ms) {
    let t = null;
    return function () {
      clearTimeout(t);
      const args = arguments;
      t = setTimeout(() => fn.apply(null, args), ms);
    };
  }

  function filtersChanged() {
    state.offset = 0;
    state.cursor = null;
    fetchGroups();
  }

  /** z / t: step a filter select to its next option, wrapping around. */
  function cycleFilter(id, key, label) {
    const select = $(id);
    select.selectedIndex = (select.selectedIndex + 1) % select.options.length;
    state.filters[key] = select.value;
    filtersChanged();
    setStatus(label + ": " + select.options[select.selectedIndex].text);
  }

  function initFilters() {
    const path = $("pathFilter");
    const size = $("sizeFilter");
    const type = $("typeFilter");
    const pageSize = $("pageSize");
    path.value = state.filters.path || "";
    size.value = state.filters.size || "all";
    type.value = state.filters.type || "all";
    pageSize.value = String(state.filters.limit || 25);
    if (pageSize.value !== String(state.filters.limit)) {
      state.filters.limit = 25;
      pageSize.value = "25";
    }

    path.addEventListener(
      "input",
      debounce(() => {
        state.filters.path = path.value.trim();
        filtersChanged();
      }, 250)
    );
    size.addEventListener("change", () => {
      state.filters.size = size.value;
      filtersChanged();
    });
    type.addEventListener("change", () => {
      state.filters.type = type.value;
      filtersChanged();
    });
    pageSize.addEventListener("change", () => {
      state.filters.limit = parseInt(pageSize.value, 10) || 25;
      filtersChanged();
    });

    $("expandAll").addEventListener("click", () => {
      state.defaultCollapsed = false;
      state.collapsed.clear();
      renderGroups();
    });
    $("collapseAll").addEventListener("click", () => {
      state.defaultCollapsed = true;
      state.collapsed.clear();
      renderGroups();
    });

    populateAutoSelect();
    $("selectScope").value = state.selectScope;
    $("selectScope").addEventListener("change", (e) => {
      state.selectScope = e.target.value === "all" ? "all" : "page";
    });
    $("autoSelect").addEventListener("change", (e) => {
      const mode = e.target.value;
      e.target.value = "";
      if (mode) autoSelectFromToolbar(mode);
    });

    $("clearAllMarks").addEventListener("click", clearAllMarks);
    $("deleteSelected").addEventListener("click", () => requestDelete("all"));
    $("batchRename").addEventListener("click", openBatchRename);
    $("shutdown").addEventListener("click", confirmShutdown);
    $("statsToggle").addEventListener("click", toggleStats);
    $("helpBtn").addEventListener("click", openHelp);
  }

  // ---- auto select through the server ----------------------------------

  /** POST /api/select with the current filter and page. */
  function selectRequest(mode, scope) {
    const body = {
      mode: mode,
      scope: scope === "all" ? "all" : "page",
      offset: state.offset,
      limit: state.filters.limit,
    };
    if (state.filters.path) body.path = state.filters.path;
    if (state.filters.size !== "all") body.size = state.filters.size;
    if (state.filters.type !== "all") body.type = state.filters.type;
    return postJSON("/api/select", body);
  }

  /** Unmark everything in `clear`, then mark `paths`. `only` (a Set of
      paths) restricts both to one group. Returns how many files got marked. */
  function applySelection(result, only) {
    let n = 0;
    for (const p of result.clear || []) {
      if (only && !only.has(p)) continue;
      state.marks.delete(p);
    }
    for (const p of result.paths || []) {
      if (only && !only.has(p)) continue;
      state.marks.set(p, knownInfo(p));
      n++;
    }
    return n;
  }

  /** Groups that received at least one mark. Exact for the page; for scope
      "all" the server's count of groups considered is the best available. */
  function groupsTouched(result, scope) {
    if (scope === "all") return Number(result.groups) || 0;
    const marked = new Set(result.paths || []);
    return state.groups.filter((g) => g.files.some((f) => marked.has(f.path))).length;
  }

  /** The TUI's confidence label for a group's suggestions. */
  function confidence(group) {
    let max = 0;
    for (const f of group.files) if (!f.keep && f.score > max) max = f.score;
    if (max >= 100) return "high confidence";
    if (max >= 80) return "good confidence";
    if (max >= 50) return "medium confidence";
    return "low confidence, review carefully";
  }

  /** "oldest", "keeper", "shortest path"... from a mode label. */
  function survivorWord(mode) {
    return mode.label.replace(/^Mark all but /i, "").toLowerCase();
  }

  /** Apply a mode to one group on the page (a, o, m-letter, card buttons). */
  async function runGroupSelect(modeKey, group) {
    const mode = modeByKey(modeKey);
    if (!mode) return;
    const only = new Set(group.files.map((f) => f.path));
    try {
      let result = await selectRequest(mode.key, "page");
      // If the page shifted under us (scan still running), fall back to every matching group.
      if (!(result.clear || []).some((p) => only.has(p))) result = await selectRequest(mode.key, "all");
      const n = applySelection(result, only);
      renderGroups();
      const gi = state.groups.indexOf(group);
      if (gi >= 0 && (!state.cursor || state.cursor.hash !== group.hash)) setCursor({ g: group, gi, f: group.files[0] }, { scroll: false });

      const survivor = group.files.find((f) => !state.marks.has(f.path));
      const survivorName = survivor ? baseName(survivor.path) : "";
      if (mode.key === "suggested") {
        setStatus(
          n === 0
            ? "No file in this group looks like a copy. Use 'o' to mark all but the keeper, or Space."
            : "Marked " + n + " suggested file(s) (" + confidence(group) + ")"
        );
      } else if (mode.key === "allButKeeper") {
        const keeper = group.files.find((f) => f.keep) || survivor;
        setStatus("Marked " + n + " file(s), keeping " + (keeper ? baseName(keeper.path) : survivorName));
      } else {
        setStatus("Marked " + n + " file(s), keeping " + survivorName + " (" + survivorWord(mode) + ")");
      }
    } catch (e) {
      announce("Auto-select failed: " + e.message, "error");
    }
  }

  /** Apply a mode to every group matching the filter (A, O, Shift+letter). */
  async function runAllSelect(modeKey) {
    const mode = modeByKey(modeKey);
    if (!mode) return;
    try {
      const result = await selectRequest(mode.key, "all");
      const n = applySelection(result, null);
      renderGroups();
      hydrateMarks();
      if (mode.key === "suggested") {
        setStatus("Marked " + n + " suggested file(s) in " + groupsTouched(result, "all") + " group(s)");
      } else if (mode.key === "allButKeeper") {
        setStatus("Marked " + n + " file(s) across all groups");
      } else {
        setStatus("Marked " + n + " file(s) across all groups, keeping the " + survivorWord(mode));
      }
    } catch (e) {
      announce("Auto-select failed: " + e.message, "error");
    }
  }

  /** The Auto-select dropdown: scope from the toggle next to it. */
  async function autoSelectFromToolbar(modeKey) {
    if (modeKey === "clear") {
      clearPageMarks();
      return;
    }
    const mode = modeByKey(modeKey);
    if (!mode) return;
    const scope = state.selectScope;
    try {
      const result = await selectRequest(mode.key, scope);
      const n = applySelection(result, null);
      renderGroups();
      if (scope === "all") hydrateMarks();
      const g = groupsTouched(result, scope);
      announce("Marked " + plural(n, "file") + " in " + plural(g, "group") + (scope === "all" ? "" : " on this page"));
    } catch (e) {
      announce("Auto-select failed: " + e.message, "error");
    }
  }

  function clearGroupMarks(group) {
    for (const f of group.files) state.marks.delete(f.path);
    renderGroups();
    setStatus("Cleared marks in this group");
  }

  function clearPageMarks() {
    for (const g of state.groups) for (const f of g.files) state.marks.delete(f.path);
    renderGroups();
    announce("Cleared marks on this page");
  }

  function clearAllMarks() {
    state.marks.clear();
    renderGroups();
    announce("Cleared all marks");
  }

  // ---- modal ------------------------------------------------------------

  let modalCloseCallback = null;

  function modalOpen() {
    return !$("modalBackdrop").hidden;
  }

  function openModal(title, bodyNodes, footNodes, opts) {
    opts = opts || {};
    const backdrop = $("modalBackdrop");
    const modal = $("modal");
    modal.classList.toggle("wide", Boolean(opts.wide));
    $("modalTitle").textContent = title;
    const body = $("modalBody");
    const foot = $("modalFoot");
    clear(body);
    clear(foot);
    for (const n of [].concat(bodyNodes || [])) if (n) body.appendChild(n);
    for (const n of [].concat(footNodes || [])) if (n) foot.appendChild(n);
    modalCloseCallback = opts.onClose || null;
    state.modalKind = opts.kind || "other";
    state.modalKeys = opts.keys || null;
    backdrop.hidden = false;
    if (opts.focus === "modal") {
      modal.focus();
    } else {
      const focus = modal.querySelector("input, button.btn-primary, button.btn-danger, button");
      if (focus) focus.focus();
    }
  }

  function closeModal() {
    const backdrop = $("modalBackdrop");
    if (backdrop.hidden) return;
    backdrop.hidden = true;
    clear($("modalBody"));
    clear($("modalFoot"));
    state.modalKind = null;
    state.modalKeys = null;
    if (modalCloseCallback) {
      const cb = modalCloseCallback;
      modalCloseCallback = null;
      cb();
    }
  }

  function initModal() {
    $("modalClose").addEventListener("click", closeModal);
    $("modalBackdrop").addEventListener("click", (e) => {
      if (e.target === e.currentTarget) closeModal();
    });
  }

  // ---- preview ----------------------------------------------------------

  async function openPreview(file) {
    const ext = extension(file.path);
    const url = fileUrl(file.path, false);
    const downloadBtn = el("button", { type: "button", class: "btn", text: "Download", onclick: () => downloadFile(file.path) });
    const closeBtn = el("button", { type: "button", class: "btn btn-primary", text: "Close", onclick: closeModal });
    const pathLine = el("p", { class: "muted" }, [el("code", { text: file.path })]);
    const opts = { wide: true, kind: "preview" };

    let content;
    if (IMAGE_EXT.includes(ext)) {
      content = el("img", { class: "preview-media", src: url, alt: "Preview of " + baseName(file.path) });
    } else if (VIDEO_EXT.includes(ext)) {
      content = el("video", { class: "preview-media", src: url, controls: "controls" });
    } else if (AUDIO_EXT.includes(ext)) {
      content = el("audio", { class: "preview-audio", src: url, controls: "controls" });
    } else if (ext === "pdf") {
      content = el("iframe", { class: "preview-frame", src: url, title: "PDF preview" });
    } else if (ext === "txt" || ext === "text" || ext === "log") {
      content = el("pre", { class: "preview-text", text: "Loading..." });
      openModal(baseName(file.path), [pathLine, content], [downloadBtn, closeBtn], opts);
      try {
        const res = await fetch(url);
        if (!res.ok) {
          let msg = res.status + " " + res.statusText;
          try {
            const j = await res.json();
            if (j && j.error) msg = j.error;
          } catch (_) {
            /* not JSON */
          }
          throw new Error(msg);
        }
        const blob = await res.blob();
        const slice = blob.size > 512 * 1024 ? blob.slice(0, 512 * 1024) : blob;
        content.textContent = (await slice.text()) + (blob.size > 512 * 1024 ? "\n\n[truncated: showing the first 512 KiB of " + formatBytes(blob.size) + "]" : "");
      } catch (e) {
        content.textContent = "Could not load preview: " + e.message;
      }
      return;
    } else {
      content = el("p", { text: "No inline preview for ." + (ext || "this") + " files. Use Download to save a copy." });
    }
    openModal(baseName(file.path), [pathLine, content], [downloadBtn, closeBtn], opts);
  }

  function downloadFile(path) {
    // Content-Disposition: attachment makes the browser save without navigating.
    window.location.assign(fileUrl(path, true));
  }

  // ---- rename -----------------------------------------------------------

  function cancelRename() {
    if (state.activeRename) {
      clear(state.activeRename);
      state.activeRename = null;
      return true;
    }
    return false;
  }

  function showRenameForm(host, group, file) {
    cancelRename();
    clear(host);
    state.activeRename = host;
    const input = el("input", { type: "text", class: "rename-input", value: baseName(file.path), "aria-label": "New file name", spellcheck: "false" });
    const save = el("button", { type: "button", class: "btn btn-small btn-primary", text: "Save" });
    const cancel = el("button", { type: "button", class: "btn btn-small", text: "Cancel", onclick: cancelRename });
    const form = el("div", { class: "rename-form" }, [input, save, cancel]);
    host.appendChild(form);
    input.focus();
    const dot = input.value.lastIndexOf(".");
    input.setSelectionRange(0, dot > 0 ? dot : input.value.length);

    const submit = async () => {
      const newName = input.value;
      if (!newName || newName === baseName(file.path)) {
        cancelRename();
        return;
      }
      save.disabled = true;
      try {
        const updated = await renameOne(file.path, newName);
        if (state.cursor && state.cursor.path === file.path) state.cursor.path = dirName(file.path) + newName;
        announce("Renamed to " + newName, "success");
        applyGroupUpdate(updated);
      } catch (e) {
        announce("Rename failed: " + e.message, "error");
        save.disabled = false;
      }
    };
    save.addEventListener("click", submit);
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        submit();
      } else if (e.key === "Escape") {
        e.stopPropagation();
        cancelRename();
      }
    });
  }

  // Mouse counterpart of the TUI's N key: give the keeper the group's
  // original name (copy markers such as " (1)" removed).
  async function renameKeeperToCanonical(group) {
    if (!group.suggested_rename) {
      setStatus("No name to restore in this group");
      return;
    }
    const ok = await renameOne(group.suggested_rename.path, group.suggested_rename.new_name);
    if (ok) setStatus("Renamed keeper to " + group.suggested_rename.new_name);
  }

  async function renameOne(path, newName) {
    const updated = await postJSON("/api/rename", { path: path, new_name: newName });
    // Carry a mark over to the new path.
    const info = state.marks.get(path);
    if (info) {
      state.marks.delete(path);
      const newPath = dirName(path) + newName;
      state.marks.set(newPath, info);
    }
    return updated;
  }

  /** Replace one group in the current page with the server's updated copy. */
  function applyGroupUpdate(updated) {
    if (!updated || !updated.hash) return fetchGroups();
    const i = state.groups.findIndex((g) => g.hash === updated.hash);
    if (i < 0) return fetchGroups();
    state.groups[i] = updated;
    rebuildPathIndex();
    reconcileMarks();
    reconcileCursor();
    renderGroups();
  }

  function expandPattern(pattern, path, index) {
    const name = baseName(path);
    const dot = name.lastIndexOf(".");
    const stem = dot > 0 ? name.slice(0, dot) : name;
    const ext = dot > 0 ? name.slice(dot) : "";
    return pattern
      .split("{name}").join(stem)
      .split("{ext}").join(ext)
      .split("{i}").join(String(index).padStart(2, "0"));
  }

  function openBatchRename() {
    const paths = Array.from(state.marks.keys()).sort();
    if (paths.length === 0) return;
    const input = el("input", { type: "text", value: "{name}{ext}", "aria-label": "Rename pattern", spellcheck: "false" });
    const help = el("p", { class: "muted" }, [
      "Tokens: ",
      el("span", { class: "kbd", text: "{name}" }),
      " original name without extension, ",
      el("span", { class: "kbd", text: "{ext}" }),
      " extension including the dot, ",
      el("span", { class: "kbd", text: "{i}" }),
      " running number starting at 01. Files stay in their folders.",
    ]);
    const previewList = el("div", { class: "path-list" });
    const renderPreview = () => {
      clear(previewList);
      paths.slice(0, 8).forEach((p, i) => {
        previewList.appendChild(el("div", { text: baseName(p) + "  ->  " + expandPattern(input.value, p, i + 1) }));
      });
      if (paths.length > 8) previewList.appendChild(el("div", { class: "muted", text: "and " + (paths.length - 8) + " more" }));
    };
    input.addEventListener("input", renderPreview);
    renderPreview();

    const apply = el("button", { type: "button", class: "btn btn-primary", text: "Rename " + plural(paths.length, "file") });
    const cancel = el("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal });
    apply.addEventListener("click", async () => {
      const pattern = input.value;
      if (!pattern.trim()) return;
      apply.disabled = true;
      cancel.disabled = true;
      const results = el("ul");
      openModal("Batch rename", [el("p", { text: "Renaming..." }), results], [], { kind: "other" });
      let ok = 0;
      for (let i = 0; i < paths.length; i++) {
        const p = paths[i];
        const newName = expandPattern(pattern, p, i + 1);
        if (newName === baseName(p)) {
          results.appendChild(el("li", { text: baseName(p) + ": unchanged" }));
          continue;
        }
        try {
          await renameOne(p, newName);
          ok++;
          results.appendChild(el("li", { class: "ok", text: baseName(p) + " -> " + newName }));
        } catch (e) {
          results.appendChild(el("li", { class: "fail", text: baseName(p) + ": " + e.message }));
        }
      }
      $("modalBody").firstChild.textContent = "Renamed " + ok + " of " + paths.length + " files.";
      $("modalFoot").appendChild(el("button", { type: "button", class: "btn btn-primary", text: "Close", onclick: closeModal }));
      setStatus("Renamed " + ok + " of " + paths.length + " file(s)");
      fetchGroups();
    });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") apply.click();
    });

    openModal(
      "Batch rename " + plural(paths.length, "marked file"),
      [el("div", { class: "field" }, [el("label", { text: "Pattern" }), input]), help, previewList],
      [cancel, apply],
      { kind: "other" }
    );
  }

  // ---- delete -----------------------------------------------------------

  /** d / D and the delete buttons: collect marked paths, then confirm. */
  async function requestDelete(scope, group) {
    let paths;
    if (scope === "group") {
      if (!group) group = cursorGroup();
      if (!group) return;
      paths = markedInGroup(group).map((f) => f.path);
    } else {
      paths = Array.from(state.marks.keys());
    }
    if (paths.length === 0) {
      setStatus("Nothing marked. Space marks a file, 'a' marks suggested copies.");
      return;
    }
    if (scope !== "group") await hydrateMarks();
    openConfirmDelete(paths.slice().sort(), scope === "group" ? "this group" : "all groups");
  }

  function openConfirmDelete(paths, scopeLabel) {
    let bytes = 0;
    for (const p of paths) {
      const info = state.marks.get(p) || knownInfo(p);
      bytes += Number(info.size) || 0;
    }
    const method = state.scan ? state.scan.delete_method_description : "unknown";
    const list = el("div", { class: "path-list confirm-list" });
    for (const p of paths.slice(0, CONFIRM_LIST_CAP)) list.appendChild(el("div", { class: "confirm-path", text: p }));
    if (paths.length > CONFIRM_LIST_CAP) {
      list.appendChild(el("div", { class: "muted", text: "... and " + (paths.length - CONFIRM_LIST_CAP) + " more" }));
    }

    let decided = false;
    const cancel = el("button", { type: "button", class: "btn", text: "Cancel (n / Esc)", onclick: () => closeModal() });
    const confirmBtn = el("button", { type: "button", class: "btn btn-danger", text: "Delete (y / Enter)" });
    const doDelete = async () => {
      if (decided) return;
      decided = true;
      confirmBtn.disabled = true;
      cancel.disabled = true;
      await performDelete(paths);
    };
    confirmBtn.addEventListener("click", doDelete);

    openModal(
      "Confirm deletion",
      [
        el("p", { class: "confirm-title" }, [
          el("strong", { text: "Delete " + paths.length + " file(s) from " + scopeLabel + ", freeing " + formatBytes(bytes) + "?" }),
        ]),
        el("p", { class: "confirm-method", text: "Method: " + method }),
        list,
        el("p", { class: "muted", text: "dupscanner never deletes the last copy in a group." }),
        el("p", { class: "muted keys-hint", text: "y / Enter: delete      n / Esc: cancel" }),
      ],
      [cancel, confirmBtn],
      {
        kind: "confirm",
        focus: "modal",
        onClose: () => {
          if (!decided) setStatus("Deletion cancelled");
        },
        keys: (e) => {
          if (e.key === "y" || e.key === "Y") {
            doDelete();
            return true;
          }
          if (e.key === "Enter") {
            // A focused button keeps its native Enter, so Cancel stays Cancel.
            if (document.activeElement && document.activeElement.tagName === "BUTTON") return false;
            doDelete();
            return true;
          }
          if (e.key === "n" || e.key === "N" || e.key === "q") {
            closeModal();
            return true;
          }
          return false;
        },
      }
    );
  }

  async function performDelete(paths) {
    const label = state.scan ? state.scan.delete_method : "unknown";
    try {
      const result = await postJSON("/api/delete", { paths: paths });
      const deleted = result.deleted || [];
      const failed = result.failed || [];
      for (const p of deleted) state.marks.delete(p);
      modalCloseCallback = null;
      closeModal();
      if (failed.length > 0) {
        announce(
          "Deleted " + deleted.length + " file(s) via " + label + ", " + failed.length + " failed: " + (failed[0].error || ""),
          "error"
        );
        for (const f of failed.slice(0, 3)) toast(f.path + ": " + f.error, "error");
      } else {
        announce("Deleted " + deleted.length + " file(s) via " + label + ", freed " + formatBytes(result.bytes_freed), "success");
      }
    } catch (e) {
      modalCloseCallback = null;
      closeModal();
      announce("Not deleting: " + e.message, "error");
    } finally {
      fetchGroups();
      refreshScan();
    }
  }

  // ---- mark menu (m) ----------------------------------------------------

  function openMarkMenu() {
    const group = cursorGroup();
    if (!group) {
      setStatus("No group selected");
      return;
    }
    const modes = selectModes();
    const list = el("div", { class: "mark-menu" });
    for (const m of modes) {
      const row = el("div", { class: "mark-menu-row" }, [
        el("span", { class: "kbd", text: m.shortcut }),
        el("button", {
          type: "button",
          class: "btn btn-small mark-menu-label",
          text: m.label,
          title: "Apply to this group (" + m.shortcut + ")",
          onclick: () => {
            closeModal();
            runGroupSelect(m.key, group);
          },
        }),
        el("button", {
          type: "button",
          class: "btn btn-small",
          text: "All groups",
          title: "Apply to all matching groups (Shift+" + m.shortcut + ")",
          onclick: () => {
            closeModal();
            runAllSelect(m.key);
          },
        }),
      ]);
      list.appendChild(row);
    }
    openModal(
      "Mark all but...",
      [
        el("p", { class: "muted", text: "Press a letter to apply the rule to this group, Shift+letter for all matching groups, Esc to close." }),
        list,
      ],
      [el("button", { type: "button", class: "btn", text: "Close (Esc)", onclick: closeModal })],
      {
        kind: "markmenu",
        focus: "modal",
        keys: (e) => {
          if (e.key.length !== 1) return false;
          const m = modes.find((x) => String(x.shortcut).toLowerCase() === e.key.toLowerCase());
          if (!m) return false;
          const all = e.shiftKey || e.key !== e.key.toLowerCase();
          closeModal();
          if (all) runAllSelect(m.key);
          else runGroupSelect(m.key, group);
          return true;
        },
      }
    );
  }

  // ---- help (?) ---------------------------------------------------------

  function openHelp() {
    if (modalOpen() && state.modalKind === "help") {
      closeModal();
      return;
    }
    const method = state.scan ? state.scan.delete_method_description : "unknown";
    const modes = selectModes().map((m) => m.shortcut + " " + m.label.replace(/^Mark /, "")).join(", ");
    const section = (title, rows) => {
      const dl = el("dl", { class: "help-list" });
      for (const [keys, desc] of rows) {
        dl.appendChild(el("dt", {}, keys.split(" / ").map((k, i) => [i ? " / " : null, el("span", { class: "kbd", text: k })]).flat()));
        dl.appendChild(el("dd", { text: desc }));
      }
      return el("div", { class: "help-section" }, [el("h3", { text: title }), dl]);
    };
    const body = [
      section("Navigation", [
        ["j / ↓", "next file"],
        ["k / ↑", "previous file"],
        ["n / →", "next group"],
        ["p / ←", "previous group"],
        ["g", "first group on the page"],
        ["G", "last group on the page"],
      ]),
      section("Marking", [
        ["Space", "toggle mark on the selected file"],
        ["a / A", "mark files that look like copies (group / all matching groups)"],
        ["o / O", "mark everything except the keeper (group / all matching groups)"],
        ["m", "more rules: " + modes + " (letter: group, Shift+letter: all)"],
        ["c / C", "clear marks (group / everywhere)"],
      ]),
      section("Deleting", [
        ["d / D", "delete marked files (group / all), after confirmation"],
        ["y / Enter", "confirm the deletion dialog"],
        ["n / Esc", "cancel the deletion dialog"],
      ]),
      el("p", { class: "help-note" }, ["Method: " + method + ". ", el("strong", { text: "dupscanner never deletes the last copy in a group." })]),
      section("Files", [
        ["Enter / e", "preview the selected file"],
        ["r", "rename the selected file"],
      ]),
      section("Filters", [
        ["/", "focus the path filter"],
        ["z", "cycle the size filter"],
        ["t", "cycle the type filter"],
      ]),
      section("Views", [
        ["Tab", "toggle the statistics panel"],
        ["?", "toggle this help"],
        ["Esc", "close dialogs, cancel a rename, or clear the cursor"],
      ]),
      el("p", { class: "help-keep", text: "KEEP marks the file the heuristics would keep: fewest copy signals, then shallowest path, then oldest." }),
    ];
    openModal("Help", body, [el("button", { type: "button", class: "btn btn-primary", text: "Close", onclick: closeModal })], {
      kind: "help",
      focus: "modal",
    });
  }

  // ---- keyboard ---------------------------------------------------------

  const NAV_KEYS = new Set(["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Enter", "Escape", "Tab", "PageDown", "PageUp", "Home", "End"]);

  function isEditable(node) {
    if (!node || node === document.body) return false;
    const tag = node.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || node.isContentEditable === true;
  }

  function isButtonLike(node) {
    if (!node || node === document.body) return false;
    const tag = node.tagName;
    return tag === "BUTTON" || tag === "A" || tag === "SUMMARY" || node.getAttribute("role") === "button";
  }

  function onKeydown(e) {
    const target = e.target;
    const inModal = modalOpen();

    if (e.key === "Escape") {
      if (inModal) {
        e.preventDefault();
        closeModal();
        return;
      }
      if (isEditable(target)) {
        target.blur();
        return;
      }
      if (cancelRename()) return;
      if (state.cursor) setCursor(null);
      clearStatus();
      return;
    }

    if (isEditable(target)) return;

    if (inModal) {
      if (state.modalKeys && state.modalKeys(e)) {
        e.preventDefault();
        return;
      }
      if (e.key === "?" && state.modalKind === "help") {
        e.preventDefault();
        closeModal();
      }
      return;
    }

    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if ((e.key === "Enter" || e.key === " ") && (isButtonLike(target) || (target && target.tagName === "INPUT"))) return;
    if (e.key === "Tab") {
      if (e.shiftKey) return;
      e.preventDefault();
      toggleStats();
      return;
    }
    if (e.key.length !== 1 && !NAV_KEYS.has(e.key)) return;

    // Like the TUI: any key clears the last status, except the ones that set one.
    const keepsStatus = e.key.length === 1 && "aAoOdDcC".includes(e.key);
    if (!keepsStatus) clearStatus();

    switch (e.key) {
      case "j":
      case "ArrowDown":
        moveFile(1);
        break;
      case "k":
      case "ArrowUp":
        moveFile(-1);
        break;
      case "n":
      case "ArrowRight":
      case "PageDown":
        moveGroup(1);
        break;
      case "p":
      case "ArrowLeft":
      case "PageUp":
        moveGroup(-1);
        break;
      case "g":
      case "Home":
        gotoGroup(0);
        break;
      case "G":
      case "End":
        gotoGroup(state.groups.length - 1);
        break;
      case " ":
        toggleCursorMark();
        break;
      case "Enter":
      case "e": {
        const entry = ensureCursor();
        if (entry) openPreview(entry.f);
        break;
      }
      case "a": {
        const g = cursorGroup();
        if (g) runGroupSelect("suggested", g);
        break;
      }
      case "A":
        runAllSelect("suggested");
        break;
      case "o": {
        const g = cursorGroup();
        if (g) runGroupSelect("allButKeeper", g);
        break;
      }
      case "O":
        runAllSelect("allButKeeper");
        break;
      case "c": {
        const g = cursorGroup();
        if (g) clearGroupMarks(g);
        break;
      }
      case "C":
        state.marks.clear();
        renderGroups();
        setStatus("Cleared all marks");
        break;
      case "m":
        openMarkMenu();
        break;
      case "d":
        requestDelete("group");
        break;
      case "D":
        requestDelete("all");
        break;
      case "r": {
        const entry = ensureCursor();
        if (entry) {
          const ref = state.rowRefs.get(entry.f.path);
          if (ref) showRenameForm(ref.host, ref.group, ref.file);
        }
        break;
      }
      case "/":
        $("pathFilter").focus();
        $("pathFilter").select();
        break;
      case "z":
        cycleFilter("sizeFilter", "size", "Size filter");
        break;
      case "t":
        cycleFilter("typeFilter", "type", "Type filter");
        break;
      case "?":
        openHelp();
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  function initKeyboard() {
    document.addEventListener("keydown", onKeydown);
    $("keybar").addEventListener("click", clearStatus);
  }

  // ---- shutdown ---------------------------------------------------------

  function confirmShutdown() {
    const stop = el("button", { type: "button", class: "btn btn-danger", text: "Stop server" });
    const cancel = el("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal });
    stop.addEventListener("click", async () => {
      stop.disabled = true;
      try {
        await postJSON("/api/shutdown", {});
      } catch (_) {
        /* the server may exit before answering */
      }
      state.stopped = true;
      if (eventSource) eventSource.close();
      openModal("Server stopped", [el("p", { text: "dupscanner has exited. You can close this tab." })], [], {
        kind: "other",
        onClose: () => {
          openModal("Server stopped", [el("p", { text: "dupscanner has exited. You can close this tab." })], [], { kind: "other" });
        },
      });
    });
    openModal(
      "Stop the dupscanner server?",
      [el("p", { text: "The page stops working until you run dupscanner serve again." })],
      [cancel, stop],
      { kind: "other" }
    );
  }

  // ---- SSE ----------------------------------------------------------------

  let eventSource = null;

  function initEvents() {
    eventSource = new EventSource("/api/events");
    eventSource.addEventListener("progress", (e) => {
      try {
        renderScan(JSON.parse(e.data));
      } catch (_) {
        /* ignore malformed */
      }
    });
    eventSource.addEventListener("groups", (e) => {
      let data = null;
      try {
        data = JSON.parse(e.data);
      } catch (_) {
        return;
      }
      if (state.scan) {
        state.scan.group_count = data.group_count;
        state.scan.duplicate_files = data.duplicate_files;
        state.scan.wasted_space = data.wasted_space;
        renderScan(state.scan);
      }
      if (data.version !== state.version) fetchGroups();
    });
    eventSource.addEventListener("complete", (e) => {
      try {
        renderScan(JSON.parse(e.data));
      } catch (_) {
        /* ignore malformed */
      }
      fetchGroups();
      toast("Scan complete", "success");
    });
    eventSource.addEventListener("error", () => {
      if (state.stopped) return;
      const badge = $("statusBadge");
      if (eventSource.readyState === EventSource.CLOSED) {
        badge.textContent = "Disconnected";
        badge.className = "badge badge-error";
      }
    });
    eventSource.addEventListener("open", () => {
      // Reconnected (or first connect): resync in case anything was missed.
      refreshScan();
      fetchGroups();
    });
  }

  // ---- boot -------------------------------------------------------------

  async function boot() {
    initTheme();
    initModal();
    initFilters();
    initKeyboard();
    setStatsOpen(state.statsOpen);
    renderKeybar();
    await refreshScan();
    await fetchGroups();
    initEvents();
  }

  document.addEventListener("DOMContentLoaded", boot);
})();
