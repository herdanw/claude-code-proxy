// requests.js — Requests tab: table, filtering, sorting, modal, anomaly focus, correlations, explanations

let entries = [];
let currentFilter = 'all';
let searchQuery = '';
let entriesRequestToken = 0;
let selectedRequestId = null;
let currentSortBy = 'timestamp_ms';
let currentSortOrder = 'desc';
let currentSessionFilter = 'all';
let anomalyFocus = null;
let anomalyFocusHasEmptyResults = false;
let requestViewSnapshot = null;
const DEFAULT_ANOMALY_WINDOW_MS = 120000;
const EXPANDED_ANOMALY_WINDOW_MS = 600000;

function setActiveTab(tabName, { persist = true } = {}) {
  const tab = normalizeTab(tabName);

  document.querySelectorAll('.tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tab));
  document.querySelectorAll('.tab-content').forEach(c => c.classList.toggle('active', c.id === `tab-${tab}`));

  if (persist) persistTab(tab);
  if (tab === 'sessions') loadSessions();
  if (tab === 'requests') {
    populateSessionFilter();
    updateSortArrows();
  }
  if (tab === 'overview') requestAnimationFrame(() => resizeCharts());
  if (tab === 'conformance' && typeof initConformanceTab === 'function') initConformanceTab();
}

function snapshotRequestViewState() {
  return {
    currentFilter,
    searchQuery,
    currentSessionFilter,
    currentSortBy,
    currentSortOrder,
    selectedRequestId,
    selectedSessionId,
  };
}

async function restoreRequestViewState(snapshot) {
  if (!snapshot) return;

  currentFilter = normalizeRequestFilter(snapshot.currentFilter);
  searchQuery = normalizeSearchQuery(snapshot.searchQuery);
  currentSessionFilter = snapshot.currentSessionFilter || 'all';
  currentSortBy = snapshot.currentSortBy || 'timestamp_ms';
  currentSortOrder = snapshot.currentSortOrder === 'asc' ? 'asc' : 'desc';
  selectedRequestId = snapshot.selectedRequestId || null;
  selectedSessionId = snapshot.selectedSessionId || null;

  document.getElementById('search-input').value = searchQuery;
  document.querySelectorAll('[data-filter]').forEach(button => {
    button.classList.toggle('active', button.dataset.filter === currentFilter);
  });

  await populateSessionFilter();
  const sessionFilter = document.getElementById('session-filter');
  if (sessionFilter) sessionFilter.value = currentSessionFilter;

  await loadEntries();
  if (selectedRequestId) {
    preselectRequestRow(selectedRequestId);
  } else {
    document.querySelectorAll('#request-table tr.request-row-clickable').forEach(row => {
      row.classList.remove('selected');
    });
  }
}

function renderAnomalyFocusBar() {
  const bar = document.getElementById('anomaly-focus-bar');
  const label = document.getElementById('anomaly-focus-label');
  if (!bar || !label) return;

  if (!anomalyFocus) {
    bar.style.display = 'none';
    label.textContent = '';
    return;
  }

  const tsLabel = Number.isFinite(anomalyFocus.timestampMs)
    ? new Date(anomalyFocus.timestampMs).toLocaleTimeString()
    : 'unknown time';
  const windowLabel = anomalyFocus.expanded ? '±10m' : '±2m';
  const kind = anomalyFocus.kind || 'anomaly';
  const messageSuffix = anomalyFocus.message ? ` — ${anomalyFocus.message}` : '';

  label.textContent = `${kind} around ${tsLabel} (${windowLabel})${messageSuffix}`;
  bar.style.display = 'flex';
}

async function clearAnomalyFocus({ restoreSnapshot = true, reload = true } = {}) {
  if (!anomalyFocus && !requestViewSnapshot) return;

  const snapshot = restoreSnapshot ? requestViewSnapshot : null;

  anomalyFocus = null;
  anomalyFocusHasEmptyResults = false;
  requestViewSnapshot = null;

  renderAnomalyFocusBar();
  updateExpandAnomalyButton();

  if (snapshot) {
    await restoreRequestViewState(snapshot);
    return;
  }

  if (reload) {
    await loadEntries();
  }
}

function setRequestFilter(filter, { persist = true, reload = true } = {}) {
  currentFilter = normalizeRequestFilter(filter);

  document.querySelectorAll('[data-filter]').forEach(button => {
    button.classList.toggle('active', button.dataset.filter === currentFilter);
  });

  if (persist) persistRequestFilter(currentFilter);
  if (reload) {
    clearAnomalyFocus({ restoreSnapshot: false, reload: false });
    loadEntries();
  }
}

function setSearchQuery(value, { persist = true, reload = true } = {}) {
  searchQuery = normalizeSearchQuery(value);
  document.getElementById('search-input').value = searchQuery;

  if (persist) persistSearchQuery(searchQuery);
  if (reload) {
    clearAnomalyFocus({ restoreSnapshot: false, reload: false });
    loadEntries();
  }
}

function selectRequest(entry) {
  selectedRequestId = entry.id;
  selectedSessionId = entry.session_id || null;
  loadExplanations(entry.id);
  if (selectedSessionId) {
    loadSessionTimeline(selectedSessionId);
  } else {
    const timeline = document.getElementById('session-timeline-list');
    if (timeline) timeline.innerHTML = '<div style="color:var(--text-2);font-size:12px">No session ID available for this request.</div>';
  }
}

function addTableRow(e, isNew = false) {
  if (!matchesFilter(e)) return;

  const tbody = document.getElementById('request-table');
  const tr = document.createElement('tr');
  tr.classList.add('request-row-clickable');
  tr.setAttribute('tabindex', '0');
  tr.dataset.requestId = e.id;
  if (isNew) tr.classList.add('new-entry');

  const time = new Date(e.timestamp).toLocaleTimeString();
  const statusClass = getStatusClass(e.status);
  const statusText = e.status?.value || e.status?.type || '?';
  const ttft = e.ttft_ms ? (e.ttft_ms / 1000).toFixed(2) + 's' : '—';
  const ttftClass = !e.ttft_ms ? '' : e.ttft_ms < 3000 ? 'ttft-good' : e.ttft_ms < 8000 ? 'ttft-slow' : 'ttft-bad';
  const dur = (e.duration_ms / 1000).toFixed(2) + 's';
  const tokens = e.input_tokens && e.output_tokens ? `${fmt(e.input_tokens)}→${fmt(e.output_tokens)}` : '—';
  const stallInfo = e.stalls?.length ? `${e.stalls.length} (${e.stalls.reduce((a, s) => a + s.duration_s, 0).toFixed(1)}s)` : '—';
  const stallClass = e.stalls?.length ? 'stall-text' : '';

  let anomalyBadges = '';
  if (e.anomalies?.length) {
    anomalyBadges = e.anomalies.map(a => {
      const cls = a.severity === 'Critical' ? 'sev-critical' : a.severity === 'Error' ? 'sev-error' : 'sev-warning';
      return `<span class="anomaly-badge ${cls}" title="${esc(a.message)}">${a.kind}</span>`;
    }).join('');
  }

  let distanceChip = '';
  if (Number.isFinite(e.distance_ms) && anomalyFocus && Number.isFinite(anomalyFocus.timestampMs)) {
    const deltaMs = new Date(e.timestamp).getTime() - anomalyFocus.timestampMs;
    distanceChip = `<span class="anomaly-badge" title="Distance from anomaly timestamp">${formatSignedDeltaMs(deltaMs)}</span>`;
  }

  const errorInfo = e.error ? `<div class="error-text" title="${esc(e.error)}">${esc(e.error.substring(0, 80))}</div>` : '';

  tr.innerHTML = `
    <td>${time}</td>
    <td><span class="status-badge ${statusClass}">${statusText}</span></td>
    <td class="ttft-cell ${ttftClass}">${ttft}</td>
    <td>${dur}</td>
    <td class="model-text">${e.model || '—'}</td>
    <td class="tokens-text">${tokens}</td>
    <td class="${stallClass}">${stallInfo}</td>
    <td style="max-width:180px;overflow:hidden;text-overflow:ellipsis" title="${esc(e.session_id || 'unknown')}">${esc(e.session_id || 'unknown')}</td>
    <td style="max-width:200px;overflow:hidden;text-overflow:ellipsis" title="${esc(e.path)}">${e.path}</td>
    <td>${distanceChip}${anomalyBadges}${errorInfo}</td>
  `;

  tr.addEventListener('click', () => {
    selectedRequestId = e.id;
    selectRequest(e);
    openRequestModal(e);
  });

  if (isNew) {
    tbody.insertBefore(tr, tbody.firstChild);
  } else {
    tbody.appendChild(tr);
  }

  while (tbody.children.length > 200) tbody.removeChild(tbody.lastChild);
}

function preselectRequestRow(requestId) {
  if (!requestId) return;

  const row = document.querySelector(`#request-table tr[data-request-id="${CSS.escape(requestId)}"]`);
  if (!row) return;

  document.querySelectorAll('#request-table tr.request-row-clickable').forEach(r => {
    r.classList.toggle('selected', r === row);
  });

  row.focus({ preventScroll: false });

  const entry = entries.find(e => e.id === requestId);
  if (entry) {
    selectedRequestId = requestId;
    selectRequest(entry);
  }
}

function formatSignedDeltaMs(deltaMs) {
  if (!Number.isFinite(deltaMs)) return 'Δ —';

  const sign = deltaMs > 0 ? '+' : deltaMs < 0 ? '−' : '±';
  const abs = Math.abs(deltaMs);

  if (abs >= 60_000) {
    return `Δ ${sign}${(abs / 60_000).toFixed(1)}m`;
  }
  if (abs >= 1_000) {
    return `Δ ${sign}${(abs / 1_000).toFixed(1)}s`;
  }
  return `Δ ${sign}${abs}ms`;
}

function rebuildTable() {
  const tbody = document.getElementById('request-table');
  tbody.innerHTML = '';
  const filtered = entries.filter(matchesFilter);
  anomalyFocusHasEmptyResults = Boolean(anomalyFocus && filtered.length === 0);
  filtered.slice(0, 200).forEach(e => addTableRow(e, false));
  document.getElementById('result-count').textContent = filtered.length + ' results';
  updateExpandAnomalyButton();
  if (filtered.length === 0) renderAnomalyFocusEmptyState();
}

function matchesFilter(e) {
  if (currentFilter === 'error' && !e.status?.type?.includes('Error') && e.status?.type !== 'Timeout' && e.status?.type !== 'ConnectionError') return false;
  if (currentFilter === '5xx' && e.status?.type !== 'ServerError') return false;
  if (currentFilter === '4xx' && e.status?.type !== 'ClientError') return false;
  if (currentFilter === 'timeout' && e.status?.type !== 'Timeout') return false;
  if (currentFilter === 'stall' && (!e.stalls || e.stalls.length === 0)) return false;
  if (currentSessionFilter !== 'all') {
    const sid = e.session_id || 'unknown';
    if (sid !== currentSessionFilter) return false;
  }

  if (searchQuery) {
    const q = searchQuery.toLowerCase();
    const hay = `${e.path} ${e.model} ${e.error || ''} ${e.status?.value || e.status?.type || ''} ${e.session_id || 'unknown'}`.toLowerCase();
    if (!hay.includes(q)) return false;
  }
  return true;
}

function updateExpandAnomalyButton() {
  const button = document.getElementById('expand-anomaly-window-btn');
  if (!button) return;

  const shouldShow = Boolean(anomalyFocus && anomalyFocus.expanded === false && anomalyFocusHasEmptyResults);
  button.hidden = !shouldShow;
  button.disabled = !shouldShow;
}

function renderAnomalyFocusEmptyState() {
  const tbody = document.getElementById('request-table');
  if (!tbody) return;

  if (!anomalyFocus) return;

  const windowLabel = anomalyFocus.expanded ? '±10m' : '±2m';
  const prompt = anomalyFocus.expanded
    ? 'No requests found in ±10m around this anomaly timestamp.'
    : 'No requests found in ±2m around this anomaly timestamp. Expand to ±10m to continue triage.';

  tbody.innerHTML = `
    <tr>
      <td colspan="10">
        <div class="empty-state" style="padding:24px 12px">
          <h3 style="font-size:14px">No anomaly-window matches (${windowLabel})</h3>
          <p>${esc(prompt)}</p>
        </div>
      </td>
    </tr>
  `;
}

function expandAnomalyWindow() {
  if (!anomalyFocus || anomalyFocus.expanded) return;
  anomalyFocus.windowMs = EXPANDED_ANOMALY_WINDOW_MS;
  anomalyFocus.expanded = true;
  updateExpandAnomalyButton();
  loadEntries();
}

function getStatusClass(status) {
  if (!status) return '';
  if (status.type === 'Success') return 'status-2xx';
  if (status.type === 'ClientError') return 'status-4xx';
  if (status.type === 'ServerError') return 'status-5xx';
  if (status.type === 'Timeout') return 'status-timeout';
  return 'status-timeout';
}

function updateSortArrows() {
  document.querySelectorAll('th.sortable').forEach(header => {
    const arrow = header.querySelector('.sort-arrow');
    if (!arrow) return;
    arrow.className = 'sort-arrow';
    if (header.dataset.sort === currentSortBy) {
      arrow.classList.add(currentSortOrder === 'asc' ? 'asc' : 'desc');
    }
  });
}

function openRequestModal(entry) {
  const root = document.getElementById('request-modal-root');
  if (!root) return;
  const statusText = entry.status?.value || entry.status?.type || '?';
  const statusClass = getStatusClass(entry.status);
  const totalTokens = (entry.input_tokens || 0) + (entry.output_tokens || 0);

  root.innerHTML = `
    <div class="modal-backdrop" id="request-modal-backdrop">
      <div class="modal-content" role="dialog" aria-modal="true" aria-label="Request details" id="request-modal-content">
        <button class="modal-close" type="button" id="request-modal-close" aria-label="Close">×</button>
        <div class="modal-header">
          <div class="modal-title">Request ${esc(entry.id)}</div>
          <div class="modal-subtitle">${esc(entry.method)} ${esc(entry.path)}</div>
        </div>
        <div class="modal-section">
          <div class="modal-section-title">Summary</div>
          <div class="modal-grid">
            <div class="modal-stat"><div class="modal-stat-label">Status</div><div class="modal-stat-value"><span class="status-badge ${statusClass}">${statusText}</span></div></div>
            <div class="modal-stat"><div class="modal-stat-label">Model</div><div class="modal-stat-value">${esc(entry.model || '—')}</div></div>
            <div class="modal-stat"><div class="modal-stat-label">TTFT</div><div class="modal-stat-value">${entry.ttft_ms ? (entry.ttft_ms / 1000).toFixed(2) + 's' : '—'}</div></div>
            <div class="modal-stat"><div class="modal-stat-label">Duration</div><div class="modal-stat-value">${(entry.duration_ms / 1000).toFixed(2)}s</div></div>
            <div class="modal-stat"><div class="modal-stat-label">Session</div><div class="modal-stat-value">${esc(entry.session_id || 'unknown')}</div></div>
            <div class="modal-stat"><div class="modal-stat-label">Tokens</div><div class="modal-stat-value">${fmt(totalTokens)}</div></div>
          </div>
        </div>
        <div class="modal-section">
          <div class="modal-section-title">Top Explanations</div>
          <div class="correlation-list" id="modal-explanations"><div style="color:var(--text-2);font-size:12px">Loading explanations…</div></div>
        </div>
        <div class="modal-section">
          <div class="modal-section-title">Bodies</div>
          <div id="modal-bodies"><div style="color:var(--text-2);font-size:12px">Loading request/response bodies…</div></div>
        </div>
      </div>
    </div>
  `;

  document.getElementById('request-modal-close')?.addEventListener('click', closeModal);
  document.getElementById('request-modal-backdrop')?.addEventListener('click', (ev) => {
    if (ev.target?.id === 'request-modal-backdrop') closeModal();
  });

  loadModalExplanations(entry.id);
  loadModalBodies(entry.id);
}

function closeModal() {
  const root = document.getElementById('request-modal-root');
  if (root) root.innerHTML = '';
}

async function loadModalExplanations(requestId) {
  const list = document.getElementById('modal-explanations');
  if (!list) return;
  try {
    const resp = await fetch(`/api/explanations?request_id=${encodeURIComponent(requestId)}&limit=5`);
    if (resp.status === 404) {
      list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No explanations for this request.</div>';
      return;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const rows = envelopeData(await resp.json());
    if (!Array.isArray(rows) || rows.length === 0) {
      list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No explanations for this request.</div>';
      return;
    }
    list.innerHTML = rows.map(row => {
      const confidence = Number.isFinite(row.confidence) ? (row.confidence * 100).toFixed(0) + '%' : '—';
      return `<div class="correlation-item"><div class="correlation-meta"><span>Rank: ${row.rank ?? '—'}</span><span>Confidence: ${confidence}</span><span>Kind: ${esc(row.anomaly_kind || 'unknown')}</span></div><div>${esc(row.summary || '')}</div></div>`;
    }).join('');
  } catch (e) {
    console.error('Failed to load modal explanations:', e);
    list.innerHTML = '<div style="color:var(--red);font-size:12px">Failed to load explanations.</div>';
  }
}

async function loadModalBodies(requestId) {
  const root = document.getElementById('modal-bodies');
  if (!root) return;
  try {
    const resp = await fetch(`/api/entry-body?request_id=${encodeURIComponent(requestId)}`);
    if (resp.status === 404) {
      root.innerHTML = '<div style="color:var(--text-2);font-size:12px">No request/response bodies saved for this entry.</div>';
      return;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const body = await resp.json();
    if (body?.error) {
      root.innerHTML = '<div style="color:var(--text-2);font-size:12px">No request/response bodies saved for this entry.</div>';
      return;
    }

    const requestBody = tryFormatJson(body.request_body || '');
    const rawResponse = body.response_body || '';
    const isSse = rawResponse.trimStart().startsWith('event:');
    const mergedResponse = isSse ? accumulateSseEvents(rawResponse) : null;

    let responseMode = 'raw'; // 'raw' | 'merged'

    function renderResponseSection() {
      const section = root.querySelector('#response-body-section');
      if (!section) return;
      if (responseMode === 'merged' && mergedResponse !== null) {
        section.querySelector('pre').textContent = JSON.stringify(mergedResponse, null, 2);
      } else {
        section.querySelector('pre').textContent = rawResponse || '—';
      }
    }

    const toggleBtn = isSse && mergedResponse
      ? `<button id="response-view-toggle" style="font-size:11px;padding:2px 8px;cursor:pointer;margin-left:8px">Show merged</button>`
      : '';

    root.innerHTML = `
      <details class="modal-collapsible" open>
        <summary>Request body ${body.truncated ? '(truncated)' : ''}</summary>
        <pre class="modal-body-content">${esc(requestBody || '—')}</pre>
      </details>
      <details class="modal-collapsible" style="margin-top:8px" open id="response-body-section">
        <summary>Response body ${body.truncated ? '(truncated)' : ''}${toggleBtn}</summary>
        <pre class="modal-body-content">${esc(rawResponse || '—')}</pre>
      </details>
    `;

    root.querySelector('#response-view-toggle')?.addEventListener('click', (ev) => {
      ev.stopPropagation();
      responseMode = responseMode === 'raw' ? 'merged' : 'raw';
      ev.target.textContent = responseMode === 'raw' ? 'Show merged' : 'Show raw SSE';
      renderResponseSection();
    });
  } catch (e) {
    console.error('Failed to load modal bodies:', e);
    root.innerHTML = '<div style="color:var(--red);font-size:12px">Failed to load request/response bodies.</div>';
  }
}

function accumulateSseEvents(sseText) {
  // Parse SSE lines into {event, data} pairs
  const events = [];
  let currentEvent = null;
  let currentData = [];
  for (const line of sseText.split('\n')) {
    const trimmed = line.trimEnd();
    if (trimmed.startsWith('event:')) {
      currentEvent = trimmed.slice(6).trim();
    } else if (trimmed.startsWith('data:')) {
      currentData.push(trimmed.slice(5).trim());
    } else if (trimmed === '') {
      if (currentEvent || currentData.length) {
        events.push({ event: currentEvent, data: currentData.join('\n') });
      }
      currentEvent = null;
      currentData = [];
    }
  }
  if (currentEvent || currentData.length) {
    events.push({ event: currentEvent, data: currentData.join('\n') });
  }

  let message = null;
  const inputBuffers = {}; // index -> partial JSON string for tool_use inputs

  for (const { event, data } of events) {
    let parsed;
    try { parsed = JSON.parse(data); } catch (_) { continue; }

    if (event === 'message_start' && parsed.message) {
      message = { ...parsed.message, content: [] };
    } else if (event === 'content_block_start' && message) {
      const block = { ...parsed.content_block };
      if (block.type === 'tool_use') {
        inputBuffers[parsed.index] = '';
        block.input = {};
      } else if (block.type === 'text') {
        block.text = '';
      } else if (block.type === 'thinking') {
        block.thinking = '';
      }
      message.content[parsed.index] = block;
    } else if (event === 'content_block_delta' && message) {
      const block = message.content[parsed.index];
      if (!block) continue;
      const delta = parsed.delta;
      if (delta.type === 'text_delta') {
        block.text = (block.text || '') + delta.text;
      } else if (delta.type === 'input_json_delta') {
        inputBuffers[parsed.index] = (inputBuffers[parsed.index] || '') + delta.partial_json;
      } else if (delta.type === 'thinking_delta') {
        block.thinking = (block.thinking || '') + delta.thinking;
      } else if (delta.type === 'signature_delta') {
        block.signature = delta.signature;
      }
    } else if (event === 'content_block_stop' && message) {
      const block = message.content[parsed.index];
      if (block?.type === 'tool_use' && inputBuffers[parsed.index] !== undefined) {
        try { block.input = JSON.parse(inputBuffers[parsed.index]); } catch (_) { block.input = inputBuffers[parsed.index]; }
        delete inputBuffers[parsed.index];
      }
    } else if (event === 'message_delta' && message && parsed.delta) {
      if (parsed.delta.stop_reason !== undefined) message.stop_reason = parsed.delta.stop_reason;
      if (parsed.delta.stop_sequence !== undefined) message.stop_sequence = parsed.delta.stop_sequence;
      if (parsed.usage) message.usage = { ...message.usage, ...parsed.usage };
    }
    // ping and message_stop are ignored
  }

  return message;
}

function tryFormatJson(text) {
  if (typeof text !== 'string') return '';
  const trimmed = text.trim();
  if (!trimmed) return '';
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch (_) {
    return text;
  }
}

function updateAnomalies(entry) {
  const list = document.getElementById('anomaly-list');
  if (list.querySelector('.empty-state')) list.innerHTML = '';

  for (const a of entry.anomalies) {
    const cls = a.severity === 'Critical' ? 'critical' : a.severity === 'Error' ? 'error' : 'warning';
    const time = new Date(entry.timestamp).toLocaleTimeString();
    const div = document.createElement('div');
    div.className = `anomaly-item ${cls}`;
    div.setAttribute('tabindex', '0');
    div.style.cursor = 'pointer';
    div.innerHTML = `<strong>${time}</strong> &nbsp; <span class="anomaly-badge sev-${cls}">${a.kind}</span> &nbsp; ${esc(a.message)}
      <span style="color:var(--text-2);margin-left:8px">${entry.model}</span>`;
    const open = () => {
      applyAnomalyFocus(entry, a);
    };
    div.addEventListener('click', open);
    div.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter') open();
    });
    list.insertBefore(div, list.firstChild);
    while (list.children.length > 100) list.removeChild(list.lastChild);
  }
}

async function applyAnomalyFocus(entry, anomaly) {
  if (!requestViewSnapshot) {
    requestViewSnapshot = snapshotRequestViewState();
  }

  const anomalyTsMs = Number.isFinite(anomaly?.timestamp_ms)
    ? anomaly.timestamp_ms
    : new Date(entry.timestamp).getTime();

  anomalyFocus = {
    id: `${entry.id}:${anomaly?.kind || 'anomaly'}:${anomalyTsMs}`,
    kind: anomaly?.kind || 'unknown',
    message: anomaly?.message || '',
    timestampMs: anomalyTsMs,
    windowMs: DEFAULT_ANOMALY_WINDOW_MS,
    expanded: false,
    preselectedRequestId: entry.id,
  };
  anomalyFocusHasEmptyResults = false;

  renderAnomalyFocusBar();
  updateExpandAnomalyButton();
  setActiveTab('requests');
  await loadEntries();
}

async function loadEntries() {
  const token = ++entriesRequestToken;

  try {
    const params = new URLSearchParams({ limit: '200' });

    if (anomalyFocus && Number.isFinite(anomalyFocus.timestampMs)) {
      params.set('anomaly_ts_ms', String(anomalyFocus.timestampMs));
      params.set('window_ms', String(anomalyFocus.windowMs));
      params.set('window_mode', 'centered');
    } else {
      if (currentFilter && currentFilter !== 'all') params.set('status', currentFilter);
      if (searchQuery && searchQuery.trim()) params.set('search', searchQuery.trim());
      if (currentSessionFilter && currentSessionFilter !== 'all') params.set('session_id', currentSessionFilter);
      params.set('sort_by', currentSortBy);
      params.set('sort_order', currentSortOrder);
    }

    const resp = await fetch(`/api/entries?${params.toString()}`);
    const data = await resp.json();
    if (token !== entriesRequestToken) return;

    entries = Array.isArray(data) ? data : (data.entries || []);

    if (anomalyFocus) {
      anomalyFocus.preselectedRequestId = Array.isArray(data)
        ? null
        : (data.preselected_request_id || null);
      renderAnomalyFocusBar();
    }

    rebuildTable();
    if (anomalyFocus && anomalyFocus.preselectedRequestId) {
      preselectRequestRow(anomalyFocus.preselectedRequestId);
    }
    updateSortArrows();
  } catch (e) {
    console.error('Failed to load entries:', e);
  }
}

async function populateSessionFilter() {
  const select = document.getElementById('session-filter');
  if (!select) return;

  const previous = currentSessionFilter;
  try {
    const resp = await fetch('/api/sessions/merged');
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const sessions = envelopeData(await resp.json());
    const options = ['<option value="all">All sessions</option>'];
    for (const s of sessions) {
      const sid = s.session_id || 'unknown';
      options.push(`<option value="${esc(sid)}">${esc(sid)} (${s.proxy_request_count || 0})</option>`);
    }
    select.innerHTML = options.join('');
    const exists = previous === 'all' || sessions.some(s => (s.session_id || 'unknown') === previous);
    currentSessionFilter = exists ? previous : 'all';
    select.value = currentSessionFilter;
  } catch (e) {
    console.error('Failed to load session filter options:', e);
  }
}

function filterSession(id) {
  setActiveTab('requests');
  const select = document.getElementById('session-filter');
  currentSessionFilter = id || 'all';
  if (select) select.value = currentSessionFilter;
  clearAnomalyFocus({ restoreSnapshot: false, reload: false });
  loadEntries();
}

async function loadExplanations(requestId) {
  const list = document.getElementById('request-explanations-list');
  list.innerHTML = '<div style="color:var(--text-2);font-size:12px">Loading explanations…</div>';

  try {
    const resp = await fetch(`/api/explanations?request_id=${encodeURIComponent(requestId)}&limit=5`);
    if (resp.status === 404) {
      list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No explanations for this request.</div>';
      return;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const rows = envelopeData(await resp.json());

    if (selectedRequestId !== requestId) return;

    if (!Array.isArray(rows) || rows.length === 0) {
      list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No explanations for this request.</div>';
      return;
    }

    list.innerHTML = '<div style="color:var(--text-2);font-size:11px;margin-bottom:6px">Confidence is heuristic (ranking signal, not probability).</div>' + rows.map(row => {
      const confidence = Number.isFinite(row.confidence) ? (row.confidence * 100).toFixed(0) + '%' : '—';
      return `
        <div class="correlation-item">
          <div class="correlation-meta">
            <span>Rank: ${row.rank ?? '—'}</span>
            <span>Confidence: ${confidence}</span>
            <span>Kind: ${esc(row.anomaly_kind || 'unknown')}</span>
          </div>
          <div>${esc(row.summary || '')}</div>
        </div>
      `;
    }).join('');
  } catch (e) {
    if (selectedRequestId !== requestId) return;
    console.error('Failed to load explanations:', e);
    list.innerHTML = '<div style="color:var(--red);font-size:12px">Failed to load explanations.</div>';
  }
}

async function loadSessionTimeline(sessionId, targetElementId = 'session-timeline-list', limit = 200) {
  const list = document.getElementById(targetElementId);
  if (!list) return [];
  list.innerHTML = '<div style="color:var(--text-2);font-size:12px">Loading timeline…</div>';

  try {
    const resp = await fetch(`/api/timeline?session_id=${encodeURIComponent(sessionId)}&limit=${limit}`);
    if (resp.status === 404) {
      list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No timeline items for this session.</div>';
      return [];
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const items = envelopeData(await resp.json());

    if (targetElementId === 'session-timeline-list' && selectedSessionId !== sessionId) return items;

    if (!Array.isArray(items) || items.length === 0) {
      list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No timeline items for this session.</div>';
      return [];
    }

    list.innerHTML = items.map(item => {
      const when = Number.isFinite(item.timestamp_ms) ? new Date(item.timestamp_ms).toLocaleTimeString() : '—';
      return `
        <div class="correlation-item">
          <div class="correlation-meta">
            <span>${when}</span>
            <span>${esc(item.kind || 'unknown')}</span>
          </div>
          <div>${esc(item.label || '')}</div>
        </div>
      `;
    }).join('');
    return items;
  } catch (e) {
    if (targetElementId === 'session-timeline-list' && selectedSessionId !== sessionId) return [];
    console.error('Failed to load session timeline:', e);
    list.innerHTML = '<div style="color:var(--red);font-size:12px">Failed to load timeline.</div>';
    return [];
  }
}

async function loadSessionGraph(sessionId, targetElementId, limit = 200) {
  const list = targetElementId ? document.getElementById(targetElementId) : null;
  if (list) list.innerHTML = '<div style="color:var(--text-2);font-size:12px">Loading session graph…</div>';

  try {
    const resp = await fetch(`/api/session-graph?session_id=${encodeURIComponent(sessionId)}&limit=${limit}`);
    if (resp.status === 404) {
      if (list) list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No graph data available.</div>';
      return null;
    }
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const graph = envelopeData(await resp.json());

    if (!graph || !Array.isArray(graph.nodes)) {
      if (list) list.innerHTML = '<div style="color:var(--text-2);font-size:12px">No graph data available.</div>';
      return null;
    }

    const requestCount = graph.nodes.filter(n => n.kind === 'request').length;
    const eventCount = graph.nodes.filter(n => n.kind === 'local_event').length;
    const explanationCount = graph.nodes.filter(n => n.kind === 'explanation').length;
    const edgeCount = Array.isArray(graph.edges) ? graph.edges.length : 0;

    if (list) {
      list.innerHTML = `
        <div class="correlation-item">
          <div class="correlation-meta">
            <span>Session: ${esc(graph.session_id || sessionId)}</span>
            <span>Requests: ${requestCount}</span>
            <span>Events: ${eventCount}</span>
            <span>Explanations: ${explanationCount}</span>
            <span>Edges: ${edgeCount}</span>
          </div>
          <div style="color:var(--text-2);font-size:12px">Summary view (node/edge counts).</div>
        </div>
      `;
    }

    return { graph, requestCount, eventCount, explanationCount, edgeCount };
  } catch (e) {
    console.error('Failed to load session graph:', e);
    if (list) list.innerHTML = '<div style="color:var(--red);font-size:12px">Failed to load session graph.</div>';
    return null;
  }
}
