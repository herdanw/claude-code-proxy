// app.js — Settings editor functions & DOMContentLoaded initialization

// ─── Settings state ───
let settingsMismatchActive = false;
let settingsDiskLoadedSnapshot = null;
let settingsApplyInFlight = false;
let settingsValidationErrorLine = null;
let settingsValidationMessage = '';
let settingsValidateTimer = null;

function isSettingsEditorDirty() {{
  const editor = document.getElementById('settings-claude-json');
  if (!editor) return false;
  const baseline = editor.dataset.lastLoadedSnapshot;
  if (baseline == null) return (editor.value || '').trim().length > 0;
  try {{
    return JSON.stringify(JSON.parse(editor.value || '{{}}')) !== baseline;
  }} catch (_) {{
    return true;
  }}
}}

function parseErrorLine(message, sourceText) {{
  if (!message) return null;
  const lineMatch = String(message).match(/line\s+(\d+)/i);
  if (lineMatch) return Number(lineMatch[1]);
  const posMatch = String(message).match(/position\s+(\d+)/i);
  if (!posMatch) return null;
  const pos = Number(posMatch[1]);
  if (!Number.isFinite(pos) || pos < 0) return null;
  const prefix = sourceText.slice(0, pos);
  return prefix.split('\n').length;
}}

function highlightJsonLine(line) {{
  const escaped = esc(line);
  const tokenRe = /"(?:\\.|[^"\\])*"(?=\s*:)|"(?:\\.|[^"\\])*"|\btrue\b|\bfalse\b|\bnull\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|[{{}}\[\]:,]/g;
  let last = 0;
  let out = '';
  let m;
  while ((m = tokenRe.exec(escaped)) !== null) {{
    out += escaped.slice(last, m.index);
    const token = m[0];
    let klass = 'json-punc';
    if (token.startsWith('"')) {{
      klass = /"\s*:$/.test(escaped.slice(m.index, m.index + token.length + 3)) ? 'json-key' : 'json-string';
    }} else if (token === 'true' || token === 'false') {{
      klass = 'json-boolean';
    }} else if (token === 'null') {{
      klass = 'json-null';
    }} else if (/^-?\d/.test(token)) {{
      klass = 'json-number';
    }}
    out += `<span class="${{klass}}">${{token}}</span>`;
    last = m.index + token.length;
  }}
  out += escaped.slice(last);
  return out;
}}

function renderSettingsEditor() {{
  const editor = document.getElementById('settings-claude-json');
  const highlight = document.getElementById('settings-highlight');
  const lineNumbers = document.getElementById('settings-line-numbers');
  const meta = document.getElementById('settings-editor-meta');
  const dirty = document.getElementById('settings-dirty-indicator');
  if (!editor || !highlight || !lineNumbers) return;

  const text = editor.value || '';
  const lines = text.split('\n');

  highlight.innerHTML = lines.map((line, i) => {{
    const lineNum = i + 1;
    const lineClass = lineNum === settingsValidationErrorLine ? 'settings-code-line settings-error-line' : 'settings-code-line';
    return `<div class="${{lineClass}}">${{highlightJsonLine(line) || '&nbsp;'}}</div>`;
  }}).join('');

  lineNumbers.innerHTML = lines.map((_, i) => {{
    const lineNum = i + 1;
    const cls = lineNum === settingsValidationErrorLine ? 'settings-line-num settings-line-num-error' : 'settings-line-num';
    return `<div class="${{cls}}">${{lineNum}}</div>`;
  }}).join('');

  if (meta) {{
    const bytes = new TextEncoder().encode(text).length;
    const cursor = editor.selectionStart ?? 0;
    const before = text.slice(0, cursor);
    const row = before.split('\n').length;
    const col = before.length - before.lastIndexOf('\n');
    meta.textContent = `${{lines.length}} lines • ${{bytes}} bytes • Ln ${{row}}, Col ${{col}}`;
  }}

  if (dirty) dirty.style.display = isSettingsEditorDirty() ? 'inline' : 'none';
}}

function syncSettingsEditorScroll() {{
  const editor = document.getElementById('settings-claude-json');
  const highlight = document.getElementById('settings-highlight');
  const lineNumbers = document.getElementById('settings-line-numbers');
  if (!editor || !highlight || !lineNumbers) return;
  highlight.scrollTop = editor.scrollTop;
  highlight.scrollLeft = editor.scrollLeft;
  lineNumbers.scrollTop = editor.scrollTop;
}}

