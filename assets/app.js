/* dupscanner web UI. Plain JS, no dependencies. All data from the server is
   rendered through textContent / DOM construction, never innerHTML. */
(function () {
  "use strict";

  // ---- storage helpers --------------------------------------------------

  const STORAGE_THEME = "dupscanner.theme";
  const STORAGE_FILTERS = "dupscanner.filters";

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
    /** path -> { hash, size } */
    marks: new Map(),
    /** hashes of collapsed groups */
    collapsed: new Set(),
    defaultCollapsed: false,
    fetching: false,
    refetchQueued: false,
    stopped: false,
  };

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

  // ---- toasts -----------------------------------------------------------

  function toast(message, kind, ttl) {
    const node = el("div", { class: "toast " + (kind || ""), role: "status", text: message });
    $("toasts").appendChild(node);
    setTimeout(() => node.remove(), ttl || (kind === "error" ? 8000 : 4500));
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

  function renderScan(scan) {
    if (!scan) return;
    state.scan = scan;
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
  }

  async function refreshScan() {
    try {
      renderScan(await api("/api/scan"));
    } catch (e) {
      toast("Could not load scan status: " + e.message, "error");
    }
  }

  // ---- groups -----------------------------------------------------------

  function groupsUrl() {
    const p = new URLSearchParams();
    p.set("offset", String(state.offset));
    p.set("limit", String(state.filters.limit));
    if (state.filters.path) p.set("path", state.filters.path);
    if (state.filters.size !== "all") p.set("size", state.filters.size);
    if (state.filters.type !== "all") p.set("type", state.filters.type);
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
      reconcileMarks();
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

  /** Drop marks that point at paths no longer present in groups we can see. */
  function reconcileMarks() {
    const present = new Map();
    for (const g of state.groups) {
      const paths = new Set(g.files.map((f) => f.path));
      present.set(g.hash, paths);
    }
    for (const [path, info] of Array.from(state.marks.entries())) {
      const paths = present.get(info.hash);
      if (paths && !paths.has(path)) state.marks.delete(path);
    }
  }

  function markedTotals() {
    let bytes = 0;
    for (const info of state.marks.values()) bytes += Number(info.size) || 0;
    return { count: state.marks.size, bytes };
  }

  function updateActionButtons() {
    const { count, bytes } = markedTotals();
    const del = $("deleteSelected");
    del.disabled = count === 0;
    del.textContent = count === 0 ? "Delete marked" : "Delete " + count + " marked (" + formatBytes(bytes) + ")";
    const ren = $("batchRename");
    ren.disabled = count === 0;
    ren.textContent = count === 0 ? "Batch rename" : "Batch rename " + count;
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
    renderSummary();
    renderPagination();
    updateActionButtons();
    window.scrollTo(0, scrollY);
  }

  function renderGroup(group, index) {
    const markedHere = group.files.filter((f) => state.marks.has(f.path)).length;
    const card = el("div", { class: "group" + (isCollapsed(group.hash) ? " collapsed" : ""), "data-hash": group.hash });

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
        toggle();
      }
    });
    card.appendChild(head);

    const body = el("div", { class: "group-body" });
    for (const file of group.files) body.appendChild(renderFile(group, file));
    card.appendChild(body);
    return card;
  }

  function renderFile(group, file) {
    const marked = state.marks.has(file.path);
    const row = el("div", { class: "file" + (marked ? " marked" : "") + (file.keep ? " keep" : ""), "data-path": file.path });

    const checkbox = el("input", {
      type: "checkbox",
      checked: marked,
      "aria-label": "Mark " + file.path + " for deletion",
    });
    checkbox.addEventListener("change", () => {
      setMark(file.path, group, checkbox.checked);
      row.classList.toggle("marked", checkbox.checked);
      renderSummary();
      updateActionButtons();
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
    return row;
  }

  function updateGroupMarkedCount(group) {
    const card = $("groups").querySelector('.group[data-hash="' + CSS.escape(group.hash) + '"]');
    if (!card) return;
    const count = group.files.filter((f) => state.marks.has(f.path)).length;
    let node = card.querySelector(".group-marked");
    if (count === 0) {
      if (node) node.remove();
      return;
    }
    if (!node) {
      node = el("span", { class: "group-marked" });
      card.querySelector(".group-title").after(node);
    }
    node.textContent = count + " marked";
  }

  function setMark(path, group, on) {
    if (on) state.marks.set(path, { hash: group.hash, size: group.file_size });
    else state.marks.delete(path);
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

    const changed = () => {
      state.offset = 0;
      fetchGroups();
    };
    path.addEventListener(
      "input",
      debounce(() => {
        state.filters.path = path.value.trim();
        changed();
      }, 250)
    );
    size.addEventListener("change", () => {
      state.filters.size = size.value;
      changed();
    });
    type.addEventListener("change", () => {
      state.filters.type = type.value;
      changed();
    });
    pageSize.addEventListener("change", () => {
      state.filters.limit = parseInt(pageSize.value, 10) || 25;
      changed();
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

    $("autoSelect").addEventListener("change", (e) => {
      const mode = e.target.value;
      e.target.value = "";
      if (mode) applyAutoSelect(mode);
    });

    $("deleteSelected").addEventListener("click", confirmDelete);
    $("batchRename").addEventListener("click", openBatchRename);
    $("shutdown").addEventListener("click", confirmShutdown);
  }

  // ---- auto select ------------------------------------------------------

  /** Pick the index of the file to keep for the given mode, or -1 for none. */
  function survivorIndex(files, mode) {
    let best = 0;
    const cmp = {
      allButOldest: (a, b) => new Date(a.modified) - new Date(b.modified),
      allButNewest: (a, b) => new Date(b.modified) - new Date(a.modified),
      allButShortest: (a, b) => a.path.length - b.path.length,
      allButLongest: (a, b) => b.path.length - a.path.length,
    }[mode];
    if (!cmp) return -1;
    for (let i = 1; i < files.length; i++) {
      if (cmp(files[i], files[best]) < 0) best = i;
    }
    return best;
  }

  function applyAutoSelect(mode) {
    let marked = 0;
    for (const group of state.groups) {
      // Auto-select owns the marks of the groups it touches: start clean so
      // the survivor can never end up marked by an earlier manual click.
      for (const f of group.files) state.marks.delete(f.path);
      if (mode === "clear") continue;

      let toMark = [];
      if (mode === "suggested") {
        toMark = group.files.filter((f) => !f.keep && f.score > 0);
      } else if (mode === "allButKeeper") {
        toMark = group.files.filter((f) => !f.keep);
        if (toMark.length === group.files.length) toMark = toMark.slice(1);
      } else {
        const keep = survivorIndex(group.files, mode);
        toMark = group.files.filter((_, i) => i !== keep);
      }
      // Never mark every copy of a group.
      if (toMark.length >= group.files.length) toMark = toMark.slice(0, group.files.length - 1);
      for (const f of toMark) {
        setMark(f.path, group, true);
        marked++;
      }
    }
    renderGroups();
    if (mode === "clear") toast("Cleared marks on this page");
    else if (marked === 0) toast("Nothing matched on this page", "", 3000);
    else toast("Marked " + marked + " file" + (marked === 1 ? "" : "s") + " on this page");
  }

  // ---- modal ------------------------------------------------------------

  let modalCloseCallback = null;

  function openModal(title, bodyNodes, footNodes, opts) {
    const backdrop = $("modalBackdrop");
    const modal = $("modal");
    modal.classList.toggle("wide", Boolean(opts && opts.wide));
    $("modalTitle").textContent = title;
    const body = $("modalBody");
    const foot = $("modalFoot");
    clear(body);
    clear(foot);
    for (const n of [].concat(bodyNodes || [])) if (n) body.appendChild(n);
    for (const n of [].concat(footNodes || [])) if (n) foot.appendChild(n);
    modalCloseCallback = (opts && opts.onClose) || null;
    backdrop.hidden = false;
    const focus = modal.querySelector("input, button.btn-primary, button.btn-danger, button");
    if (focus) focus.focus();
  }

  function closeModal() {
    const backdrop = $("modalBackdrop");
    if (backdrop.hidden) return;
    backdrop.hidden = true;
    clear($("modalBody"));
    clear($("modalFoot"));
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
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") closeModal();
    });
  }

  // ---- preview ----------------------------------------------------------

  async function openPreview(file) {
    const ext = extension(file.path);
    const url = fileUrl(file.path, false);
    const downloadBtn = el("button", { type: "button", class: "btn", text: "Download", onclick: () => downloadFile(file.path) });
    const closeBtn = el("button", { type: "button", class: "btn btn-primary", text: "Close", onclick: closeModal });
    const pathLine = el("p", { class: "muted" }, [el("code", { text: file.path })]);

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
      openModal(baseName(file.path), [pathLine, content], [downloadBtn, closeBtn], { wide: true });
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
    openModal(baseName(file.path), [pathLine, content], [downloadBtn, closeBtn], { wide: true });
  }

  function downloadFile(path) {
    // Content-Disposition: attachment makes the browser save without navigating.
    window.location.assign(fileUrl(path, true));
  }

  // ---- rename -----------------------------------------------------------

  function showRenameForm(host, group, file) {
    clear(host);
    const input = el("input", { type: "text", class: "rename-input", value: baseName(file.path), "aria-label": "New file name", spellcheck: "false" });
    const save = el("button", { type: "button", class: "btn btn-small btn-primary", text: "Save" });
    const cancel = el("button", { type: "button", class: "btn btn-small", text: "Cancel", onclick: () => clear(host) });
    const form = el("div", { class: "rename-form" }, [input, save, cancel]);
    host.appendChild(form);
    input.focus();
    const dot = input.value.lastIndexOf(".");
    input.setSelectionRange(0, dot > 0 ? dot : input.value.length);

    const submit = async () => {
      const newName = input.value;
      if (!newName || newName === baseName(file.path)) {
        clear(host);
        return;
      }
      save.disabled = true;
      try {
        const updated = await renameOne(file.path, newName);
        toast("Renamed to " + newName, "success");
        applyGroupUpdate(updated);
      } catch (e) {
        toast("Rename failed: " + e.message, "error");
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
        clear(host);
      }
    });
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
    reconcileMarks();
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

    const apply = el("button", { type: "button", class: "btn btn-primary", text: "Rename " + paths.length + " file" + (paths.length === 1 ? "" : "s") });
    const cancel = el("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal });
    apply.addEventListener("click", async () => {
      const pattern = input.value;
      if (!pattern.trim()) return;
      apply.disabled = true;
      cancel.disabled = true;
      const results = el("ul");
      openModal("Batch rename", [el("p", { text: "Renaming..." }), results], [], {});
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
      fetchGroups();
    });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") apply.click();
    });

    openModal(
      "Batch rename " + paths.length + " marked file" + (paths.length === 1 ? "" : "s"),
      [el("div", { class: "field" }, [el("label", { text: "Pattern" }), input]), help, previewList],
      [cancel, apply],
      {}
    );
  }

  // ---- delete -----------------------------------------------------------

  function confirmDelete() {
    const paths = Array.from(state.marks.keys()).sort();
    if (paths.length === 0) return;
    const { bytes } = markedTotals();
    const method = state.scan ? state.scan.delete_method_description : "unknown";
    const list = el("div", { class: "path-list" });
    for (const p of paths) list.appendChild(el("div", { text: p }));

    const confirmBtn = el("button", { type: "button", class: "btn btn-danger", text: "Delete " + paths.length + " file" + (paths.length === 1 ? "" : "s") });
    const cancel = el("button", { type: "button", class: "btn", text: "Cancel", onclick: closeModal });
    confirmBtn.addEventListener("click", async () => {
      confirmBtn.disabled = true;
      cancel.disabled = true;
      try {
        const result = await postJSON("/api/delete", { paths: paths });
        for (const p of result.deleted || []) state.marks.delete(p);
        closeModal();
        const failed = (result.failed || []).length;
        toast(
          "Deleted " + (result.deleted || []).length + " file" + ((result.deleted || []).length === 1 ? "" : "s") + ", freed " + formatBytes(result.bytes_freed) + (failed ? ", " + failed + " failed" : ""),
          failed ? "error" : "success"
        );
        for (const f of (result.failed || []).slice(0, 3)) toast(f.path + ": " + f.error, "error");
      } catch (e) {
        closeModal();
        toast("Not deleted: " + e.message, "error");
      } finally {
        fetchGroups();
        refreshScan();
      }
    });

    openModal(
      "Delete " + paths.length + " marked file" + (paths.length === 1 ? "" : "s") + "?",
      [
        el("p", {}, ["This frees ", el("strong", { text: formatBytes(bytes) }), ". At least one copy of every group is always kept."]),
        el("p", { class: "muted" }, ["Delete method: ", el("strong", { text: method })]),
        list,
      ],
      [cancel, confirmBtn],
      {}
    );
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
        onClose: () => {
          openModal("Server stopped", [el("p", { text: "dupscanner has exited. You can close this tab." })], [], {});
        },
      });
    });
    openModal(
      "Stop the dupscanner server?",
      [el("p", { text: "The page stops working until you run dupscanner serve again." })],
      [cancel, stop],
      {}
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
    await refreshScan();
    await fetchGroups();
    initEvents();
  }

  document.addEventListener("DOMContentLoaded", boot);
})();
