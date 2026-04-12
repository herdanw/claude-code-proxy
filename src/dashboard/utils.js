// utils.js — shared helpers

const TAB_STORAGE_KEY = 'claude-proxy.active-tab';
const REQUEST_FILTER_STORAGE_KEY = 'claude-proxy.request-filter';
const SEARCH_QUERY_STORAGE_KEY = 'claude-proxy.search-query';
const OVERVIEW_MODE_STORAGE_KEY = 'claude-proxy.overview-mode';
const MAX_TABLE_ENTRIES = 500;

function fmt(n) {{
  if (!n) return '0';
  if (n > 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n > 1e3) return (n / 1e3).toFixed(1) + 'K';
  return n.toString();
}}

function esc(s) {{
  if (s == null) return '';
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}}

function envelopeData(payload) {{
  if (payload && typeof payload === 'object' && payload.data !== undefined) {{
    return payload.data;
  }}
  return payload;
}}

function formatDuration(s) {{
  const h = Math.floor(s / 3600); const m = Math.floor((s % 3600) / 60); const sec = Math.floor(s % 60);
  return `${{h}}:${{m.toString().padStart(2, '0')}}:${{sec.toString().padStart(2, '0')}}`;
}}

function tryFormatJson(text) {{
  if (typeof text !== 'string') return '';
  const trimmed = text.trim();
  if (!trimmed) return '';
  try {{
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  }} catch (_) {{
    return text;
  }}
}}

function getStatusClass(status) {{
  if (!status) return '';
  if (status.type === 'Success') return 'status-2xx';
  if (status.type === 'ClientError') return 'status-4xx';
  if (status.type === 'ServerError') return 'status-5xx';
  if (status.type === 'Timeout') return 'status-timeout';
  return 'status-timeout';
}}

function formatSignedDeltaMs(deltaMs) {{
  if (!Number.isFinite(deltaMs)) return 'Δ —';

  const sign = deltaMs > 0 ? '+' : deltaMs < 0 ? '−' : '±';
  const abs = Math.abs(deltaMs);

  if (abs >= 60_000) {{
    return `Δ ${{sign}}${{(abs / 60_000).toFixed(1)}}m`;
  }}
  if (abs >= 1_000) {{
    return `Δ ${{sign}}${{(abs / 1_000).toFixed(1)}}s`;
  }}
  return `Δ ${{sign}}${{abs}}ms`;
}}

function formatCoverageLabel(mode, start, end) {{
  const prefix = mode === 'historical' ? 'Historical' : 'Live';
  if (!start && !end) return `${{prefix}} overview`;

  if (start && end) {{
    return `${{prefix}} · ${{start.toLocaleString()}} → ${{end.toLocaleString()}}`;
  }}

  const point = start || end;
  return `${{prefix}} · ${{point.toLocaleString()}}`;
}}

function normalizeOverviewMode(mode) {{
  return mode === 'historical' ? 'historical' : 'live';
}}

function normalizeTab(tab) {{
  return ['overview', 'requests', 'conformance', 'anomalies', 'sessions'].includes(tab) ? tab : 'overview';
}}

function normalizeRequestFilter(filter) {{
  return ['all', 'error', '5xx', '4xx', 'timeout', 'stall'].includes(filter) ? filter : 'all';
}}

function normalizeSearchQuery(value) {{
  return typeof value === 'string' ? value.trim().slice(0, 200) : '';
}}

function loadStoredValue(key, normalize, fallback) {{
  try {{
    const value = window.localStorage.getItem(key);
    return value == null ? fallback : normalize(value);
  }} catch (_) {{
    return fallback;
  }}
}}

function persistStoredValue(key, value) {{
  try {{
    window.localStorage.setItem(key, value);
  }} catch (_) {{
    // Ignore storage failures and keep the in-memory preference.
  }}
}}

function loadPersistedTab() {{
  return loadStoredValue(TAB_STORAGE_KEY, normalizeTab, 'overview');
}}

function persistTab(tab) {{
  persistStoredValue(TAB_STORAGE_KEY, normalizeTab(tab));
}}

function loadPersistedRequestFilter() {{
  return loadStoredValue(REQUEST_FILTER_STORAGE_KEY, normalizeRequestFilter, 'all');
}}

function persistRequestFilter(filter) {{
  persistStoredValue(REQUEST_FILTER_STORAGE_KEY, normalizeRequestFilter(filter));
}}

function loadPersistedSearchQuery() {{
  return loadStoredValue(SEARCH_QUERY_STORAGE_KEY, normalizeSearchQuery, '');
}}

function persistSearchQuery(value) {{
  persistStoredValue(SEARCH_QUERY_STORAGE_KEY, normalizeSearchQuery(value));
}}

function loadPersistedOverviewMode() {{
  return loadStoredValue(OVERVIEW_MODE_STORAGE_KEY, normalizeOverviewMode, 'live');
}}

function persistOverviewMode(mode) {{
  persistStoredValue(OVERVIEW_MODE_STORAGE_KEY, normalizeOverviewMode(mode));
}}