function renderSettingsErrorBanner() {{
  const banner = document.getElementById('settings-error-banner');
  const text = document.getElementById('settings-error-text');
  const applyBtn = document.getElementById('settings-apply-btn');
  if (!banner || !text || !applyBtn) return;
  if (settingsValidationErrorLine != null) {{
    banner.style.display = 'block';
    text.textContent = `Line ${{settingsValidationErrorLine}}: ${{settingsValidationMessage}}`;
    applyBtn.disabled = true;
    applyBtn.style.opacity = '0.45';
    applyBtn.style.cursor = 'not-allowed';
  }} else {{
    banner.style.display = 'none';
    text.textContent = '';
    applyBtn.disabled = settingsApplyInFlight;
    applyBtn.style.opacity = '1';
    applyBtn.style.cursor = 'pointer';
  }}
}}

function validateSettingsEditorNow() {{
  const editor = document.getElementById('settings-claude-json');
  const status = document.getElementById('settings-apply-status');
  if (!editor) return;

  const text = editor.value || '{{}}';
  try {{
    const parsed = JSON.parse(text);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {{
      settingsValidationErrorLine = 1;
      settingsValidationMessage = 'JSON must be an object';
    }} else {{
      settingsValidationErrorLine = null;
      settingsValidationMessage = '';
      if (status && !settingsApplyInFlight) {{
        status.textContent = isSettingsEditorDirty() ? 'Unsaved changes' : 'Loaded current settings';
        status.style.color = 'var(--text-2)';
      }}
    }}
  }} catch (err) {{
    settingsValidationErrorLine = parseErrorLine(err?.message || 'Invalid JSON', text) || 1;
    settingsValidationMessage = err?.message || 'Invalid JSON';
  }}

  renderSettingsEditor();
  renderSettingsErrorBanner();
}}

function scheduleSettingsValidation() {{
  if (settingsValidateTimer) clearTimeout(settingsValidateTimer);
  settingsValidateTimer = setTimeout(() => {{
    validateSettingsEditorNow();
  }}, 300);
}}

function jumpToSettingsErrorLine() {{
  const editor = document.getElementById('settings-claude-json');
  if (!editor || settingsValidationErrorLine == null) return;
  const targetLine = Math.max(1, settingsValidationErrorLine);
  const lines = (editor.value || '').split('\n');
  let idx = 0;
  for (let i = 0; i < targetLine - 1 && i < lines.length; i++) idx += lines[i].length + 1;
  editor.focus();
  editor.selectionStart = idx;
  editor.selectionEnd = idx;
  editor.scrollTop = Math.max(0, (targetLine - 4) * 20);
  syncSettingsEditorScroll();
}}

async function loadSettingsCurrent() {{
  const editor = document.getElementById('settings-claude-json');
  const status = document.getElementById('settings-apply-status');
  if (!editor) return;

  try {{
    const resp = await fetch('/api/settings/current');
    if (!resp.ok) throw new Error(`HTTP ${{resp.status}}`);
    const payload = await resp.json();

    const raw = payload?.data?.claude_settings?.raw_json;
    const normalized = raw && typeof raw === 'object' && !Array.isArray(raw) ? raw : {{}};

    settingsDiskLoadedSnapshot = normalized;
    settingsMismatchActive = false;

    const normalizedText = JSON.stringify(normalized, null, 2);
    const normalizedBaseline = JSON.stringify(normalized);

    if (editor.dataset.lastLoadedSnapshot == null || !isSettingsEditorDirty()) {{
      editor.value = normalizedText;
      editor.dataset.lastLoadedSnapshot = normalizedBaseline;
    }}

    if (status) {{
      status.textContent = 'Loaded current settings';
      status.style.color = 'var(--text-2)';
    }}

    settingsValidationErrorLine = null;
    settingsValidationMessage = '';
    renderSettingsEditor();
    renderSettingsErrorBanner();
    syncSettingsEditorScroll();
  }} catch (e) {{
    console.error('Failed to load current settings:', e);
    if (status) {{
      status.textContent = 'Failed to load current settings';
      status.style.color = 'var(--red)';
    }}
    settingsMismatchActive = false;
  }}
}}

async function applySettingsPayload(parsed, {{ successMessage = 'Settings applied' }} = {{}}) {{
  const editor = document.getElementById('settings-claude-json');
  const status = document.getElementById('settings-apply-status');
  const applyBtn = document.getElementById('settings-apply-btn');
  if (!editor || !applyBtn) return false;
  if (settingsApplyInFlight) return false;

  settingsApplyInFlight = true;
  const original = applyBtn.textContent;
  applyBtn.disabled = true;
  applyBtn.textContent = 'Applying…';

  try {{
    const resp = await fetch('/api/settings/apply', {{
      method: 'POST',
      headers: {{ 'content-type': 'application/json' }},
      body: JSON.stringify({{ settings: parsed }})
    }});

    const body = await resp.json().catch(() => null);
    if (!resp.ok) {{
      const errorNode = body?.error && typeof body.error === 'object' ? body.error : null;
      const message = errorNode?.message || body?.message || body?.error_message || `HTTP ${{resp.status}}`;
      if (status) {{
        status.textContent = `Failed to apply settings: ${{message}}`;
        status.style.color = 'var(--red)';
      }}
      return false;
    }}

    editor.dataset.lastLoadedSnapshot = JSON.stringify(parsed);
    settingsMismatchActive = false;
    settingsValidationErrorLine = null;
    settingsValidationMessage = '';
    if (status) {{
      status.textContent = successMessage;
      status.style.color = 'var(--green)';
    }}

    renderSettingsEditor();
    renderSettingsErrorBanner();
    setTimeout(() => {{
      if (status && status.textContent === successMessage) {{
        status.textContent = 'Loaded current settings';
        status.style.color = 'var(--text-2)';
      }}
    }}, 3000);
    return true;
  }} catch (e) {{
    console.error('Failed to apply settings:', e);
    if (status) {{
      status.textContent = 'Failed to apply settings';
      status.style.color = 'var(--red)';
    }}
    return false;
  }} finally {{
    settingsApplyInFlight = false;
    applyBtn.disabled = false;
    applyBtn.textContent = original;
    renderSettingsErrorBanner();
  }}
}}

async function applySettings() {{
  const editor = document.getElementById('settings-claude-json');
  const status = document.getElementById('settings-apply-status');
  if (!editor) return;

  let parsed;
  try {{
    parsed = JSON.parse(editor.value || '{{}}');
  }} catch (_) {{
    if (status) {{
      status.textContent = 'Invalid JSON: fix syntax before apply';
      status.style.color = 'var(--red)';
    }}
    return;
  }}

  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {{
    if (status) {{
      status.textContent = 'JSON must be an object';
      status.style.color = 'var(--red)';
    }}
    return;
  }}

  await applySettingsPayload(parsed, {{ successMessage: 'Settings applied' }});
}}

function formatSettingsEditor() {{
  const editor = document.getElementById('settings-claude-json');
  const status = document.getElementById('settings-apply-status');
  if (!editor) return;
  try {{
    const parsed = JSON.parse(editor.value || '{{}}');
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {{
      throw new Error('JSON must be an object');
    }}
    editor.value = JSON.stringify(parsed, null, 2);
    validateSettingsEditorNow();
    if (status) {{
      status.textContent = 'Formatted JSON';
      status.style.color = 'var(--text-2)';
    }}
  }} catch (err) {{
    settingsValidationErrorLine = parseErrorLine(err?.message || 'Invalid JSON', editor.value || '') || 1;
    settingsValidationMessage = err?.message || 'Invalid JSON';
    renderSettingsEditor();
    renderSettingsErrorBanner();
    if (status) {{
      status.textContent = 'Cannot format invalid JSON';
      status.style.color = 'var(--red)';
    }}
  }}
}}

async function resetSettingsEditor() {{
  if (isSettingsEditorDirty() && !window.confirm('Discard unsaved settings changes and reload from disk?')) {{
    return;
  }}
  await loadSettingsCurrent();
}}

// ─── DOMContentLoaded ───
document.addEventListener('DOMContentLoaded', () => {{
  initCharts();
  connectWS();
  resetOverview();
  setSearchQuery(loadPersistedSearchQuery(), {{ persist: false, reload: false }});
  setRequestFilter(loadPersistedRequestFilter(), {{ persist: false, reload: false }});
  setOverviewMode(loadPersistedOverviewMode());
  setActiveTab(loadPersistedTab(), {{ persist: false }});

  document.querySelectorAll('th.sortable').forEach(header => {{
    header.addEventListener('click', () => {{
      const sortBy = header.dataset.sort || 'timestamp_ms';
      if (currentSortBy === sortBy) {{
        currentSortOrder = currentSortOrder === 'asc' ? 'desc' : 'asc';
      }} else {{
        currentSortBy = sortBy;
        currentSortOrder = 'desc';
      }}
      updateSortArrows();
      loadEntries();
    }});
  }});

  const sessionFilter = document.getElementById('session-filter');
  sessionFilter?.addEventListener('change', (e) => {{
    currentSessionFilter = e.target.value || 'all';
    clearAnomalyFocus({{ restoreSnapshot: false, reload: false }});
    loadEntries();
  }});

  document.addEventListener('keydown', (ev) => {{
    if (ev.key === 'Escape') {{
      const backdrop = document.getElementById('request-modal-backdrop');
      if (backdrop) closeModal();
      return;
    }}

    const active = document.activeElement;
    if (!active || !(active instanceof HTMLElement) || !active.classList.contains('request-row-clickable')) return;

    if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {{
      ev.preventDefault();
      const rows = Array.from(document.querySelectorAll('#request-table tr.request-row-clickable'));
      const idx = rows.indexOf(active);
      if (idx < 0) return;
      const nextIdx = ev.key === 'ArrowDown' ? Math.min(rows.length - 1, idx + 1) : Math.max(0, idx - 1);
      const target = rows[nextIdx];
      if (target && target.focus) target.focus();
    }} else if (ev.key === 'Enter') {{
      ev.preventDefault();
      active.click();
    }}
  }});

  // Load initial entries
  populateSessionFilter();
  loadEntries();

  // Tabs
  document.querySelectorAll('.tab').forEach(tab => {{
    tab.addEventListener('click', () => {{
      setActiveTab(tab.dataset.tab);
    }});
  }});

  // Filters
  document.querySelectorAll('[data-filter]').forEach(btn => {{
    btn.addEventListener('click', () => {{
      setRequestFilter(btn.dataset.filter);
    }});
  }});

  document.getElementById('overview-live-btn').addEventListener('click', () => setOverviewMode('live'));
  document.getElementById('overview-historical-btn').addEventListener('click', () => setOverviewMode('historical'));

  // Search
  let searchTimeout;
  document.getElementById('search-input').addEventListener('input', (e) => {{
    clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {{
      setSearchQuery(e.target.value);
    }}, 200);
  }});

  document.getElementById('expand-anomaly-window-btn').addEventListener('click', expandAnomalyWindow);
  document.getElementById('clear-anomaly-focus-btn').addEventListener('click', () => clearAnomalyFocus({{ restoreSnapshot: true, reload: true }}));
  document.getElementById('clear-memory-btn').addEventListener('click', clearMemoryStats);
  document.getElementById('clear-data-btn').addEventListener('click', clearAllData);
  const settingsEditor = document.getElementById('settings-claude-json');
  const settingsErrorBanner = document.getElementById('settings-error-banner');
  document.getElementById('settings-apply-btn')?.addEventListener('click', applySettings);
  document.getElementById('settings-format-btn')?.addEventListener('click', formatSettingsEditor);
  document.getElementById('settings-reset-btn')?.addEventListener('click', resetSettingsEditor);
  settingsEditor?.addEventListener('input', () => {{
    renderSettingsEditor();
    scheduleSettingsValidation();
  }});
  settingsEditor?.addEventListener('scroll', syncSettingsEditorScroll);
  settingsEditor?.addEventListener('click', renderSettingsEditor);
  settingsEditor?.addEventListener('keyup', renderSettingsEditor);
  settingsEditor?.addEventListener('keydown', (event) => {{
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {{
      event.preventDefault();
      applySettings();
      return;
    }}
    if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === 'f') {{
      event.preventDefault();
      formatSettingsEditor();
    }}
  }});
  settingsErrorBanner?.addEventListener('click', jumpToSettingsErrorLine);
  window.addEventListener('resize', resizeCharts);
  updateSortArrows();
}});
